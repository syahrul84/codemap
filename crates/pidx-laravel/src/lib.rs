//! Laravel adapter for `pidx`: version and layout detection, static route
//! extraction (with optional `artisan route:list`), and `doctor` reports.
//! Phase 4 adds models, the migration-replayed schema and Form Requests.

pub mod config_php;
pub mod detect;
pub mod doctor;
pub mod filament;
pub mod inflector;
pub mod lang;
pub mod literal;
pub mod models;
pub mod refresh;
pub mod requests;
pub mod responses;
pub mod routes;
pub mod schema;
pub mod settings;

pub use detect::{Detection, Layout};
pub use refresh::{LaravelAdapter, LaravelReport, RefreshReport, refresh};
