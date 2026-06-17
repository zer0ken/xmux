//! Resolves and performs the terminal handover into a mux session: the in-mux
//! switch plan (same-server teleport vs cross-server detach-to-home) and the
//! out-of-mux attach that hands the controlling terminal to a child.

use anyhow::{anyhow, Result};

/// Reports whether the process is running inside a mux, by checking `$TMUX`
/// (psmux also sets `TMUX` for tmux-compat, so this one check covers both).
pub fn in_mux() -> bool {
    in_mux_value(std::env::var("TMUX").ok().as_deref())
}

/// The pure core of [`in_mux`]: a non-empty value means inside a mux. Split out
/// so it is testable without mutating the process environment.
fn in_mux_value(tmux: Option<&str>) -> bool {
    matches!(tmux, Some(v) if !v.is_empty())
}

/// Returns a descriptive error when `in_mux` is true, else `Ok`. Attaching a mux
/// from inside a mux is refused (psmux/tmux forbid nesting). The message tells
/// the user to detach first (prefix d). It takes the [`in_mux`] result as a
/// parameter so it is testable without touching the environment.
pub fn nest_guard(in_mux: bool) -> Result<()> {
    if in_mux {
        return Err(anyhow!(
            "already inside a mux session: detach first (prefix d), then run xmux"
        ));
    }
    Ok(())
}

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

    #[test]
    fn in_mux_value_cases() {
        assert!(in_mux_value(Some("/tmp/tmux-1000/default,1234,0")));
        assert!(!in_mux_value(Some("")));
        assert!(!in_mux_value(None));
    }

    #[test]
    fn nest_guard_outside() {
        assert!(nest_guard(false).is_ok());
    }

    #[test]
    fn nest_guard_inside() {
        let err = nest_guard(true).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("detach"),
            "message should mention detaching: {err}"
        );
    }

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
