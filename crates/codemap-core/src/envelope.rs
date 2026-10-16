//! The response envelope every tool returns (brief §8): `data` plus freshness `meta`.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::store::{Store, meta_keys};

#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
}

/// Fields at their default (`stale: false`, `truncated: false`, no cursor, no
/// warnings, nothing re-indexed) are left out: they cost tokens on every call.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Meta {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "not_true")]
    pub git_dirty: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub reindexed_files: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// This answer's id, for `report_wrong_answer` (MCP server only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

fn not_true(v: &Option<bool>) -> bool {
    *v != Some(true)
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
