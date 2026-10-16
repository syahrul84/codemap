//! Vue / Nuxt adapter for `codemap`: detection (Vue 2/3, Nuxt 2/3/4, build tool,
//! aliases), components with their props/emits/slots, who renders whom,
//! Pinia stores, Vuex modules and composables.

pub mod api;
pub mod component;
pub mod detect;
pub mod doctor;
pub mod extract;
pub mod i18n;
pub mod naming;
pub mod next;
pub mod pages;
pub mod paths;
pub mod refresh;
pub mod registry;
pub mod ts_types;

pub use detect::VueDetection;
pub use refresh::{VueReport, refresh, stored_detection};
