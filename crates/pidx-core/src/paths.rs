//! Where an index lives: `<data_dir>/<project_id>/index.db`, always outside
//! the scanned project.

use std::path::{Path, PathBuf};

use crate::{Error, Result, git};

pub const INDEX_FILE: &str = "index.db";

/// `explicit` (from `--data-dir` / `PIDX_DATA_DIR`), else the platform cache dir.
pub fn resolve_data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| dirs::cache_dir().map(|d| d.join("pidx")))
        .ok_or_else(|| Error::Config("no data dir: pass --data-dir or set PIDX_DATA_DIR".into()))
}

/// `explicit` (from `--project-id` / `PIDX_PROJECT_ID`), else a hash of the git
/// remote + root commit, else a hash of the canonical project path.
pub fn resolve_project_id(explicit: Option<&str>, root: &Path) -> Result<String> {
    if let Some(id) = explicit {
        validate_project_id(id)?;
        return Ok(id.to_owned());
    }
    let remote = git::remote_url(root);
    let root_commit = git::root_commit(root);
    if remote.is_some() || root_commit.is_some() {
        let key = format!(
            "git:{}\n{}",
            remote.unwrap_or_default(),
            root_commit.unwrap_or_default()
        );
        return Ok(short_hash(&key));
    }
    let canonical = root
        .canonicalize()
        .map_err(|e| Error::io(root, e))?
        .to_string_lossy()
        .into_owned();
    tracing::warn!(
        path = %canonical,
        "no project id given and no git info; deriving it from the path \
         (set PIDX_PROJECT_ID when running in Docker)"
    );
    Ok(short_hash(&format!("path:{canonical}")))
}

/// The id becomes a directory name, so keep it to a safe character set.
pub fn validate_project_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "invalid project id {id:?}: use 1-64 of [A-Za-z0-9_-]"
        )))
    }
}

pub fn index_path(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir.join(project_id).join(INDEX_FILE)
}

fn short_hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex()[..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_id_is_validated() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_project_id(Some("abc_123-x"), dir.path()).unwrap(),
            "abc_123-x"
        );
        for bad in ["", "../etc", "a/b", "a b", &"x".repeat(65)] {
            assert!(
                resolve_project_id(Some(bad), dir.path()).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn path_fallback_is_stable_and_short() {
        let dir = tempfile::tempdir().unwrap();
        let a = resolve_project_id(None, dir.path()).unwrap();
        let b = resolve_project_id(None, dir.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        validate_project_id(&a).unwrap();
    }

    #[test]
    fn index_path_layout() {
        assert_eq!(
            index_path(Path::new("/data"), "abc"),
            PathBuf::from("/data/abc/index.db")
        );
    }
}
