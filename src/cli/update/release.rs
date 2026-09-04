//! Direct self-update from GitHub Releases. `update` queries the latest release,
//! downloads the build for the running platform, verifies its SHA-256 against the
//! release's `SHA256SUMS`, and replaces the running binary. The replacement is
//! platform-aware: on Unix a running executable may be renamed over, so the swap is
//! a single rename; on Windows a running process locks its own image file, so the
//! swap is handed to a detached updater that waits for every xmux process to exit
//! and only then copies the staged build over the live binary.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::Platform;

const LATEST_API: &str = "https://api.github.com/repos/zer0ken/xmux/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/zer0ken/xmux/releases/download";

/// The release build asset name for a version, OS, and architecture. `None` when
/// the running platform has no published build. Mirrors the release workflow's
/// asset naming exactly.
pub fn asset_name(version: &str, os: &str, arch: &str) -> Option<String> {
    let target = match (os, arch) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc.exe",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu.tar.gz",
        ("macos", "aarch64") => "aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "x86_64-apple-darwin.tar.gz",
        _ => return None,
    };
    Some(format!("xmux-v{version}-{target}"))
}

/// True when `a` is a newer `x.y.z` version than `b`. Leading `v` is tolerated and
/// non-numeric segments compare as `0`, so `1.10.0 > 1.9.0` and `v1.2.0 == 1.2.0`.
pub fn is_newer(a: &str, b: &str) -> bool {
    parse(a).cmp(&parse(b)) == std::cmp::Ordering::Greater
}

fn parse(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split('.')
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

/// Parses a release `SHA256SUMS` body (`<hex>  <asset>`) into asset -> digest.
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let digest = it.next()?;
            let name = it.next()?;
            Some((name.to_string(), digest.to_string()))
        })
        .collect()
}

/// SHA-256 hex digest of a file's contents.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut reader = std::io::BufReader::new(file);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Runs `curl` with `args`, returning stdout on success and curl's stderr on
/// failure. `-f` turns HTTP errors into a non-zero exit, so a 404 or 5xx page
/// never lands in the returned bytes as if it were a release.
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "120"])
        .args(args)
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub(crate) fn latest_version() -> Result<String, String> {
    let text = String::from_utf8(
        curl(&[LATEST_API]).map_err(|e| format!("cannot query the latest release: {e}"))?,
    )
    .map_err(|e| format!("cannot read the latest release: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("cannot parse the latest release: {e}"))?;
    json["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or_else(|| "the latest release has no version tag".to_string())
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    let dest_arg = dest
        .to_str()
        .ok_or_else(|| format!("cannot write {}: path is not UTF-8", dest.display()))?;
    curl(&["-o", dest_arg, url])
        .map(|_| ())
        .map_err(|e| format!("cannot download {url}: {e}"))
}

/// Download URL for a release asset.
fn download_url(version: &str, asset: &str) -> String {
    format!("{DOWNLOAD_BASE}/v{version}/{asset}")
}

fn staging_dir() -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!("xmux-update-{}", std::process::id()));
    d
}

/// Replaces the running binary with the staged build.
fn replace_binary(staged: &Path, target: &Path, platform: Platform) -> Result<(), String> {
    match platform {
        Platform::Unix => std::fs::rename(staged, target)
            .map_err(|e| format!("cannot replace {}: {e}", target.display())),
        Platform::Windows => spawn_detached_updater(staged, target),
    }
}

/// On Windows the running binary cannot be renamed over, so a detached updater is
/// launched: a `cmd` script (not `xmux`) that polls until no xmux process holds the
/// image file, copies the staged build over the live binary, then cleans up. The
/// updater outlives the update command, which exits immediately after spawning it.
fn spawn_detached_updater(staged: &Path, target: &Path) -> Result<(), String> {
    let dir = staged.parent().unwrap_or_else(|| Path::new("."));
    let content = format!(
        "{preamble}\
         copy /Y \"{staged}\" \"{target}\" >nul\r\n\
         del /Q \"{staged}\" >nul 2>&1\r\n\
         rmdir /S /Q \"{dir}\" >nul 2>&1\r\n",
        preamble = super::UPDATER_WAIT_PREAMBLE,
        staged = staged.display(),
        target = target.display(),
        dir = dir.display(),
    );
    super::spawn_detached_cmd(dir, content)
}

fn extract_tar(archive: &Path, dest_dir: &Path) -> Result<(), String> {
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest_dir)
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tar exited with {status}"))
    }
}

