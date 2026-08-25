//! The `xmux update` command: detects how xmux was installed from the running
//! executable's path and delegates to the owning package manager (cargo, winget,
//! Homebrew) when one can be identified. Directly downloading and replacing the
//! binary from a release is intentionally not supported; the package manager (or a
//! fresh install) is always the update path.

use std::path::{Path, PathBuf};

pub struct Args {
    pub check: bool,
    pub method: Option<String>,
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
    if cargo_bins
        .iter()
        .any(|b| exe.starts_with(b) || real.starts_with(b))
    {
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

fn parse_method(s: &str) -> Result<InstallMethod, String> {
    match s {
        "cargo" => Ok(InstallMethod::Cargo),
        "winget" => Ok(InstallMethod::Winget),
        "brew" => Ok(InstallMethod::Brew),
        "self" => Ok(InstallMethod::Self_),
        _ => Err(format!(
            "unknown method {s:?} (expected cargo|winget|brew|self)"
        )),
    }
}

fn resolve_method(forced: Option<&str>) -> Result<InstallMethod, String> {
    if let Some(m) = forced {
        return parse_method(m);
    }
    if let Some(m) = std::env::var("XMUX_UPDATE_METHOD")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return parse_method(&m);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    Ok(classify(&exe, &cargo_bins(), platform()))
}

fn tool_on_path(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn run_delegated(program: &str, args: &[&str]) -> Result<(), String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn run_blocking(args: &Args) -> Result<(), String> {
    let method = resolve_method(args.method.as_deref())?;
    match method {
        InstallMethod::Cargo => {
            if args.check {
                println!("xmux is installed via cargo; update with `cargo install xmux`");
                return Ok(());
            }
            if !tool_on_path("cargo") {
                return Err(
                    "cargo is not on PATH; install a release build for your platform".to_string()
                );
            }
            run_delegated("cargo", &["install", "xmux"])
        }
        InstallMethod::Winget => {
            if args.check {
                println!(
                    "xmux is installed via winget; update with `winget upgrade --id zer0ken.xmux`"
                );
                return Ok(());
            }
            if !tool_on_path("winget") {
                return Err(
                    "winget is not on PATH; install a release build for your platform".to_string()
                );
            }
            run_delegated("winget", &["upgrade", "--id", "zer0ken.xmux"])
        }
        InstallMethod::Brew => {
            if args.check {
                println!(
                    "xmux is installed via Homebrew; update with `brew upgrade zer0ken/xmux/xmux`"
                );
                return Ok(());
            }
            if !tool_on_path("brew") {
                return Err(
                    "brew is not on PATH; install a release build for your platform".to_string()
                );
            }
            run_delegated("brew", &["upgrade", "zer0ken/xmux/xmux"])
        }
        InstallMethod::Self_ => Err(
            "xmux does not update itself; update through the package manager you installed it with (`cargo install xmux`, `winget upgrade`, or `brew upgrade`) or install a fresh release build"
                .to_string(),
        ),
    }
}

/// The public entry: runs the blocking update flow on a worker thread so no blocking
/// network or file work sits on the async runtime path, then maps the outcome to an
/// exit code.
pub async fn run(args: Args) -> i32 {
    match tokio::task::spawn_blocking(move || run_blocking(&args)).await {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            eprintln!("xmux update: {e}");
            1
        }
        Err(_) => {
            eprintln!("xmux update: internal panic");
            1
        }
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
            classify(
                "/home/u/.cargo/bin/xmux",
                &["/home/u/.cargo/bin"],
                Platform::Unix
            ),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn brew_cellar_path_is_brew() {
        assert_eq!(
            classify(
                "/opt/homebrew/Cellar/xmux/0.5.0/bin/xmux",
                &[],
                Platform::Unix
            ),
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
    fn parses_known_method_names() {
        assert_eq!(super::parse_method("cargo").unwrap(), InstallMethod::Cargo);
        assert_eq!(super::parse_method("self").unwrap(), InstallMethod::Self_);
        assert!(super::parse_method("bogus").is_err());
    }
}
