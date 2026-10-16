//! MCP server for `codemap` (stdio transport) and the tool implementations it
//! shares with `codemap query`.

pub mod api;
pub mod browser;
pub mod compose;
pub mod copy_rules;
pub mod i18n;
pub mod map;
pub mod misses;
pub mod refresh;
pub mod release;
pub mod server;
pub mod tools;
pub mod usages;
pub mod verify;
pub mod workspace;

pub use refresh::{RefreshReport, refresh};
pub use server::{CodemapServer, serve};
pub use tools::{Freshness, Project, TOOL_NAMES, ToolError, call};
