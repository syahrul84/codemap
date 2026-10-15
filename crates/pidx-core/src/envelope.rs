//! The response envelope every tool returns (brief §8): `data` plus freshness `meta`.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::store::{Store, meta_keys};

#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Meta {
    pub project: String,
    pub indexed_at: Option<String>,
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    pub stale: bool,
    pub reindexed_files: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub warnings: Vec<String>,
}

/// How much each item says. `brief` is the default everywhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    #[default]
    Brief,
    Full,
}

/// Short commit ids keep responses small.
const COMMIT_CHARS: usize = 12;

impl Meta {
    /// Freshness fields from the index's meta table.
    pub fn from_store(store: &Store, project: &str, reindexed_files: usize) -> Result<Self> {
        Ok(Self {
            project: project.to_owned(),
            indexed_at: store.get_meta(meta_keys::INDEXED_AT)?,
            git_commit: store
                .get_meta(meta_keys::GIT_COMMIT)?
                .map(|c| c.chars().take(COMMIT_CHARS).collect()),
            git_dirty: store.get_meta(meta_keys::GIT_DIRTY)?.map(|v| v == "true"),
            reindexed_files,
            ..Self::default()
        })
    }
}

impl<T> Envelope<T> {
    pub fn new(data: T, meta: Meta) -> Self {
        Self { data, meta }
    }
}
