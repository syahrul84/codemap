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

pub fn remote_url(root: &Path) -> Option<String> {
    git(root, &["config", "--get", "remote.origin.url"]).filter(|s| !s.is_empty())
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
            .args(["-c", "user.name=pidx", "-c", "user.email=pidx@example.test"])
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
}
