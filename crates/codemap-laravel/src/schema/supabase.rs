//! Supabase (and other plain-SQL) projects: the schema is replayed from
//! `supabase/migrations/*.sql` in file order, with the same SQL reader Laravel
//! raw statements use. Only for projects that are not Laravel apps; rebuilt
//! when those files change.

use std::path::Path;

use codemap_core::{Result, Store};
use rusqlite::TransactionBehavior;

use super::Schema;

const META_KEY: &str = "supabase_schema";
const VERSION: u32 = 1;

/// Migration files in replay order, with a fingerprint of their names, sizes
/// and modification times.
fn files(root: &Path) -> (Vec<String>, String) {
    let dir = root.join("supabase/migrations");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".sql").then_some(n)
        })
        .collect();
    names.sort();
    let mut fp = format!("v{VERSION}");
    for n in &names {
        if let Ok(m) = std::fs::metadata(dir.join(n)) {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            fp.push_str(&format!("|{n}:{}:{t}", m.len()));
        }
    }
    (names, fp)
}

/// Rebuilds the schema from the SQL migrations when they changed. Returns the
/// number of tables, or `None` when there is nothing to do.
pub fn refresh(store: &mut Store, root: &Path) -> Result<Option<usize>> {
    let (names, fp) = files(root);
    if names.is_empty() {
        return Ok(None);
    }
    if store.get_meta(META_KEY)?.as_deref() == Some(fp.as_str()) {
        return Ok(None);
    }
    let mut schema = Schema::default();
    for n in &names {
        let rel = format!("supabase/migrations/{n}");
        let Ok(sql) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        super::raw::apply(&mut schema, &sql, &format!("{rel}:1"), false);
        schema.replayed.push(rel);
    }
    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    super::store(&tx, &schema)?;
    codemap_core::store::set_meta(&tx, META_KEY, &fp)?;
    tx.commit()?;
    Ok(Some(schema.tables.iter().filter(|t| !t.dropped).count()))
}
