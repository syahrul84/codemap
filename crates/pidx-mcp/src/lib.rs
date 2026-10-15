//! MCP server for `pidx` (stdio transport) and the tool implementations it
//! shares with `pidx query`.

pub mod api;
pub mod i18n;
pub mod refresh;
pub mod server;
pub mod tools;
pub mod usages;
pub mod verify;
pub mod workspace;

pub use refresh::{RefreshReport, refresh};
pub use server::{PidxServer, serve};
pub use tools::{Freshness, Project, TOOL_NAMES, ToolError, call};
