//! Core of `codemap`: index model, SQLite store, scanner, freshness checks and
//! the response envelope shared by the CLI and the MCP server.

pub mod adapter;
pub mod config;
pub mod envelope;
pub mod error;
pub mod git;
pub mod logging;
pub mod paths;
pub mod scan;
pub mod store;
pub mod symbols;
pub mod time;
pub mod updates;

pub use config::Config;
pub use error::{Error, Result};
pub use scan::{ScanOptions, ScanReport};
pub use store::Store;

/// Version of the `codemap` workspace.
/// `0.1.2` for a release build, `0.1.2-dev+abc1234` otherwise (see build.rs).
pub const VERSION: &str = env!("CODEMAP_VERSION");
