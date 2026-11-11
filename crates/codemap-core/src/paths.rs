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

/// [`resolve_project_id`], with an index of its own for a second checkout of
/// the same repository (another clone, a git worktree). Both have the same
/// remote and first commit, so by that alone they would share one index and
/// each would undo the other's: every call re-reads what differs, and an
/// answer given before that is done describes the other checkout.
///
/// The checkout an index was made for keeps the id; another one, while the
/// first still exists, gets an id of its own from its path. A project that
/// moved (the folder it was indexed at is gone) keeps its index. In Docker
/// the project is always at the mount point, so nothing changes there.
pub fn resolve_project_id_in(
    explicit: Option<&str>,
    root: &Path,
    data_dir: &Path,
) -> Result<String> {
    let id = resolve_project_id(explicit, root)?;
    if explicit.is_some() || crate::updates::in_docker() {
        return Ok(id);
    }
    let here = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // The indexes under this id: the project's own, and one per app when it
    // is a workspace (`<id>-backend`).
    let mut dirs = vec![data_dir.join(&id)];
    for entry in std::fs::read_dir(data_dir).into_iter().flatten().flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(&format!("{id}-"))
        {
            dirs.push(entry.path());
        }
    }
    let recorded: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| recorded_root(&d.join(INDEX_FILE)))
        .collect();
    let ours = |r: &PathBuf| r == &here || r.starts_with(&here);
    if recorded.is_empty() || recorded.iter().any(ours) || !recorded.iter().any(|r| r.is_dir()) {
        return Ok(id);
    }
    Ok(short_hash(&format!("{id}\npath:{}", here.display())))
}

/// The folder an index was made for, as it says itself.
fn recorded_root(index: &Path) -> Option<PathBuf> {
    if !index.is_file() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        index,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [crate::store::meta_keys::PROJECT_ROOT],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .map(PathBuf::from)
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
    fn a_second_checkout_gets_its_own_index() {
        let base = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let first = base.path().join("first");
        std::fs::create_dir(&first).unwrap();
        git(&first, &["init", "-q"]);
        std::fs::write(first.join("a"), "a").unwrap();
        git(&first, &["add", "."]);
        git(&first, &["commit", "-qm", "x"]);
        git(
            &first,
            &["remote", "add", "origin", "https://example.test/shop.git"],
        );
        // Another clone of the same repository, and a worktree of the first.
        let second = base.path().join("second");
        git(base.path(), &["clone", "-q", "first", "second"]);
        git(
            &second,
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.test/shop.git",
            ],
        );
        let tree = base.path().join("tree");
        git(&first, &["worktree", "add", "-q", "../tree"]);

        // Nothing indexed yet: all three would use the same id.
        let id = resolve_project_id_in(None, &first, data.path()).unwrap();
        assert_eq!(resolve_project_id(None, &second).unwrap(), id);
        // The first is indexed: it keeps the id, the others get their own.
        let store = crate::Store::open(&index_path(data.path(), &id)).unwrap();
        store
            .set_meta(
                crate::store::meta_keys::PROJECT_ROOT,
                &first.canonicalize().unwrap().to_string_lossy(),
            )
            .unwrap();
        drop(store);
        assert_eq!(
            resolve_project_id_in(None, &first, data.path()).unwrap(),
            id
        );
        let other = resolve_project_id_in(None, &second, data.path()).unwrap();
        let worktree = resolve_project_id_in(None, &tree, data.path()).unwrap();
        assert_ne!(other, id);
        assert_ne!(worktree, id);
        assert_ne!(worktree, other);
        assert_eq!(
            resolve_project_id_in(None, &second, data.path()).unwrap(),
            other
        );
        // An explicit id is used as given.
        assert_eq!(
            resolve_project_id_in(Some("mine"), &second, data.path()).unwrap(),
            "mine"
        );
        // The first is gone (the project moved): its index goes with it.
        std::fs::rename(&first, base.path().join("moved")).unwrap();
        assert_eq!(
            resolve_project_id_in(None, &second, data.path()).unwrap(),
            id
        );
    }

    #[test]
    fn index_path_layout() {
        assert_eq!(
            index_path(Path::new("/data"), "abc"),
            PathBuf::from("/data/abc/index.db")
        );
    }
}
