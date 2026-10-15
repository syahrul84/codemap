//! JavaScript / TypeScript analysis with oxc, plus Vue single-file component
//! splitting and a template tokenizer. Framework knowledge (Vue, Nuxt) lives
//! in `pidx-vue`; this crate only reports what the code says.

pub mod facts;
pub mod literal;
pub mod script;
pub mod sfc;
pub mod template;

pub use facts::*;
pub use script::{ScriptKind, analyze};
