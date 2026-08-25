//! The `xmux update` command: detects how xmux was installed from the running
//! executable's path, delegates to the owning package manager (cargo, winget,
//! Homebrew) when one can be identified, and otherwise replaces the binary in
//! place with a checksum-verified build from the latest GitHub release.

use std::path::{Path, PathBuf};

pub struct Args {
    pub check: bool,
    pub method: Option<String>,
}

pub async fn run(_args: Args) -> i32 {
    eprintln!("xmux update: not implemented");
    1
}

/// Which OS family the binary runs on. A parameter (not `cfg`) so detection logic
/// is unit-testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Unix,
}

/// How the installed xmux is owned, decided from the executable's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Cargo,
    Winget,
    Brew,
    Self_,
}

const WIN_WINGET_MARKERS: &[&str] = &["microsoft\\winget", "microsoft/winget"];
const BREW_MARKERS: &[&str] = &["/cellar/", "/homebrew/", "/home/linuxbrew/"];

/// Decides the install method from the executable path. Resolves symlinks first so a
/// Homebrew `/usr/local/bin/xmux` symlink (into `Cellar/`) is read as brew, not as a
/// bare prebuilt. `cargo_bins` are the Cargo bin directories to check containment in.
fn classify(exe: &Path, cargo_bins: &[PathBuf], platform: Platform) -> InstallMethod {
    let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let p = real.to_string_lossy().to_lowercase();
    match platform {
        Platform::Windows => {
            if WIN_WINGET_MARKERS.iter().any(|m| p.contains(m)) {
                return InstallMethod::Winget;
            }
        }
        Platform::Unix => {
            if BREW_MARKERS.iter().any(|m| p.contains(m)) {
                return InstallMethod::Brew;
            }
        }
    }
    if cargo_bins.iter().any(|b| exe.starts_with(b) || real.starts_with(b)) {
        return InstallMethod::Cargo;
    }
    InstallMethod::Self_
}

/// The Cargo bin directories to check: `$CARGO_HOME/bin` if set, else `~/.cargo/bin`.
fn cargo_bins() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        v.push(PathBuf::from(home).join("bin"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join(".cargo").join("bin"));
    }
    v
}

/// The running platform, used by detection at runtime.
fn platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Unix
    }
}

/// A `major.minor.patch` release version, parsed strictly (a trailing `-dev`, extra
/// segment, or non-numeric input is rejected — those are not released builds).
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim_start_matches('v');
    let mut it = v.split('.');
    let major: u64 = it.next()?.parse().ok()?;
    let minor: u64 = it.next()?.parse().ok()?;
    let patch: u64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// The latest release's version, tag, and asset list (from the GitHub API).
struct Release {
    version: String,
    #[allow(dead_code)]
    tag: String,
    assets: Vec<serde_json::Value>,
}

/// The asset (name, download URL) whose name ends with `suffix`.
fn asset_for<'a>(release: &'a Release, suffix: &str) -> Option<(&'a str, &'a str)> {
    for a in &release.assets {
        let name = a["name"].as_str()?;
        if name.ends_with(suffix) {
            return Some((name, a["browser_download_url"].as_str()?));
        }
    }
    None
}

/// The sha256 for `file_name` out of a `SHA256SUMS` body (`<hash>  <file>` per line).
fn checksum_for_name(body: &str, file_name: &str) -> Result<String, String> {
    body.lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let hash = it.next()?.to_string();
            if it.next()? == file_name {
                Some(hash)
            } else {
                None
            }
        })
        .ok_or_else(|| format!("SHA256SUMS has no entry for {file_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn classify(p: &str, cargo_bins: &[&str], platform: Platform) -> InstallMethod {
        let bins: Vec<PathBuf> = cargo_bins.iter().map(PathBuf::from).collect();
        super::classify(Path::new(p), &bins, platform)
    }

    #[test]
    fn cargo_bin_path_is_cargo() {
        assert_eq!(
            classify("/home/u/.cargo/bin/xmux", &["/home/u/.cargo/bin"], Platform::Unix),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn brew_cellar_path_is_brew() {
        assert_eq!(
            classify("/opt/homebrew/Cellar/xmux/0.5.0/bin/xmux", &[], Platform::Unix),
            InstallMethod::Brew
        );
    }

    #[test]
    fn winget_links_shim_is_winget() {
        assert_eq!(
            classify(
                "C:\\Users\\u\\AppData\\Local\\Microsoft\\WinGet\\Links\\xmux.exe",
                &[],
                Platform::Windows
            ),
            InstallMethod::Winget
        );
    }

    #[test]
    fn plain_path_falls_back_to_self() {
        assert_eq!(
            classify("/usr/local/bin/xmux", &[], Platform::Unix),
            InstallMethod::Self_
        );
    }

    #[test]
    fn parses_x_y_z_versions() {
        assert_eq!(parse_version("0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_non_release_versions() {
        assert_eq!(parse_version("0.5.0-dev"), None);
        assert_eq!(parse_version("0.5"), None);
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn asset_for_matches_the_target_suffix() {
        let release = Release {
            version: "0.5.1".to_string(),
            tag: "v0.5.1".to_string(),
            assets: vec![
                json_asset("xmux-v0.5.1-x86_64-unknown-linux-gnu.tar.gz", "u1"),
                json_asset("xmux-v0.5.1-x86_64-pc-windows-msvc.exe", "u2"),
            ],
        };
        let (name, url) = asset_for(&release, "x86_64-pc-windows-msvc.exe").unwrap();
        assert_eq!(name, "xmux-v0.5.1-x86_64-pc-windows-msvc.exe");
        assert_eq!(url, "u2");
        assert!(asset_for(&release, "aarch64-apple-darwin.tar.gz").is_none());
    }

    #[test]
    fn checksum_line_parses_by_filename() {
        let sums =
            "abc  xmux-v0.5.1-x86_64-unknown-linux-gnu.tar.gz\ndef  SHA256SUMS\n";
        assert_eq!(
            checksum_for_name(sums, "xmux-v0.5.1-x86_64-unknown-linux-gnu.tar.gz")
                .as_deref(),
            Ok("abc")
        );
        assert!(checksum_for_name(sums, "missing.tar.gz").is_err());
    }

    fn json_asset(name: &str, url: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "browser_download_url": url })
    }
}
