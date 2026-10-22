//! Exact references from an `index.scip` file, when the project has one
//! (rust-analyzer, scip-typescript, scip-php, scip-java, scip-python… write
//! them). codemap's own references are matched by name; these come from a
//! compiler or type checker, so "possible" uses can be settled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use protobuf::Message as _;

/// File name looked for at the project root.
pub const FILE: &str = "index.scip";

/// One occurrence: file, 1-based line, whether it defines the symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub path: String,
    pub line: u32,
    pub definition: bool,
}

/// An `index.scip`, read once and kept until the file changes.
#[derive(Debug)]
pub struct Index {
    pub path: PathBuf,
    pub modified: SystemTime,
    /// symbol → its occurrences.
    by_symbol: HashMap<String, Vec<Occurrence>>,
    /// file → (line, symbol) of its definitions.
    definitions: HashMap<String, Vec<(u32, String)>>,
}

static CACHE: Mutex<Option<Arc<Index>>> = Mutex::new(None);

/// The project's `index.scip`, or `None` when there is none (or it cannot be read).
pub fn load(root: &Path) -> Option<Arc<Index>> {
    let path = root.join(FILE);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = cache.as_ref()
        && i.path == path
        && i.modified == modified
    {
        return Some(i.clone());
    }
    let bytes = std::fs::read(&path).ok()?;
    let index = match scip::types::Index::parse_from_bytes(&bytes) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "could not read index.scip");
            return None;
        }
    };
    let mut by_symbol: HashMap<String, Vec<Occurrence>> = HashMap::new();
    let mut definitions: HashMap<String, Vec<(u32, String)>> = HashMap::new();
    for doc in &index.documents {
        let file = doc.relative_path.replace('\\', "/");
        for o in &doc.occurrences {
            // Local symbols (`local 3`) are only meaningful inside one file.
            if o.symbol.is_empty() || o.symbol.starts_with("local ") {
                continue;
            }
            let line = o.range.first().copied().unwrap_or(0).max(0) as u32 + 1;
            let definition = o.symbol_roles & scip::types::SymbolRole::Definition as i32 != 0;
            if definition {
                definitions
                    .entry(file.clone())
                    .or_default()
                    .push((line, o.symbol.clone()));
            }
            by_symbol
                .entry(o.symbol.clone())
                .or_default()
                .push(Occurrence {
                    path: file.clone(),
                    line,
                    definition,
                });
        }
    }
    let loaded = Arc::new(Index {
        path,
        modified,
        by_symbol,
        definitions,
    });
    *cache = Some(loaded.clone());
    Some(loaded)
}

/// The SCIP symbol for `name` defined in `file` between `from` and `to`
/// (1-based lines): the definition whose symbol ends with the name.
pub fn symbol_at(index: &Index, file: &str, from: u32, to: u32, name: &str) -> Option<String> {
    let defs = index.definitions.get(file)?;
    let in_range = |l: u32| l >= from && l <= to.max(from);
    let named = |s: &str| {
        // `rust-analyzer cargo demo 0.1.0 store/Store#add().` → `add`
        let tail = s.trim_end_matches(['.', '#', '/', ':', '(', ')']);
        let tail = tail.trim_end_matches("()");
        tail.rsplit(['/', '#', '.', ':', ' ', '`']).next() == Some(name)
            || tail.ends_with(&format!("`{name}`"))
    };
    defs.iter()
        .filter(|(l, s)| in_range(*l) && named(s))
        .min_by_key(|(l, _)| l.abs_diff(from))
        .map(|(_, s)| s.clone())
}

/// Every reference (not the definitions) of `symbol`.
pub fn references(index: &Index, symbol: &str) -> Vec<Occurrence> {
    index
        .by_symbol
        .get(symbol)
        .into_iter()
        .flatten()
        .filter(|o| !o.definition)
        .cloned()
        .collect()
}

/// Files changed after the index was written (of those given).
pub fn newer_than_index(index: &Index, root: &Path, files: &[&str]) -> usize {
    files
        .iter()
        .filter(|f| {
            std::fs::metadata(root.join(f))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > index.modified)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_names_are_matched_by_their_last_part() {
        let index = Index {
            path: PathBuf::new(),
            modified: SystemTime::UNIX_EPOCH,
            by_symbol: HashMap::new(),
            definitions: HashMap::from([(
                "src/store.rs".to_owned(),
                vec![
                    (5, "rust-analyzer cargo demo 0.1.0 store/Store#".to_owned()),
                    (
                        9,
                        "rust-analyzer cargo demo 0.1.0 store/Store#add().".to_owned(),
                    ),
                    (
                        12,
                        "scip-php composer acme 1.0 App/Models/Order#`refresh`().".to_owned(),
                    ),
                ],
            )]),
        };
        assert_eq!(
            symbol_at(&index, "src/store.rs", 9, 11, "add").as_deref(),
            Some("rust-analyzer cargo demo 0.1.0 store/Store#add().")
        );
        assert_eq!(
            symbol_at(&index, "src/store.rs", 5, 20, "Store").as_deref(),
            Some("rust-analyzer cargo demo 0.1.0 store/Store#")
        );
        assert!(symbol_at(&index, "src/store.rs", 12, 12, "refresh").is_some());
        assert_eq!(symbol_at(&index, "src/store.rs", 9, 11, "remove"), None);
    }
}
