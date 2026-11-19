//! Git facts for response freshness. Git is optional: anything that fails
//! (no git binary, not a repo, unsafe directory) yields `None`, never an error.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitInfo {
    pub commit: Option<String>,
    pub dirty: Option<bool>,
}

pub fn info(root: &Path) -> GitInfo {
    let commit = head(root);
    // --no-optional-locks: the project is usually mounted read-only, and a
    // plain `git status` tries to refresh (write) .git/index.
    let dirty = commit.as_ref().and_then(|_| dirty(root));
    GitInfo { commit, dirty }
}

/// What `HEAD` points at, read from git's files (no git process, so it can
/// be asked every few seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadState {
    /// Changes when a branch is switched, a commit is made, or a pull,
    /// rebase or reset moves the branch.
    pub marker: String,
    /// Git is in the middle of something (a rebase, a merge, a checkout
    /// writing files): the working tree is not settled yet.
    pub busy: bool,
}

/// The state of `HEAD` for the repository `root` is in (its own, or a
/// folder above; a worktree's or a submodule's `.git` file is followed).
/// `None` outside a repository.
pub fn head_state(root: &Path) -> Option<HeadState> {
    let dot_git = root
        .ancestors()
        .take(8)
        .map(|d| d.join(".git"))
        .find(|p| p.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // `gitdir: /path/to/.git/worktrees/name` (relative to the file's folder).
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(target)
    };
    // A worktree keeps `HEAD` in its own folder and the branches in the
    // main repository's.
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.clone());
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let at = match head.strip_prefix("ref: ") {
        // The branch's commit, or (packed) when the packed list last changed.
        Some(name) => std::fs::read_to_string(common.join(name))
            .map(|c| c.trim().to_owned())
            .or_else(|_| {
                std::fs::metadata(common.join("packed-refs"))
                    .and_then(|m| m.modified())
                    .map(|t| format!("{t:?}"))
            })
            .unwrap_or_default(),
        None => String::new(),
    };
    let busy = ["index.lock", "rebase-merge", "rebase-apply", "MERGE_HEAD"]
        .iter()
        .any(|f| git_dir.join(f).exists());
    Some(HeadState {
        marker: format!("{head} {at}"),
        busy,
    })
}

/// `HEAD` commit only (fast: no working-tree scan).
pub fn head(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "HEAD"])
}

/// Whether the working tree has changes. Slow on large repos and on Docker
/// Desktop bind mounts (hundreds of ms), so callers reuse it when they can.
///
/// `core.checkStat=minimal` + `core.trustctime=false`: inside a container the
/// project's inode/device/owner differ from what the host's git index recorded,
/// which otherwise makes git re-read every tracked file (seconds on big repos).
pub fn dirty(root: &Path) -> Option<bool> {
    git(
        root,
        &[
            "-c",
            "core.checkStat=minimal",
            "-c",
            "core.trustctime=false",
            "--no-optional-locks",
            "status",
            "--porcelain",
        ],
    )
    .map(|s| !s.is_empty())
}

/// Files changed and not committed (staged, unstaged and untracked), relative
/// to `root`. Read-only: no locks are taken.
pub fn changed_files(root: &Path) -> Option<Vec<String>> {
    let base = [
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustctime=false",
        "--no-optional-locks",
    ];
    let mut args: Vec<&str> = base.to_vec();
    args.extend(["diff", "--name-only", "--relative", "HEAD"]);
    let mut out: Vec<String> = git(root, &args)?.lines().map(str::to_owned).collect();
    let mut args: Vec<&str> = base.to_vec();
    args.extend(["ls-files", "--others", "--exclude-standard"]);
    if let Some(untracked) = git(root, &args) {
        out.extend(untracked.lines().map(str::to_owned));
    }
    out.retain(|l| !l.is_empty());
    out.sort();
    out.dedup();
    Some(out)
}

