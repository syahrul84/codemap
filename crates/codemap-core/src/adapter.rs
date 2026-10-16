//! Framework adapters add framework knowledge on top of the generic index
//! (brief §4). Laravel is the first; Vue/Nuxt follow in Milestone 2.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Config, Result, ScanReport, Store};

pub struct ExtractContext<'a> {
    pub root: &'a Path,
    pub config: &'a Config,
    /// What changed in this refresh; adapters decide whether to re-extract.
    pub scan: &'a ScanReport,
    pub full: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractReport {
    /// False when nothing relevant changed and stored facts were kept.
    pub ran: bool,
    pub items: usize,
    pub unresolved: usize,
    pub warnings: usize,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub trait FrameworkAdapter {
    type Detection;

    /// e.g. `laravel`.
    fn id(&self) -> &'static str;

    /// Detection with the reasons behind it; `None` when the framework is absent.
    fn detect(&self, root: &Path, store: &Store) -> Result<Option<Self::Detection>>;

    fn extract(
        &self,
        ctx: &ExtractContext<'_>,
        detection: &Self::Detection,
        store: &mut Store,
    ) -> Result<ExtractReport>;
}
