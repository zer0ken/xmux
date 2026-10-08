//! Startup update consent, device preference, and handover to the installed build.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::{InstallMethod, Platform};

const HANDOVER: &str = "XMUX_STARTUP_UPDATED";
const PREFERENCE: &str = "auto-update";

#[derive(Debug, PartialEq, Eq)]
enum Choice {
    Automatic,
    Once,
    Skip,
}

fn prompt(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<Choice> {
    writeln!(output, "1. Update and enable automatic updates (default)")?;
    writeln!(output, "2. Update once")?;
    writeln!(output, "3. Run without updating")?;
    loop {
        write!(output, "Select [1]: ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Choice::Skip);
        }
        match line.trim() {
            "" | "1" => return Ok(Choice::Automatic),
            "2" => return Ok(Choice::Once),
            "3" => return Ok(Choice::Skip),
            _ => writeln!(output, "Enter 1, 2, or 3.")?,
        }
    }
}

fn automatic(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(PREFERENCE)).is_ok_and(|s| s.trim() == "true")
}

pub(crate) async fn run(dir: &Path) -> Option<i32> {
    // Consume the marker so sessions launched inside xmux still check for updates.
    if std::env::var_os(HANDOVER).is_some() {
        std::env::remove_var(HANDOVER);
        return None;
    }
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let dir = dir.to_path_buf();
    match tokio::task::spawn_blocking(move || prepare(&dir, interactive)).await {
        Ok(Ok(Some(exe))) => match tokio::task::spawn_blocking(move || restart(&exe)).await {
            Ok(Ok(code)) => Some(code),
            Ok(Err(e)) => {
                eprintln!("xmux update: {e}; starting the current build");
                None
            }
            Err(e) => {
                eprintln!("xmux update: {e}; starting the current build");
                None
            }
        },
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            eprintln!("xmux update: {e}; starting the current build");
            None
        }
        Err(e) => {
            eprintln!("xmux update: {e}; starting the current build");
            None
        }
    }
}

fn prepare(dir: &Path, interactive: bool) -> Result<Option<PathBuf>, String> {
    let cfg = crate::provision::config::load(&crate::provision::env::config_path())
        .map_err(|e| e.to_string())?;
    let auto = automatic(dir);
    if !cfg.update.check || (!interactive && !auto) {
        return Ok(None);
    }
    println!("Checking for xmux updates...");
    let latest = super::notify::refresh(dir)?;
    if !super::release::is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    println!(
        "xmux {latest} is available (running {}).",
        env!("CARGO_PKG_VERSION")
    );
    let choice = if auto {
        Choice::Once
    } else {
        prompt(&mut io::stdin().lock(), &mut io::stdout().lock()).map_err(|e| e.to_string())?
    };
    if choice == Choice::Skip {
        return Ok(None);
    }
    if choice == Choice::Automatic {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(PREFERENCE), "true\n")
            .map_err(|e| format!("cannot save automatic update preference: {e}"))?;
    }
    install(&latest).map(Some)
}

fn install(latest: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (method, forced) = super::resolve_method(None)?;
    let args = super::Args {
        check: false,
        method: None,
        version: Some(latest.into()),
    };
    let launcher = launcher_path(&exe);
    let mut restart = if method == InstallMethod::Script {
        super::script_root(&exe)
            .ok_or("cannot locate script install root")?
            .join("versions")
            .join(latest)
            .join(if cfg!(windows) { "xmux.exe" } else { "xmux" })
    } else {
        launcher.clone().unwrap_or_else(|| exe.clone())
    };
    match method {
        InstallMethod::Cargo if !forced => {
            super::release::update_at_startup(&args, super::platform())?;
        }
        InstallMethod::Self_ => {
            super::release::update_at_startup(&args, super::platform())?;
        }
        InstallMethod::Winget if super::platform() == Platform::Windows => {
            super::clean_stale_sidecars(&exe);
            super::delegate_with_binary_aside(&exe, std::process::id(), || {
                super::run_delegated(
                    "winget",
                    &["upgrade", "--id", "zer0ken.xmux", "--disable-interactivity"],
                )
            })?;
        }
        _ => super::run_blocking(&args)?,
    }
    if method == InstallMethod::Brew {
        let prefix = std::process::Command::new("brew")
            .args(["--prefix", "zer0ken/xmux/xmux"])
            .output()
            .map_err(|e| format!("cannot locate Homebrew install: {e}"))?;
        if !prefix.status.success() {
            return Err("cannot locate Homebrew install".into());
        }
        restart = PathBuf::from(String::from_utf8_lossy(&prefix.stdout).trim()).join("bin/xmux");
    }
    let output = std::process::Command::new(&restart)
        .arg("version")
        .output()
        .map_err(|e| format!("cannot verify installed build: {e}"))?;
    let version = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || !version
            .trim()
            .strip_prefix("xmux ")
            .is_some_and(|v| super::release::is_newer(v, env!("CARGO_PKG_VERSION")))
    {
        return Err("the installer did not provide a newer runnable build".into());
    }
    Ok(restart)
}

/// Keep the launcher's path, since package managers can change its versioned target.
pub(super) fn launcher_path(exe: &Path) -> Option<PathBuf> {
    let invoked = PathBuf::from(std::env::args_os().next()?);
    let mut candidates = vec![invoked];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(
            std::env::split_paths(&path)
                .map(|dir| dir.join(if cfg!(windows) { "xmux.exe" } else { "xmux" })),
        );
    }
    let real = exe.canonicalize().ok()?;
    let versions = super::script_root(exe).map(|root| root.join("versions"));
    candidates.into_iter().find(|candidate| {
        let absolute = std::path::absolute(candidate).ok();
        !versions.as_ref().is_some_and(|versions| {
            absolute
                .as_ref()
                .is_some_and(|path| path.starts_with(versions))
        }) && candidate.canonicalize().is_ok_and(|p| p == real)
    })
}

fn restart(exe: &Path) -> Result<i32, String> {
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1)).env(HANDOVER, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("cannot start updated xmux: {}", command.exec()))
    }
    #[cfg(not(unix))]
    {
        command
            .status()
            .map(|s| s.code().unwrap_or(1))
            .map_err(|e| format!("cannot start updated xmux: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_enables_automatic_updates_but_eof_skips() {
        for (input, expected) in [
            ("\n", Choice::Automatic),
            ("1\n", Choice::Automatic),
            ("2\n", Choice::Once),
            ("3\n", Choice::Skip),
            ("", Choice::Skip),
            ("bad\n2\n", Choice::Once),
        ] {
            assert_eq!(
                prompt(&mut input.as_bytes(), &mut Vec::new()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn preference_survives_reloads_and_requires_explicit_true() {
        let dir = std::env::temp_dir().join(format!("xmux-auto-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!automatic(&dir));
        std::fs::write(dir.join(PREFERENCE), "true\n").unwrap();
        assert!(automatic(&dir));
        std::fs::write(dir.join(PREFERENCE), "false\n").unwrap();
        assert!(!automatic(&dir));
        std::fs::write(dir.join(PREFERENCE), "broken").unwrap();
        assert!(!automatic(&dir));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