/// Files changed on the current branch: since it left `base` (or the first of
/// origin/HEAD, origin/main, origin/master, main, master, develop that exists),
/// plus uncommitted and untracked files. Returns the base used.
pub fn branch_files(root: &Path, base: Option<&str>) -> Option<(String, Vec<String>)> {
    let opts = [
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustctime=false",
        "--no-optional-locks",
    ];
    let candidates: Vec<String> = match base {
        Some(b) => vec![b.to_owned()],
        None => {
            let mut v = Vec::new();
            if let Some(h) = git(
                root,
                &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            ) {
                v.push(h.trim_start_matches("refs/remotes/").to_owned());
            }
            v.extend(
                ["origin/main", "origin/master", "main", "master", "develop"]
                    .iter()
                    .map(|s| (*s).to_owned()),
            );
            v
        }
    };
    let head = head(root)?;
    let mut out = changed_files(root).unwrap_or_default();
    let mut used = "uncommitted changes".to_owned();
    for c in candidates {
        let Some(mb) = git(root, &["merge-base", "HEAD", &c]) else {
            continue;
        };
        if mb != head {
            let mut args: Vec<&str> = opts.to_vec();
            args.extend(["diff", "--name-only", "--relative", &mb]);
            if let Some(d) = git(root, &args) {
                out.extend(d.lines().map(str::to_owned));
            }
            used = c;
        } else {
            used = format!("{c} (no commits of its own on this branch; uncommitted changes)");
        }
        break;
    }
    out.retain(|l| !l.is_empty());
    out.sort();
    out.dedup();
    Some((used, out))
}

/// One file changed on the branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// `A` added, `M` modified, `D` deleted, `R` renamed.
    pub status: char,
    /// Relative to the root; the new name for a rename.
    pub path: String,
    /// The old name of a renamed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The branch's changes with their kind: committed since it left `base`
/// (as in [`branch_files`]) plus uncommitted and untracked files. Also the
/// commit it left from, to read files as they were.
pub fn branch_changes(root: &Path, base: Option<&str>) -> Option<(String, String, Vec<Change>)> {
    let opts = [
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustctime=false",
        "--no-optional-locks",
    ];
    let head = head(root)?;
    let candidates: Vec<String> = match base {
        Some(b) => vec![b.to_owned()],
        None => {
            let mut v = Vec::new();
            if let Some(h) = git(
                root,
                &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            ) {
                v.push(h.trim_start_matches("refs/remotes/").to_owned());
            }
            v.extend(
                ["origin/main", "origin/master", "main", "master", "develop"]
                    .iter()
                    .map(|s| (*s).to_owned()),
            );
            v
        }
    };
    let mut from = head.clone();
    let mut used = "uncommitted changes".to_owned();
    for c in candidates {
        if let Some(mb) = git(root, &["merge-base", "HEAD", &c]) {
            used = if mb == head {
                format!("{c} (no commits of its own on this branch; uncommitted changes)")
            } else {
                c
            };
            from = mb;
            break;
        }
    }
    // `git diff <commit>` compares it with the working tree: committed and
    // uncommitted changes to tracked files.
    let mut args: Vec<&str> = opts.to_vec();
    args.extend(["diff", "--name-status", "-M", "--relative", &from]);
    let mut out = Vec::new();
    for line in git(root, &args)?.lines() {
        let mut parts = line.split('\t');
        let (Some(st), Some(first)) = (parts.next(), parts.next()) else {
            continue;
        };
        let status = st.chars().next().unwrap_or('M');
        match (status, parts.next()) {
            ('R' | 'C', Some(new)) => out.push(Change {
                status: if status == 'C' { 'A' } else { 'R' },
                path: new.to_owned(),
                from: Some(first.to_owned()),
            }),
            _ => out.push(Change {
                status: if matches!(status, 'A' | 'D') {
                    status
                } else {
                    'M'
                },
                path: first.to_owned(),
                from: None,
            }),
        }
    }
    let mut args: Vec<&str> = opts.to_vec();
    args.extend(["ls-files", "--others", "--exclude-standard"]);
    if let Some(untracked) = git(root, &args) {
        out.extend(untracked.lines().filter(|l| !l.is_empty()).map(|l| Change {
            status: 'A',
            path: l.to_owned(),
            from: None,
        }));
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    Some((used, from, out))
}

/// A file's content at `rev` (`HEAD`, a commit), when it existed there.
pub fn show(root: &Path, rev: &str, path: &str) -> Option<String> {
    git(root, &["show", &format!("{rev}:./{path}")])
}

pub fn remote_url(root: &Path) -> Option<String> {
    git(root, &["config", "--get", "remote.origin.url"]).filter(|s| !s.is_empty())
}

/// The folder's path inside its repository (`frontend/`), empty at the top.
pub fn prefix(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "--show-prefix"])
}

