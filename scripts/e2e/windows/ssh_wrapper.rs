//! An `ssh.exe` for the Windows client run. Windows OpenSSH reads `~/.ssh` from the
//! profile folder whatever `HOME` says, so this program runs the real ssh with
//! `-F $XMUX_E2E_SSH_CONFIG` in front of the arguments it was given, passing the
//! terminal, the streams, and the exit code through.

use std::process::{exit, Command};

fn main() {
    let real = std::env::var("XMUX_E2E_REAL_SSH")
        .unwrap_or_else(|_| r"C:\Windows\System32\OpenSSH\ssh.exe".to_string());
    let config = std::env::var("XMUX_E2E_SSH_CONFIG").expect("XMUX_E2E_SSH_CONFIG is not set");
    let status = Command::new(real)
        .arg("-F")
        .arg(config)
        .args(std::env::args_os().skip(1))
        .status()
        .expect("cannot start ssh");
    exit(status.code().unwrap_or(1));
}
