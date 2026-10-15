//! Core of `pidx`: index model, SQLite store, scanner, freshness checks and
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

pub use config::Config;
pub use error::{Error, Result};
pub use scan::{ScanOptions, ScanReport};
pub use store::Store;

/// Version of the `pidx` workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
