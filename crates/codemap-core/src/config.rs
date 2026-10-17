//! Optional `.codemap.toml` in the scanned project's root. Every field has a
//! default, so zero config works.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const CONFIG_FILE: &str = ".codemap.toml";
/// The config file's name before codemap (pidx); read when there is no new one.
pub const OLD_CONFIG_FILE: &str = ".pidx.toml";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub project: ProjectConfig,
    pub scan: ScanConfig,
    pub laravel: LaravelConfig,
    pub freshness: FreshnessConfig,
    /// Linked repos (e.g. the Laravel backend of a Vue / Nuxt frontend).
    pub links: Vec<LinkConfig>,
    pub workspace: WorkspaceConfig,
    pub i18n: I18nConfig,
    /// How this app's tests are run, when not the plain defaults.
    pub tests: TestsConfig,
    /// House copy rules: words that must not appear in user-facing text.
    pub copy: CopyConfig,
    /// Release check settings.
    pub release: ReleaseConfig,
    /// Databases to compare migrations with (read-only); see [`DatabaseConfig`].
    pub databases: Vec<DatabaseConfig>,
    /// Project rules tied to files: a note returned when a matching file changes.
    pub rules: Vec<RuleConfig>,
    /// Wrong-answer reports.
    pub report: ReportConfig,
}

/// `[report] send = "ask" | "never"`: whether a wrong-answer report may be
/// offered for sending to the codemap repository (always after asking).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send: Option<String>,
}

/// `[[rules]] files = ["database/migrations/**"], when = "modified",
/// note = "Never edit a migration that has run; add a new one."`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    /// Patterns in `.gitignore` style, relative to the app's folder.
    pub files: Vec<String>,
    /// What to tell whoever changes a matching file.
    pub note: String,
    /// `added`, `modified`, `deleted` or `changed` (any; the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// `problem` makes the release check fail on it; default `note`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// In a workspace's file: only for this app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

/// A database whose `migrations` table is compared with the migration files.
/// The connection URL is read from the environment variable `env` (never from
/// a file): `postgres://user:pass@host:5432/db`, `mysql://…`, `sqlite:///path`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// `local`, `staging`.
    pub name: String,
    /// Environment variable holding the URL, e.g. `CODEMAP_DB_LOCAL`.
    pub env: String,
    /// In a workspace: the app this database belongs to (default: every Laravel app).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Migrations table (default: `config/database.php`, else `migrations`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// `"live"`: also read the tables and columns from this database
    /// (information_schema / pragma, read-only) for `get_schema` and `verify`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// A structure dump (`mysqldump --no-data`, `pg_dump --schema-only`),
    /// relative to the project, read instead of connecting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dump: Option<PathBuf>,
}

/// Commands that run tests; the test files are appended. Unset: detected
/// (`php artisan test`, `<pnpm exec|yarn|npx> vitest run`, `… playwright test`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestsConfig {
    /// e.g. `docker compose exec -T app php artisan test`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// Unit tests, e.g. `pnpm exec vitest run`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
    /// Browser tests, e.g. `pnpm exec playwright test`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
}

impl TestsConfig {
    pub fn is_empty(&self) -> bool {
        self.php.is_none() && self.js.is_none() && self.browser.is_none()
    }
}

/// `[copy] banned = { ms = ["beg"], en = ["bag", "bags", "save food"], all = ["rescue"] }`:
/// words or phrases (case-insensitive, whole words) checked in translation
/// files of that locale and, for `all` and the base locale, admin (Filament) labels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyConfig {
    pub banned: std::collections::BTreeMap<String, Vec<String>>,
}

/// `[release] ignore = ["ALLOW_DB_WIPE", "app/Support/Legacy.php:12"]`: release
/// check problems containing any of these texts are reported as ignored.
/// A `codemap-ignore` comment on the line (or the line above) does the same.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseConfig {
    pub ignore: Vec<String>,
}

/// Translation checks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct I18nConfig {
    /// Locales the app ships (e.g. `["en", "ms"]`): only these are checked for
    /// missing keys; others are listed as partial. Empty: locales with at least
    /// half of the base locale's keys count as shipped.
    pub locales: Vec<String>,
}

