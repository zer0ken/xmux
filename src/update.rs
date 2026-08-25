//! The `xmux update` command: detects how xmux was installed from the running
//! executable's path, delegates to the owning package manager (cargo, winget,
//! Homebrew) when one can be identified, and otherwise replaces the binary in
//! place with a checksum-verified build from the latest GitHub release.

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

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)
        .map_err(|e| format!("cannot hash {}: {e}", path.display()))?;
    Ok(format!("{:x}", h.finalize()))
}

fn checksum_matches(expected: String, actual: String) -> Result<(), String> {
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch (expected {expected}, got {actual}); refusing to install"
        ))
    }
}

/// Extracts the `xmux` binary from a release `.tar.gz` archive into `dest`.
#[cfg(not(windows))]
fn extract_binary(tar_gz: &Path, dest: &Path) -> Result<(), String> {
    use std::ffi::OsStr;
    let gz = std::fs::File::open(tar_gz)
        .map_err(|e| format!("cannot open {}: {e}", tar_gz.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(gz));
    let entries = archive.entries().map_err(|e| format!("bad archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("bad archive entry: {e}"))?;
        let name = entry.path().map_err(|e| format!("bad entry path: {e}"))?;
        if name.file_name() == Some(OsStr::new("xmux")) {
            entry
                .unpack(dest)
                .map_err(|e| format!("cannot extract xmux: {e}"))?;
            return Ok(());
        }
    }
    Err("release archive contains no xmux binary".to_string())
}

/// Replaces the running executable's file with `downloaded_bin`.
#[cfg(not(windows))]
fn install(exe: &Path, downloaded_bin: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = exe.parent().ok_or("cannot locate xmux install directory")?;
    let tmp = dir.join("xmux.new");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(downloaded_bin, &tmp)
        .map_err(|e| format!("cannot stage new binary: {e}"))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot mark binary executable: {e}"))?;
    std::fs::rename(&tmp, exe).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "cannot write {} (permission denied); retry with elevated privileges, e.g. `sudo xmux update`",
                exe.display()
            )
        } else {
            format!("cannot replace {}: {e}", exe.display())
        }
    })
}

/// Replaces the running executable on Windows. The running `.exe` cannot be overwritten,
/// so the new binary is staged beside it and a detached `cmd` moves it into place after
/// this process has exited.
#[cfg(windows)]
fn install(exe: &Path, downloaded: &Path) -> Result<(), String> {
    let dir = exe.parent().ok_or("cannot locate xmux install directory")?;
    let new = dir.join("xmux.exe.new");
    let _ = std::fs::remove_file(&new);
    std::fs::copy(downloaded, &new)
        .map_err(|e| format!("cannot stage new binary: {e}"))?;
    let q = |p: &std::path::Path| format!("\"{}\"", p.display());
    let script = format!(
        "timeout /t 1 /nobreak >nul & move /y {} {}",
        q(&new),
        q(exe)
    );
    std::process::Command::new("cmd")
        .args(["/c", "start", "\"\"", "/b", "cmd", "/c", &script])
        .spawn()
        .map_err(|e| format!("cannot schedule replacement: {e}"))?;
    Ok(())
}

use std::time::Duration;

fn http() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build();
    ureq::Agent::new_with_config(cfg)
}

