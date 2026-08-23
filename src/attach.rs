//! The terminal handover into a mux session: the `run_attach` exec that hands the
//! controlling terminal to the mux client.
//!
//! It does not ask whether xmux is itself inside a mux. The app attaches its clients as
//! PTY children rather than handing over this terminal, so nesting costs it nothing, and
//! a handover a mux WOULD refuse is left to that mux to refuse in its own words.

use anyhow::{anyhow, Result};

/// Hands the controlling terminal to a child process and waits.
pub trait Execer {
    fn exec(&self, argv: &[String]) -> Result<()>;
}

/// Runs `argv[0]` with `argv[1..]`, wiring the standard streams (inherited), and
/// waits — the same code on Windows and unix.
pub struct OsExecer;

impl Execer for OsExecer {
    fn exec(&self, argv: &[String]) -> Result<()> {
        // std::process inherits stdin/stdout/stderr by default, handing over the
        // terminal and blocking until the child exits.
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("command exited with status {status}"))
        }
    }
}

/// Runs the given argv through the [`Execer`]. Returns an error for empty argv
/// without calling the Execer.
pub fn run_attach(e: &dyn Execer, argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        return Err(anyhow!("attach: empty argv"));
    }
    e.exec(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records the argv it was handed and returns a canned result.
    struct FakeExecer {
        got: RefCell<Option<Vec<String>>>,
        fail: bool,
    }

    impl Execer for FakeExecer {
        fn exec(&self, argv: &[String]) -> Result<()> {
            *self.got.borrow_mut() = Some(argv.to_vec());
            if self.fail {
                Err(anyhow!("boom"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn run_attach_passes_argv_and_error() {
        let f = FakeExecer {
            got: RefCell::new(None),
            fail: true,
        };
        let argv: Vec<String> = ["tmux", "attach", "-t", "dev"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = run_attach(&f, &argv).unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert_eq!(f.got.borrow().as_ref().unwrap(), &argv);
    }

    #[test]
    fn run_attach_empty_argv() {
        let f = FakeExecer {
            got: RefCell::new(None),
            fail: false,
        };
        assert!(run_attach(&f, &[]).is_err());
        assert!(f.got.borrow().is_none(), "execer must not be called");
    }

    #[cfg(windows)]
    #[test]
    fn os_execer_runs_harmless_command() {
        let argv: Vec<String> = ["cmd", "/c", "exit", "0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(OsExecer.exec(&argv).is_ok());
    }
}
