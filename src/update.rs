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
}
