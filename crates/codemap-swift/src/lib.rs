//! Swift adapter for `codemap`: Swift packages and Xcode apps (iOS, macOS),
//! with SwiftUI views, the screens they make up, and string catalogs.

pub mod extract;
pub mod index;
pub mod parse;
pub mod view;

pub use index::{SwiftReport, refresh};
