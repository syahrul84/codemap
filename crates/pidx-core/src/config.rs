//! Optional `.pidx.toml` in the scanned project's root. Every field has a
//! default, so zero config works.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const CONFIG_FILE: &str = ".pidx.toml";

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
}

/// A folder holding several apps (e.g. `backend/`, `consumer/`, `merchant/`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// App folders relative to the root. Empty: found automatically when the
    /// root is not itself a project.
    pub apps: Vec<String>,
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
    ("workspace", &["apps"]),
];

impl Config {
    /// Loads `<root>/.pidx.toml`, or defaults when it does not exist.
    /// Unknown keys are logged and ignored.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
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
            tracing::warn!(file = CONFIG_FILE, key, "ignoring unknown config key");
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
}