/// The `xmux update` release path: report (`--check`) or perform a checksum-verified
/// upgrade of the running binary.
pub fn update(args: &super::Args, platform: Platform) -> Result<(), String> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = latest_version()?;
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let asset =
        asset_name(&latest, os, arch).ok_or_else(|| format!("no release build for {os}/{arch}"))?;

    if args.check {
        if is_newer(&latest, current) {
            println!(
                "xmux {current} is installed; latest is {latest} - run `xmux update` to upgrade"
            );
        } else {
            println!("xmux is up to date ({current})");
        }
        return Ok(());
    }

    if !is_newer(&latest, current) {
        println!("xmux is already up to date ({current})");
        return Ok(());
    }

    let dir = staging_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create staging dir {}: {e}", dir.display()))?;
    let archive = dir.join(&asset);
    let url = download_url(&latest, &asset);
    println!("downloading {asset} …");
    if let Err(e) = download_to(&url, &archive) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }

    let sums = fetch_checksums(&latest)?;
    let expected = sums
        .get(&asset)
        .ok_or_else(|| format!("no checksum recorded for {asset}"))?;
    let actual = sha256_file(&archive)?;
    if &actual != expected {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    println!("checksum verified ({actual})");

    let staged_bin = if platform == Platform::Windows {
        archive.clone()
    } else {
        extract_tar(&archive, &dir)?;
        dir.join("xmux")
    };

    let target = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    println!("installing {latest} → {}", target.display());
    replace_binary(&staged_bin, &target, platform)?;

    if platform == Platform::Windows {
        println!("xmux will swap in the new build once all xmux instances exit");
    } else {
        println!("updated xmux to {latest}");
    }
    Ok(())
}

fn fetch_checksums(version: &str) -> Result<HashMap<String, String>, String> {
    let url = format!("{DOWNLOAD_BASE}/v{version}/SHA256SUMS");
    let text =
        String::from_utf8(curl(&[&url]).map_err(|e| format!("cannot download checksums: {e}"))?)
            .map_err(|e| format!("cannot read checksums: {e}"))?;
    Ok(parse_checksums(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_match_release_workflow() {
        assert_eq!(
            asset_name("0.6.4", "windows", "x86_64").unwrap(),
            "xmux-v0.6.4-x86_64-pc-windows-msvc.exe"
        );
        assert_eq!(
            asset_name("0.6.4", "linux", "x86_64").unwrap(),
            "xmux-v0.6.4-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name("0.6.4", "macos", "aarch64").unwrap(),
            "xmux-v0.6.4-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name("0.6.4", "macos", "x86_64").unwrap(),
            "xmux-v0.6.4-x86_64-apple-darwin.tar.gz"
        );
        assert!(asset_name("0.6.4", "windows", "aarch64").is_none());
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(is_newer("0.7.0", "0.6.4"));
        assert!(!is_newer("0.6.4", "0.6.4"));
        assert!(!is_newer("v1.2.0", "v1.2.1"));
        assert!(!is_newer("0.6.4", "0.7.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
    }

    #[test]
    fn parses_sha256sums() {
        let text = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  xmux-v0.6.4-x86_64-pc-windows-msvc.exe\nabcdef  other.txt\n";
        let sums = parse_checksums(text);
        assert_eq!(
            sums.get("xmux-v0.6.4-x86_64-pc-windows-msvc.exe")
                .map(|s| s.as_str()),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert_eq!(sums.get("other.txt").map(|s| s.as_str()), Some("abcdef"));
    }

    #[test]
    fn sha256_of_known_input() {
        let dir = std::env::temp_dir().join(format!("xmux-sha-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("hello.txt");
        std::fs::write(&f, "hello").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_url_is_versioned() {
        assert_eq!(
            download_url("0.6.4", "xmux-v0.6.4-x86_64-pc-windows-msvc.exe"),
            "https://github.com/zer0ken/xmux/releases/download/v0.6.4/xmux-v0.6.4-x86_64-pc-windows-msvc.exe"
        );
    }

    /// Real Windows-only behavior check: the detached updater must copy the staged
    /// build over the target. Ignored by default because it waits for no xmux
    /// process to be running and touches real process state; run explicitly with
    /// `cargo test -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn detached_updater_swaps_binary() {
        let base = std::env::temp_dir().join(format!("xmux-upd-test-{}", std::process::id()));
        let staged_dir = base.join("stage");
        let target_dir = base.join("target");
        std::fs::create_dir_all(&staged_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let staged = staged_dir.join("new.exe");
        let target = target_dir.join("target.exe");
        std::fs::write(&staged, b"new-binary").unwrap();
        std::fs::write(&target, b"old-binary").unwrap();
        super::spawn_detached_updater(&staged, &target).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut done = false;
        while std::time::Instant::now() < deadline {
            if std::fs::read(&target)
                .map(|b| b == b"new-binary")
                .unwrap_or(false)
            {
                done = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(done, "updater did not swap the target");
        assert!(!staged_dir.exists(), "staging dir should be cleaned up");
        let _ = std::fs::remove_dir_all(&base);
    }
}
