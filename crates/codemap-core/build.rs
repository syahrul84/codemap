//! The version codemap reports: the Cargo version for a release build
//! (`CODEMAP_RELEASE=1`, set by the release workflow), otherwise
//! `<version>-dev+<commit>`, so a development build is never mistaken for
//! the release it comes before.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CODEMAP_RELEASE");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // A checkout of the release tag itself is the release too.
    let at_tag = Command::new("git")
        .args(["describe", "--exact-match", "--tags", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == format!("v{version}"));
    let full = if at_tag || std::env::var("CODEMAP_RELEASE").is_ok_and(|v| v == "1") {
        version
    } else {
        let commit = Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|c| !c.is_empty());
        // A new commit moves the branch ref (or packed-refs), not HEAD: watch
        // all three so the stamp follows every commit.
        let out = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        };
        let branch = out(&["symbolic-ref", "-q", "HEAD"]);
        for p in ["HEAD".to_owned(), "packed-refs".to_owned()]
            .into_iter()
            .chain(branch)
        {
            if let Some(path) = out(&["rev-parse", "--git-path", &p]) {
                println!("cargo:rerun-if-changed={path}");
            }
        }
        match commit {
            Some(c) => format!("{version}-dev+{c}"),
            None => format!("{version}-dev"),
        }
    };
    println!("cargo:rustc-env=CODEMAP_VERSION={full}");
}
