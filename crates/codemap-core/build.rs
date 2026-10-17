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
        if let Ok(head) = Command::new("git")
            .args(["rev-parse", "--git-path", "HEAD"])
            .output()
            && head.status.success()
        {
            let p = String::from_utf8_lossy(&head.stdout).trim().to_owned();
            println!("cargo:rerun-if-changed={p}");
        }
        match commit {
            Some(c) => format!("{version}-dev+{c}"),
            None => format!("{version}-dev"),
        }
    };
    println!("cargo:rustc-env=CODEMAP_VERSION={full}");
}
