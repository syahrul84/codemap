//! The one refresh path shared by the CLI and the MCP server: scan → PHP →
//! Laravel → Vue/Nuxt → Swift →
//! Android → Rust. Each adapter skips work when nothing relevant changed.

use std::path::Path;

use codemap_android::AndroidReport;
use codemap_core::{Config, Result, ScanOptions, ScanReport, Store};
use codemap_laravel::LaravelReport;
use codemap_php::PhpReport;
use codemap_rust::RustReport;
use codemap_swift::SwiftReport;
use codemap_vue::VueReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub scan: ScanReport,
    pub php: PhpReport,
    pub laravel: LaravelReport,
    pub vue: VueReport,
    #[serde(default)]
    pub swift: SwiftReport,
    #[serde(default)]
    pub android: AndroidReport,
    #[serde(default)]
    pub rust: RustReport,
}

impl RefreshReport {
    /// Files whose facts changed in this refresh.
    pub fn reindexed_files(&self) -> usize {
        self.scan.reparsed + self.scan.removed
    }
}

pub fn refresh(
    store: &mut Store,
    root: &Path,
    config: &Config,
    opts: ScanOptions,
) -> Result<RefreshReport> {
    let base = codemap_laravel::refresh(store, root, config, opts)?;
    let vue = codemap_vue::refresh(store, root, &base.scan, opts.full)?;
    let swift = codemap_swift::refresh(store, root, &base.scan, opts.full)?;
    let android = codemap_android::refresh(store, root, &base.scan, opts.full)?;
    let rust = codemap_rust::refresh(store, root, &base.scan, opts.full)?;
    // Supabase / plain SQL migrations give the schema of non-Laravel apps.
    if !root.join("artisan").is_file()
        && let Some(n) = codemap_laravel::schema::supabase::refresh(store, root)?
    {
        tracing::info!(tables = n, "schema replayed from supabase/migrations");
    }
    Ok(RefreshReport {
        scan: base.scan,
        php: base.php,
        laravel: base.laravel,
        vue,
        swift,
        android,
        rust,
    })
}
