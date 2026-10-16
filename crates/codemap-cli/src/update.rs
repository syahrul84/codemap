//! `codemap update`: replaces this program with the latest release, after
//! checking it against the release's SHA256SUMS. Also the once-a-day check
//! behind the "new version" notice. Uses the system's `curl` and `tar`, which
//! macOS, Linux and WSL have.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use codemap_core::updates::{self, REPO, VERSION};
use sha2::{Digest as _, Sha256};

/// The release file for this machine.
fn asset() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("codemap-aarch64-apple-darwin.tar.gz")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("codemap-x86_64-apple-darwin.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("codemap-x86_64-unknown-linux-gnu.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("codemap-aarch64-unknown-linux-gnu.tar.gz")
    } else {
        None
    }
}

fn curl(url: &str, timeout_s: u32) -> anyhow::Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", &timeout_s.to_string()])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", &format!("User-Agent: codemap/{VERSION}")])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .context("`curl` is needed to download updates and was not found")?;
    if !out.status.success() {
        bail!(
            "downloading {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// The latest release's tag, e.g. `v0.2.0`.
pub fn latest(timeout_s: u32) -> anyhow::Result<String> {
    let body = curl(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        timeout_s,
    )?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).context("unexpected reply from GitHub")?;
    v["tag_name"]
        .as_str()
        .map(str::to_owned)
        .context("GitHub's reply has no release tag")
}

/// Asks GitHub when the last check is a day old; remembers the answer (or
/// the failure, so an offline machine is not asked again for a day).
pub fn check_if_due(data_dir: &Path, timeout_s: u32) {
    if updates::disabled() || !updates::due(data_dir) {
        return;
    }
    let previous = updates::load(data_dir).and_then(|c| c.latest);
    match latest(timeout_s) {
        Ok(tag) => updates::save(data_dir, Some(&tag)),
        Err(e) => {
            tracing::debug!("update check failed: {e:#}");
            updates::save(data_dir, previous.as_deref());
        }
    }
}

/// The same, without delaying the caller.
pub fn check_in_background(data_dir: PathBuf) {
    if updates::disabled() || !updates::due(&data_dir) {
        return;
    }
    std::thread::spawn(move || check_if_due(&data_dir, 10));
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn run(data_dir: &Path, check_only: bool, force: bool) -> anyhow::Result<()> {
    let tag = latest(20)?;
    updates::save(data_dir, Some(&tag));
    let new = tag.trim_start_matches('v');
    if !updates::is_newer(&tag, VERSION) {
        if !force || check_only {
            println!("codemap {VERSION} is the latest version.");
            return Ok(());
        }
        println!("Reinstalling codemap {new}.");
    } else {
        println!("codemap {new} is available (this is {VERSION}).");
    }
    println!("Release notes: https://github.com/{REPO}/releases/tag/{tag}");
    if check_only {
        println!("Run `codemap update` to install it.");
        return Ok(());
    }
    if updates::in_docker() {
        println!(
            "This copy runs in Docker. To update: in your codemap folder run `git pull` and `docker build -t codemap:latest --target runtime .`"
        );
        return Ok(());
    }
    let Some(asset) = asset() else {
        bail!(
            "there is no ready-made codemap for this system; build it with `cargo install --locked --git https://github.com/{REPO} --tag {tag} codemap-cli`"
        );
    };

    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let archive = curl(&format!("{base}/{asset}"), 300)?;
    let sums = String::from_utf8(curl(&format!("{base}/SHA256SUMS"), 30)?)?;
    let expected = sums
        .lines()
        .find_map(|l| l.split_once("  ").filter(|(_, f)| f.trim() == asset))
        .map(|(h, _)| h.trim().to_owned())
        .with_context(|| format!("SHA256SUMS has no line for {asset}"))?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        bail!(
            "the download does not match its checksum ({actual} ≠ {expected}); nothing was changed"
        );
    }

    // Unpack next to nothing important, then check the new program runs.
    let work = std::env::temp_dir().join(format!("codemap-update-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let tarball = work.join(asset);
    std::fs::write(&tarball, &archive)?;
    let ok = Command::new("tar")
        .arg("xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&work)
        .status()
        .context("`tar` is needed to unpack the update and was not found")?
        .success();
    let unpacked = work.join("codemap");
    if !ok || !unpacked.is_file() {
        bail!("could not unpack {asset}");
    }
    let version = Command::new(&unpacked).arg("--version").output()?;
    let said = String::from_utf8_lossy(&version.stdout);
    if !said.contains(new) {
        bail!(
            "the downloaded program reports `{}`, expected {new}; nothing was changed",
            said.trim()
        );
    }

    // Replace this program: write beside it, then rename over it (the
    // running copy keeps working until it exits).
    let exe = std::env::current_exe()?.canonicalize()?;
    let staged = exe.with_extension("new");
    let replace = || -> std::io::Result<()> {
        std::fs::copy(&unpacked, &staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&staged, &exe)
    };
    let result = replace();
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&staged);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "cannot replace {}: permission denied. Run `sudo codemap update`, or install it into a folder you own (see the README)",
                exe.display()
            );
        }
        bail!("cannot replace {}: {e}", exe.display());
    }
    println!(
        "Updated {} to {new}. Claude Code uses it from its next session (or restart the codemap server).",
        exe.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn checksum_is_sha256_hex() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