/// A folder holding several apps (e.g. `backend/`, `consumer/`, `merchant/`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// App folders relative to the root. Empty: found automatically when the
    /// root is not itself a project.
    pub apps: Vec<String>,
    /// Test commands per app: `[workspace.tests.backend] php = "…"`.
    pub tests: std::collections::BTreeMap<String, TestsConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub name: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            // Laravel, then Vue / Nuxt source dirs; missing dirs are skipped.
            include: [
                "app",
                "routes",
                "database",
                "config",
                "bootstrap",
                "lang",
                "resources/lang",
                "resources/views",
                "src",
                "resources/js",
                "components",
                "pages",
                "layouts",
                "composables",
                "stores",
                "store",
                "plugins",
                "middleware",
                "utils",
                "mixins",
                "i18n",
                "locales",
                "tests",
                "test",
                // React / Next.js and Supabase.
                "lib",
                "hooks",
                "context",
                "contexts",
                "providers",
                "actions",
                "features",
                "services",
                "types",
                "supabase",
                "middleware.ts",
                "middleware.js",
                "proxy.ts",
                "proxy.js",
            ]
            .map(String::from)
            .to_vec(),
            // Anchored to the project root (`/vendor`), so a source folder that
            // happens to be called `vendor` or `public` is still indexed.
            exclude: [
                "/vendor",
                "node_modules",
                "/storage",
                "/public",
                "/bootstrap/cache",
                "/dist",
                "/.output",
                "/coverage",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtisanMode {
    #[default]
    Off,
    Auto,
    On,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaravelConfig {
    pub artisan: ArtisanMode,
    pub artisan_command: String,
    /// `route:list` is killed after this long and static routes are used.
    pub artisan_timeout_ms: u64,
}

impl Default for LaravelConfig {
    fn default() -> Self {
        Self {
            artisan: ArtisanMode::Off,
            artisan_command: "php artisan".into(),
            artisan_timeout_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FreshnessConfig {
    pub check_interval_ms: u64,
}

impl Default for FreshnessConfig {
    fn default() -> Self {
        Self {
            check_interval_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    /// Set for apps of the same workspace: their index, so it is shared
    /// rather than built twice.
    #[serde(skip)]
    pub index_path: Option<PathBuf>,
}

const KNOWN_KEYS: &[(&str, &[&str])] = &[
    ("project", &["name", "kind"]),
    ("scan", &["include", "exclude"]),
    (
        "laravel",
        &["artisan", "artisan_command", "artisan_timeout_ms"],
    ),
    ("freshness", &["check_interval_ms"]),
    ("links", &["name", "path", "kind", "api_base"]),
    ("workspace", &["apps", "tests"]),
    ("i18n", &["locales"]),
    ("tests", &["php", "js", "browser"]),
    ("copy", &["banned"]),
    ("release", &["ignore"]),
    (
        "databases",
        &["name", "env", "app", "table", "schema", "dump"],
    ),
    ("rules", &["files", "note", "when", "level", "app"]),
    ("report", &["send"]),
];

impl Config {
    /// Loads `<root>/.codemap.toml` (or the older `.pidx.toml`), or defaults
    /// when neither exists.
    /// Unknown keys are logged and ignored.
    pub fn load(root: &Path) -> Result<Self> {
        let path = if !root.join(CONFIG_FILE).exists() && root.join(OLD_CONFIG_FILE).is_file() {
            root.join(OLD_CONFIG_FILE)
        } else {
            root.join(CONFIG_FILE)
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::io(path, e)),
        };
        let config = Self::parse(&text).map_err(|source| Error::TomlParse {
            path: path.clone(),
            source,
        })?;
        for key in unknown_keys(&text) {
            tracing::warn!(file = %path.display(), key, "ignoring unknown config key");
        }
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

/// Dotted paths of keys that `Config` does not know about.
pub fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut unknown = Vec::new();
    for (section, value) in &table {
        let Some((_, fields)) = KNOWN_KEYS.iter().find(|(name, _)| name == section) else {
            unknown.push(section.clone());
            continue;
        };
        let tables: Vec<&toml::Table> = match value {
            toml::Value::Table(t) => vec![t],
            toml::Value::Array(items) => items.iter().filter_map(|v| v.as_table()).collect(),
            _ => Vec::new(),
        };
        for t in tables {
            for key in t.keys() {
                if !fields.contains(&key.as_str()) {
                    unknown.push(format!("{section}.{key}"));
                }
            }
        }
    }
    unknown.sort();
    unknown.dedup();
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_EXAMPLE: &str = r#"
[project]
name = "shop-api"
kind = "laravel"

[scan]
include = ["app", "routes", "database", "config", "bootstrap"]
exclude = ["vendor", "node_modules", "storage", "public", "bootstrap/cache"]

[laravel]
artisan = "auto"
artisan_command = "php artisan"

[freshness]
check_interval_ms = 500

[[links]]
name = "shop-web"
path = "../shop-web"
kind = "nuxt"
api_base = "/api"
"#;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.scan.include[0], "app");
        assert_eq!(config.freshness.check_interval_ms, 2000);
        assert_eq!(config.laravel.artisan, ArtisanMode::Off);
    }

    #[test]
    fn parses_full_example() {
        let config = Config::parse(FULL_EXAMPLE).unwrap();
        assert_eq!(config.project.name.as_deref(), Some("shop-api"));
        assert_eq!(config.laravel.artisan, ArtisanMode::Auto);
        assert_eq!(config.freshness.check_interval_ms, 500);
        assert_eq!(config.links.len(), 1);
        assert_eq!(config.links[0].api_base.as_deref(), Some("/api"));
        assert!(unknown_keys(FULL_EXAMPLE).is_empty());
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let config = Config::parse("[scan]\ninclude = [\"src\"]\n").unwrap();
        assert_eq!(config.scan.include, ["src"]);
        assert_eq!(config.scan.exclude, ScanConfig::default().exclude);
    }

    #[test]
    fn unknown_keys_do_not_fail() {
        let text = "[scan]\ninclude = [\"app\"]\nfollow = true\n[extra]\nx = 1\n";
        assert!(Config::parse(text).is_ok());
        assert_eq!(unknown_keys(text), ["extra", "scan.follow"]);
    }

    #[test]
    fn invalid_value_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[laravel]\nartisan = \"maybe\"\n",
        )
        .unwrap();
        assert!(matches!(
            Config::load(dir.path()),
            Err(Error::TomlParse { .. })
        ));
    }

    #[test]
    fn old_config_name_still_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(OLD_CONFIG_FILE),
            "[project]\nname = \"old\"\n",
        )
        .unwrap();
        assert_eq!(
            Config::load(dir.path()).unwrap().project.name.as_deref(),
            Some("old")
        );
        // The new name wins when both exist.
        std::fs::write(dir.path().join(CONFIG_FILE), "[project]\nname = \"new\"\n").unwrap();
        assert_eq!(
            Config::load(dir.path()).unwrap().project.name.as_deref(),
            Some("new")
        );
    }
}
