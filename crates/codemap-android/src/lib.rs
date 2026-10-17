//! Android adapter for `codemap`: Kotlin and Java apps, with Jetpack Compose
//! screens, manifest components and `strings.xml` translations.

pub mod extract;
pub mod index;
pub mod parse;
pub mod view;

pub use index::{AndroidReport, refresh};
