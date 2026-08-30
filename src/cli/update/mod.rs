//! The `xmux update` command. It detects how xmux was installed from the running
//! executable's path and delegates to the owning package manager (cargo, winget,
//! Homebrew); an install no package manager owns is replaced in place with a
//! checksum-verified build from the latest GitHub release. On Windows a running
//! process locks its own image file against deletion and overwrite but not against
//! rename, so the cargo delegation renames the live binary aside first, while the
//! winget delegation and the release swap are handed to a detached updater that
//! waits for every xmux process to exit.

pub mod release;

use std::ffi::OsStr;
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

/// The Cargo bin directories to check: `$CARGO_HOME/bin` when set, plus the
/// profile home's `.cargo/bin` from `HOME` and `USERPROFILE`. Windows processes
/// commonly run without `HOME`, so `USERPROFILE` carries the cargo bin there.
fn cargo_bins() -> Vec<PathBuf> {
    cargo_bins_from(
        std::env::var_os("CARGO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("USERPROFILE").as_deref(),
    )
}

/// Builds the candidates from explicit variables. A parameter (not `cfg`), like
/// `classify`, so detection logic is unit-testable on any host.
fn cargo_bins_from(
    cargo_home: Option<&OsStr>,
    home: Option<&OsStr>,
    profile: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = cargo_home {
        v.push(PathBuf::from(dir).join("bin"));
    }
    if let Some(dir) = home {
        v.push(PathBuf::from(dir).join(".cargo").join("bin"));
    }
    if let Some(dir) = profile {
        v.push(PathBuf::from(dir).join(".cargo").join("bin"));
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
    let p = platform();
    match method {
        InstallMethod::Self_ => release::update(args, p),
        InstallMethod::Cargo => run_cargo(args, p),
        InstallMethod::Winget => run_winget(args, p),
        InstallMethod::Brew => run_brew(args),
    }
}

fn run_cargo(args: &Args, platform: Platform) -> Result<(), String> {
    if args.check {
        println!("xmux is installed via cargo; update with `cargo install xmux`");
        return Ok(());
    }
    if !tool_on_path("cargo") {
        return Err("cargo is not on PATH; install a release build for your platform".to_string());
    }
    // No `--force`: cargo reinstalls on its own when the registry has a newer
    // version and does nothing when the install is current.
    let delegate = || run_delegated("cargo", &["install", "xmux"]);
    match platform {
        Platform::Unix => delegate(),
        // On Windows cargo's final copy into the bin directory would fail on the
        // running binary's image lock, so the binary is renamed aside first. A
        // missing binary makes cargo rebuild even when the install is current,
        // so whether an update exists at all is decided before the rename.
        Platform::Windows => {
            let target =
                std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
            clean_stale_sidecars(&target);
            let current = env!("CARGO_PKG_VERSION");
            let agent = ureq::AgentBuilder::new().build();
            let latest = release::latest_version(&agent)?;
            if !release::is_newer(&latest, current) {
                println!("xmux is already up to date ({current})");
                return Ok(());
            }
            delegate_with_binary_aside(&target, std::process::id(), delegate)
        }
    }
}

/// Runs a package-manager delegation with the live binary renamed to a sidecar
/// name, so the package manager can write the binary's path without hitting the
/// image lock. The sidecar is renamed back when the delegation writes nothing
/// (already up to date, or failed); when a new binary lands, the sidecar (this
/// process's own image, undeletable while it runs) is left for the next update's
/// `clean_stale_sidecars`.
fn delegate_with_binary_aside(
    target: &Path,
    pid: u32,
    delegate: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let sidecar = sidecar_path(target, pid);
    std::fs::rename(target, &sidecar)
        .map_err(|e| format!("cannot move {} aside: {e}", target.display()))?;
    let outcome = delegate();
    if target.exists() {
        return outcome;
    }
    let restored = std::fs::rename(&sidecar, target)
        .map_err(|e| format!("cannot restore {}: {e}", target.display()));
    match (outcome, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(r)) | (Err(r), Ok(())) => Err(r),
        (Err(e), Err(r)) => Err(format!("{e}; {r}")),
    }
}

/// The sidecar a live binary is renamed to while a delegation runs:
/// `<name>.old-<pid>` next to the binary.
fn sidecar_path(target: &Path, pid: u32) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!("{name}.old-{pid}"))
}

