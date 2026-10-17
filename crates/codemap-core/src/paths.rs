//! Where an index lives: `<data_dir>/<project_id>/index.db`, always outside
//! the scanned project.

use std::path::{Path, PathBuf};

use crate::{Error, Result, git};

pub const INDEX_FILE: &str = "index.db";

/// `explicit` (from `--data-dir` / `CODEMAP_DATA_DIR`), else the platform cache dir.
pub fn resolve_data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| dirs::cache_dir().map(|d| d.join("codemap")))
        .ok_or_else(|| Error::Config("no data dir: pass --data-dir or set CODEMAP_DATA_DIR".into()))
}

/// `explicit` (from `--project-id` / `CODEMAP_PROJECT_ID`), else a hash of the git
/// remote + root commit, else a hash of the canonical project path.
pub fn resolve_project_id(explicit: Option<&str>, root: &Path) -> Result<String> {
    if let Some(id) = explicit {
        validate_project_id(id)?;
        return Ok(id.to_owned());
    }
    let remote = git::remote_url(root);
    let root_commit = git::root_commit(root);
    if remote.is_some() || root_commit.is_some() {
        let mut key = format!(
            "git:{}\n{}",
            remote.unwrap_or_default(),
            root_commit.unwrap_or_default()
        );
        // Two folders of one repository (a monorepo's `frontend/` and
        // `backend/`) are two projects: each gets its own index.
        if let Some(sub) = git::prefix(root).filter(|p| !p.is_empty()) {
            key.push_str(&format!("\n{}", sub.trim_end_matches('/')));
        }
        return Ok(short_hash(&key));
    }
    // A folder holding several apps, each its own repository (a workspace):
    // the apps' git info, so two workspaces mounted at /workspace differ.
    let mut apps: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(".git").exists())
        .filter_map(|e| {
            let p = e.path();
            let commit = git::root_commit(&p)?;
            Some(format!(
                "{}\n{commit}",
                git::remote_url(&p).unwrap_or_default()
            ))
        })
        .collect();
    if !apps.is_empty() {
        apps.sort();
        return Ok(short_hash(&format!("git-apps:{}", apps.join("\n"))));
    }
    let canonical = root
        .canonicalize()
        .map_err(|e| Error::io(root, e))?
        .to_string_lossy()
        .into_owned();
    tracing::warn!(
        path = %canonical,
        "no project id given and no git info; deriving it from the path \
         (set CODEMAP_PROJECT_ID when running in Docker)"
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
    fn folders_of_one_repository_get_their_own_ids() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        for sub in ["frontend", "backend"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
            std::fs::write(dir.path().join(sub).join("a"), sub).unwrap();
        }
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "x",
        ]);
        let top = resolve_project_id(None, dir.path()).unwrap();
        let front = resolve_project_id(None, &dir.path().join("frontend")).unwrap();
        let back = resolve_project_id(None, &dir.path().join("backend")).unwrap();
        assert_ne!(front, back);
        assert_ne!(top, front);
    }

    #[test]
    fn index_path_layout() {
        assert_eq!(
            index_path(Path::new("/data"), "abc"),
            PathBuf::from("/data/abc/index.db")
        );
    }
}
