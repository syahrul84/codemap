//! The one refresh path shared by the CLI and the MCP server: scan → PHP →
//! Laravel → Vue/Nuxt. Each adapter skips work when nothing relevant changed.

use std::path::Path;

use pidx_core::{Config, Result, ScanOptions, ScanReport, Store};
use pidx_laravel::LaravelReport;
use pidx_php::PhpReport;
use pidx_vue::VueReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub scan: ScanReport,
    pub php: PhpReport,
    pub laravel: LaravelReport,
    pub vue: VueReport,
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
    let base = pidx_laravel::refresh(store, root, config, opts)?;
    let vue = pidx_vue::refresh(store, root, &base.scan, opts.full)?;
    Ok(RefreshReport {
        scan: base.scan,
        php: base.php,
        laravel: base.laravel,
        vue,
    })
}