/// True when `candidate` is a sidecar an earlier update left next to a binary
/// named `target_name`.
fn is_stale_sidecar(target_name: &str, candidate: &str) -> bool {
    candidate
        .strip_prefix(target_name)
        .and_then(|rest| rest.strip_prefix(".old-"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// Best-effort removal of sidecars left by earlier updates. A sidecar whose old
/// binary still runs somewhere stays locked and is retried on the next update.
fn clean_stale_sidecars(target: &Path) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let name = name.to_string_lossy();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if is_stale_sidecar(&name, &entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn run_winget(args: &Args, platform: Platform) -> Result<(), String> {
    if args.check {
        println!("xmux is installed via winget; update with `winget upgrade --id zer0ken.xmux`");
        return Ok(());
    }
    if !tool_on_path("winget") {
        return Err("winget is not on PATH; install a release build for your platform".to_string());
    }
    match platform {
        Platform::Unix => run_delegated("winget", &["upgrade", "--id", "zer0ken.xmux"]),
        // winget removes the old build's files during an upgrade and that removal
        // fails on a running executable's image lock, so the upgrade is handed to
        // a detached updater that waits for every xmux process to exit.
        Platform::Windows => run_winget_detached(),
    }
}

/// Hands `winget upgrade` to a detached updater and exits; winget's output lands
/// in a log file next to the updater script.
#[cfg(windows)]
fn run_winget_detached() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("xmux-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create staging dir {}: {e}", dir.display()))?;
    let log = dir.join("winget-upgrade.log");
    spawn_detached_cmd(&dir, winget_updater_script(&log))?;
    println!(
        "update handed to winget; it runs once every xmux instance exits (log: {})",
        log.display()
    );
    Ok(())
}

/// Unix stub: never called because `run_winget` only routes here on Windows.
#[cfg(not(windows))]
fn run_winget_detached() -> Result<(), String> {
    unreachable!("Windows-only updater ran on a non-Windows host")
}

/// The detached updater script for a winget delegation: wait until no xmux
/// process runs, then upgrade with output appended to `log`. Production use is
/// the Windows-only detached updater; the test build exercises the script shape
/// on every platform.
#[cfg(any(windows, test))]
fn winget_updater_script(log: &Path) -> String {
    format!(
        "{UPDATER_WAIT_PREAMBLE}\
         winget upgrade --id zer0ken.xmux >> \"{log}\" 2>&1\r\n",
        log = log.display(),
    )
}

/// cmd-script preamble that polls until no xmux process holds an image file.
pub(crate) const UPDATER_WAIT_PREAMBLE: &str = "@echo off\r\n\
    :wait\r\n\
    %SystemRoot%\\System32\\tasklist.exe /FI \"IMAGENAME eq xmux.exe\" | %SystemRoot%\\System32\\find.exe /I \"xmux.exe\" >nul\r\n\
    if errorlevel 1 goto done\r\n\
    %SystemRoot%\\System32\\ping.exe -n 2 127.0.0.1 >nul\r\n\
    goto wait\r\n\
    :done\r\n";

/// Writes `content` as a cmd script in `dir` and launches it hidden and detached,
/// so it outlives the update command.
#[cfg(windows)]
pub(crate) fn spawn_detached_cmd(dir: &Path, content: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let script = dir.join("update.cmd");
    std::fs::write(&script, content).map_err(|e| format!("cannot write updater: {e}"))?;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .arg("/c")
        .arg(&script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("cannot start updater: {e}"))?;
    Ok(())
}

/// Unix stub: never called because every caller routes here only on Windows.
#[cfg(not(windows))]
pub(crate) fn spawn_detached_cmd(_dir: &Path, _content: String) -> Result<(), String> {
    unreachable!("Windows-only updater ran on a non-Windows host")
}

fn run_brew(args: &Args) -> Result<(), String> {
    if args.check {
        println!("xmux is installed via Homebrew; update with `brew upgrade zer0ken/xmux/xmux`");
        return Ok(());
    }
    if !tool_on_path("brew") {
        return Err("brew is not on PATH; install a release build for your platform".to_string());
    }
    run_delegated("brew", &["upgrade", "zer0ken/xmux/xmux"])
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

    #[test]
    fn cargo_bins_cover_profile_home_without_home_var() {
        let bins = super::cargo_bins_from(None, None, Some(OsStr::new("C:\\Users\\u")));
        assert_eq!(
            bins,
            vec![PathBuf::from("C:\\Users\\u").join(".cargo").join("bin")]
        );
    }

    #[test]
    fn sidecar_name_is_target_name_old_pid() {
        let p = super::sidecar_path(Path::new("/bin/dir/xmux.exe"), 42);
        assert_eq!(p, PathBuf::from("/bin/dir/xmux.exe.old-42"));
    }

    #[test]
    fn stale_sidecar_detection_requires_exact_shape() {
        assert!(super::is_stale_sidecar("xmux.exe", "xmux.exe.old-123"));
        assert!(!super::is_stale_sidecar("xmux.exe", "xmux.exe"));
        assert!(!super::is_stale_sidecar("xmux.exe", "xmux.exe.old-"));
        assert!(!super::is_stale_sidecar("xmux.exe", "xmux.exe.old-12a"));
        assert!(!super::is_stale_sidecar("xmux.exe", "other.exe.old-1"));
    }

    #[test]
    fn winget_script_waits_then_upgrades_into_log() {
        let s = super::winget_updater_script(Path::new("C:\\t\\up.log"));
        assert!(s.starts_with(super::UPDATER_WAIT_PREAMBLE));
        assert!(s.contains("winget upgrade --id zer0ken.xmux >> \"C:\\t\\up.log\" 2>&1"));
    }

    fn temp_target(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xmux-sidecar-test-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("xmux.exe");
        std::fs::write(&target, b"old").unwrap();
        target
    }

    #[test]
    fn aside_delegation_keeps_new_binary_and_leaves_sidecar() {
        let target = temp_target("writes");
        let written = target.clone();
        let out = super::delegate_with_binary_aside(&target, 1, move || {
            std::fs::write(&written, b"new").unwrap();
            Ok(())
        });
        assert_eq!(out, Ok(()));
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(super::sidecar_path(&target, 1).exists());
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }

    #[test]
    fn aside_delegation_restores_when_nothing_is_written() {
        let target = temp_target("noop");
        let out = super::delegate_with_binary_aside(&target, 1, || Ok(()));
        assert_eq!(out, Ok(()));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!super::sidecar_path(&target, 1).exists());
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }

    #[test]
    fn aside_delegation_restores_on_failure() {
        let target = temp_target("fails");
        let out = super::delegate_with_binary_aside(&target, 1, || Err("boom".to_string()));
        assert_eq!(out, Err("boom".to_string()));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }

    #[test]
    fn entry_cleanup_removes_only_stale_sidecars() {
        let target = temp_target("cleanup");
        let dir = target.parent().unwrap();
        let stale = dir.join("xmux.exe.old-99");
        let unrelated = dir.join("xmux.exe.bak");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&unrelated, b"x").unwrap();
        super::clean_stale_sidecars(&target);
        assert!(!stale.exists());
        assert!(unrelated.exists());
        assert!(target.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