/// The repository's first commit (lexicographically smallest if there are several roots).
pub fn root_commit(root: &Path) -> Option<String> {
    git(root, &["rev-list", "--max-parents=0", "HEAD"])?
        .lines()
        .min()
        .map(str::to_owned)
}

/// Runs `git -C <root> <args>`; trimmed stdout on success.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        Ok(out) => {
            tracing::debug!(?args, status = %out.status, "git command failed");
            None
        }
        Err(e) => {
            tracing::debug!(?args, error = %e, "git not available");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=codemap",
                "-c",
                "user.email=codemap@example.test",
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn non_repo_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(info(dir.path()), GitInfo::default());
        assert_eq!(root_commit(dir.path()), None);
        assert_eq!(remote_url(dir.path()), None);
    }

    #[test]
    fn repo_reports_commit_and_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q"]);
        std::fs::write(root.join("a.php"), "<?php\n").unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-q", "-m", "init"]);

        let clean = info(root);
        let commit = clean.commit.clone().unwrap();
        assert_eq!(commit.len(), 40);
        assert_eq!(clean.dirty, Some(false));
        assert_eq!(root_commit(root).as_deref(), Some(commit.as_str()));

        std::fs::write(root.join("a.php"), "<?php // changed\n").unwrap();
        assert_eq!(info(root).dirty, Some(true));

        run(
            root,
            &["remote", "add", "origin", "git@example.test:acme/api.git"],
        );
        assert_eq!(
            remote_url(root).as_deref(),
            Some("git@example.test:acme/api.git")
        );
    }

    #[test]
    fn head_state_changes_with_the_branch_and_its_commits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(head_state(root), None);
        run(root, &["init", "-q"]);
        std::fs::create_dir(root.join("app")).unwrap();
        std::fs::write(root.join("app/a.php"), "<?php\n").unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-q", "-m", "init"]);
        let first = head_state(root).unwrap();
        assert!(!first.busy);
        // Asked again, and from a folder inside the repository: the same.
        assert_eq!(head_state(root), Some(first.clone()));
        assert_eq!(head_state(&root.join("app")), Some(first.clone()));
        // A new branch at the same commit, then a commit on it.
        run(root, &["checkout", "-q", "-b", "feature"]);
        let switched = head_state(root).unwrap();
        assert_ne!(switched.marker, first.marker);
        std::fs::write(root.join("app/a.php"), "<?php // changed\n").unwrap();
        run(root, &["commit", "-q", "-am", "more"]);
        let committed = head_state(root).unwrap();
        assert_ne!(committed.marker, switched.marker);
        // Back on the first branch: what it was.
        run(root, &["checkout", "-q", "-"]);
        assert_eq!(head_state(root).unwrap().marker, first.marker);
        // Git in the middle of something.
        std::fs::write(root.join(".git/index.lock"), "").unwrap();
        assert!(head_state(root).unwrap().busy);
        std::fs::remove_file(root.join(".git/index.lock")).unwrap();
        // A worktree has its own HEAD.
        let other = tempfile::tempdir().unwrap();
        let tree = other.path().join("tree");
        run(
            root,
            &["worktree", "add", "-q", tree.to_str().unwrap(), "feature"],
        );
        assert_eq!(head_state(&tree).unwrap().marker, committed.marker);
    }
}
