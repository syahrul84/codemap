//! `codemap update`: replaces this program with the latest release, after
//! checking it against the release's SHA256SUMS. Also the once-a-day check
//! behind the "new version" notice. Uses the system's `curl` and `tar`, which
//! macOS, Linux, WSL and Windows 10 and later have.

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
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("codemap-x86_64-pc-windows-msvc.zip")
    } else {
        None
    }
}

/// A system tool: on Windows the one in `System32` (`tar.exe` there reads
/// .zip; a GNU `tar` from Git for Windows earlier in `PATH` does not).
fn tool(name: &str) -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let p = PathBuf::from(root)
            .join("System32")
            .join(format!("{name}.exe"));
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(name)
}

fn curl(url: &str, timeout_s: u32) -> anyhow::Result<Vec<u8>> {
    let out = Command::new(tool("curl"))
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

/// `C:\\x` for `\\?\C:\\x` (what `canonicalize` gives on Windows).
fn shown(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}

/// The previous program, kept beside the new one on Windows (a running
/// program can be renamed there, not replaced).
fn old_copy(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Removes the copy a previous update left (Windows). Fails quietly while
/// that copy is still running.
pub fn clean_old_copy() {
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::fs::remove_file(old_copy(&exe));
    }
}

/// Puts `new` where `exe` is. On Unix, a rename over the running program
/// (it keeps running until it exits); on Windows, the running program is
/// moved aside first.
fn replace_exe(new: &Path, exe: &Path) -> std::io::Result<()> {
    let staged = exe.with_extension("new");
    let result = (|| {
        std::fs::copy(new, &staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        if cfg!(windows) {
            let old = old_copy(exe);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(exe, &old)?;
            if let Err(e) = std::fs::rename(&staged, exe) {
                // Put the working program back.
                let _ = std::fs::rename(&old, exe);
                return Err(e);
            }
            Ok(())
        } else {
            std::fs::rename(&staged, exe)
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

pub fn run(data_dir: &Path, check_only: bool, force: bool) -> anyhow::Result<()> {
    clean_old_copy();
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
    // `-xf` reads .tar.gz with GNU tar and bsdtar, and .zip with bsdtar
    // (Windows' `tar.exe`).
    let ok = Command::new(tool("tar"))
        .arg("-xf")
        .arg(&tarball)
        .arg("-C")
        .arg(&work)
        .status()
        .context("`tar` is needed to unpack the update and was not found")?
        .success();
    let unpacked = work.join(format!("codemap{}", std::env::consts::EXE_SUFFIX));
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
    let result = replace_exe(&unpacked, &exe);
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = result {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            if cfg!(windows) {
                bail!(
                    "cannot replace {}: permission denied. Run the terminal as administrator, or install codemap into a folder you own (see the README)",
                    shown(&exe)
                );
            }
            bail!(
                "cannot replace {}: permission denied. Run `sudo codemap update`, or install it into a folder you own (see the README)",
                shown(&exe)
            );
        }
        bail!("cannot replace {}: {e}", shown(&exe));
    }
    println!(
        "Updated {} to {new}. Claude Code uses it from its next session (or restart the codemap server).",
        shown(&exe)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The program is replaced in place, also while "running" (on Windows the
    /// old copy is moved aside, and cleaned up later).
    #[test]
    fn replaces_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir
            .path()
            .join(format!("codemap{}", std::env::consts::EXE_SUFFIX));
        let new = dir.path().join("download.bin");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        replace_exe(&new, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(!exe.with_extension("new").exists());
        assert_eq!(old_copy(&exe).exists(), cfg!(windows));
    }

    #[test]
    fn checksum_is_sha256_hex() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