fn latest_release() -> Result<Release, String> {
    let resp = http()
        .get("https://api.github.com/repos/zer0ken/xmux/releases/latest")
        .header("User-Agent", concat!("xmux/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let json: serde_json::Value = serde_json::from_reader(resp.into_body().into_reader())
        .map_err(|e| format!("bad release response: {e}"))?;
    let version = json["tag_name"]
        .as_str()
        .ok_or("release response has no tag_name")?
        .trim_start_matches('v')
        .to_string();
    let tag = json["tag_name"].as_str().unwrap_or("").to_string();
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    Ok(Release {
        version,
        tag,
        assets,
    })
}

fn checksum_for(release: &Release, asset_name: &str) -> Result<String, String> {
    let url = release
        .assets
        .iter()
        .find(|a| a["name"].as_str() == Some("SHA256SUMS"))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or("release has no SHA256SUMS asset")?;
    let body = http()
        .get(url)
        .header("User-Agent", concat!("xmux/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("could not fetch SHA256SUMS: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("bad SHA256SUMS body: {e}"))?;
    checksum_for_name(&body, asset_name)
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let resp = http()
        .get(url)
        .header("User-Agent", concat!("xmux/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut out = std::fs::File::create(dest)
        .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    let mut reader = resp.into_body().into_reader();
    std::io::copy(&mut reader, &mut out)
        .map_err(|e| format!("download interrupted: {e}"))?;
    Ok(())
}

/// The release-asset suffix for this platform, matching the release workflow's naming.
fn asset_suffix() -> &'static str {
    #[cfg(windows)]
    {
        "x86_64-pc-windows-msvc.exe"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu.tar.gz"
    }
    #[cfg(not(any(
        windows,
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        compile_error!("xmux update: unsupported platform for release assets")
    }
}

/// Downloads, verifies, and installs the latest release over the running binary.
fn self_update(check: bool) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate own binary: {e}"))?;
    let cur = env!("CARGO_PKG_VERSION");
    let cur_v = parse_version(cur).ok_or(
        "current build is not a released version (a source/dev build); update it the same way it was built",
    )?;

    let release = latest_release()?;
    let latest_v = parse_version(&release.version)
        .ok_or("latest release has an unparseable version")?;
    if latest_v <= cur_v {
        println!("xmux is already up to date (v{cur})");
        return Ok(());
    }

    let suffix = asset_suffix();
    let (asset_name, url) = asset_for(&release, suffix)
        .ok_or_else(|| format!("latest release has no asset matching {suffix}"))?;
    if check {
        println!(
            "update available: v{cur} -> v{} ({asset_name})",
            release.version
        );
        return Ok(());
    }

    println!("updating xmux v{cur} -> v{}", release.version);
    let expected = checksum_for(&release, asset_name)?;

    let dir = std::env::temp_dir();
    #[cfg(windows)]
    let download_name = format!("xmux-{}.exe", release.version);
    #[cfg(not(windows))]
    let download_name = format!("xmux-{}.tar.gz", release.version);
    let dl = dir.join(download_name);
    let _ = std::fs::remove_file(&dl);
    download(url, &dl)?;
    checksum_matches(expected, sha256_file(&dl)?)?;

    #[cfg(windows)]
    {
        install(&exe, &dl)?;
    }
    #[cfg(not(windows))]
    {
        let bin = dir.join(format!("xmux-{}", release.version));
        let _ = std::fs::remove_file(&bin);
        extract_binary(&dl, &bin)?;
        install(&exe, &bin)?;
    }
    println!("updated to v{}", release.version);
    Ok(())
}

fn parse_method(s: &str) -> Result<InstallMethod, String> {
    match s {
        "cargo" => Ok(InstallMethod::Cargo),
        "winget" => Ok(InstallMethod::Winget),
        "brew" => Ok(InstallMethod::Brew),
        "self" => Ok(InstallMethod::Self_),
        _ => Err(format!("unknown method {s:?} (expected cargo|winget|brew|self)")),
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
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate own binary: {e}"))?;
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
    let status = cmd.status().map_err(|e| format!("cannot run {program}: {e}"))?;
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
                return Err("cargo is not on PATH; use `xmux update --method self`".to_string());
            }
            run_delegated("cargo", &["install", "xmux"])
        }
        InstallMethod::Winget => {
            if args.check {
                println!("xmux is installed via winget; update with `winget upgrade --id zer0ken.xmux`");
                return Ok(());
            }
            if !tool_on_path("winget") {
                return Err("winget is not on PATH; use `xmux update --method self`".to_string());
            }
            run_delegated("winget", &["upgrade", "--id", "zer0ken.xmux"])
        }
        InstallMethod::Brew => {
            if args.check {
                println!("xmux is installed via Homebrew; update with `brew upgrade zer0ken/xmux/xmux`");
                return Ok(());
            }
            if !tool_on_path("brew") {
                return Err("brew is not on PATH; use `xmux update --method self`".to_string());
            }
            run_delegated("brew", &["upgrade", "zer0ken/xmux/xmux"])
        }
        InstallMethod::Self_ => self_update(args.check),
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

    #[test]
    fn sha256_of_a_file_matches_expected() {
        let dir = std::env::temp_dir();
        let path = dir.join("xmux-sha-test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(
            super::sha256_file(&path).unwrap(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        assert!(super::checksum_matches("expected".to_string(), "expected".to_string()).is_ok());
        assert!(super::checksum_matches("expected".to_string(), "actual".to_string()).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn extract_binary_pulls_xmux_out_of_tar_gz() {
        let dir = std::env::temp_dir();
        let src = dir.join("xmux-extract-src.txt");
        std::fs::write(&src, b"fake-binary").unwrap();
        let tgz = dir.join("xmux-extract.tar.gz");
        let file = std::fs::File::create(&tgz).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut ar = tar::Builder::new(enc);
        ar.append_path_with_name(&src, "xmux").unwrap();
        ar.into_inner().unwrap().finish().unwrap();

        let out = dir.join("xmux-extracted-bin");
        let _ = std::fs::remove_file(&out);
        super::extract_binary(&tgz, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"fake-binary");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&tgz);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn asset_suffix_maps_to_release_naming() {
        let s = super::asset_suffix();
        assert!(
            s == "x86_64-pc-windows-msvc.exe"
                || s == "aarch64-apple-darwin.tar.gz"
                || s == "x86_64-apple-darwin.tar.gz"
                || s == "x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn parses_known_method_names() {
        assert_eq!(super::parse_method("cargo").unwrap(), InstallMethod::Cargo);
        assert_eq!(super::parse_method("self").unwrap(), InstallMethod::Self_);
        assert!(super::parse_method("bogus").is_err());
    }
}
