//! PHP parsing for `codemap` (framework-agnostic): tree-sitter-php extraction of
//! classes, interfaces, traits, enums, methods, properties, constants, enum
//! cases and functions, with namespace and `use` resolution to FQNs.

pub mod ast;
pub mod doc;
pub mod extract;
pub mod index;
pub mod names;
pub mod parse;
pub mod refs;

pub use extract::{ParseWarning, ParsedFile, extract};
pub use index::{PHP_EXTRACTOR_VERSION, PhpReport, RefreshReport, index_pending, refresh};
pub use names::{Scope, UseKind};
