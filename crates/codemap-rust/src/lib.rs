//! Rust adapter for `codemap`: crates and workspaces, with their types,
//! functions, trait impls, binaries and command-line subcommands.

pub mod extract;
pub mod index;
pub mod parse;

pub use index::{RustReport, overview, refresh, test_ranges};
