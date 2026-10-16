//! New-release notice. At most once a day `codemap serve` (and `doctor`) ask
//! GitHub for the latest release; the answer is kept in
//! `<data_dir>/update-check.json`, and `project_overview` / `doctor` read it
//! to say that a newer version exists. Nothing about the project is sent:
//! the request is a plain GET of the public releases page.
//!
//! Turned off with `CODEMAP_NO_UPDATE_CHECK=1`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const REPO: &str = "syahrul84/codemap";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const FILE: &str = "update-check.json";
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const OFF_ENV: &str = "CODEMAP_NO_UPDATE_CHECK";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checked {
    /// When GitHub was last asked (RFC 3339).
    pub checked_at: String,
    /// The latest release's tag, e.g. `v0.2.0`.
    pub latest: Option<String>,
}

/// `CODEMAP_NO_UPDATE_CHECK` set to anything but `0` / `false` / empty.
pub fn disabled() -> bool {
    std::env::var(OFF_ENV)
        .ok()
        .is_some_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no"))
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

pub fn load(data_dir: &Path) -> Option<Checked> {
    serde_json::from_str(&std::fs::read_to_string(path(data_dir)).ok()?).ok()
}

pub fn save(data_dir: &Path, latest: Option<&str>) {
    let c = Checked {
        checked_at: jiff::Timestamp::now().to_string(),
        latest: latest.map(str::to_owned),
    };
    if let Ok(text) = serde_json::to_string(&c) {
        let _ = std::fs::create_dir_all(data_dir);
        let _ = std::fs::write(path(data_dir), text);
    }
}

/// True when the last check is older than a day (or never happened).
pub fn due(data_dir: &Path) -> bool {
    let Some(c) = load(data_dir) else {
        return true;
    };
    let Ok(at) = c.checked_at.parse::<jiff::Timestamp>() else {
        return true;
    };
    let age = jiff::Timestamp::now().duration_since(at);
    age.unsigned_abs() >= INTERVAL
}

/// `v0.2.0` / `0.2.0` → `(0, 2, 0)`.
pub fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let v = version.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Is `latest` newer than `current`?
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// One line about a newer release, from the last check (no network).
pub fn notice(data_dir: &Path) -> Option<String> {
    if disabled() {
        return None;
    }
    let latest = load(data_dir)?.latest?;
    is_newer(&latest, VERSION).then(|| {
        let how = if in_docker() {
            "pull the latest code and rebuild the image".to_owned()
        } else {
            "run `codemap update`".to_owned()
        };
        format!(
            "codemap {} is available (this is {VERSION}): {how}. Release notes: https://github.com/{REPO}/releases/tag/{latest}",
            latest.trim_start_matches('v')
        )
    })
}

/// Set in the Docker image.
pub fn in_docker() -> bool {
    std::env::var("CODEMAP_IN_DOCKER").is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-rc1", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn notice_from_the_last_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(due(dir.path()));
        assert_eq!(notice(dir.path()), None);
        save(dir.path(), Some("v99.0.0"));
        assert!(!due(dir.path()));
        let n = notice(dir.path()).unwrap();
        assert!(n.starts_with("codemap 99.0.0 is available"), "{n}");
        save(dir.path(), Some(&format!("v{VERSION}")));
        assert_eq!(notice(dir.path()), None);
    }
}
