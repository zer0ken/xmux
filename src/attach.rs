//! The terminal handover into a mux session: the `run_attach` exec that hands the
//! controlling terminal to the mux client, and `own_mux_session`, which names the
//! session xmux is ITSELF running in.
//!
//! Nesting is not refused. The app attaches its clients as PTY children rather than
//! handing over this terminal, so nesting costs it nothing, and a handover a mux WOULD
//! refuse is left to that mux to refuse in its own words. The one session it will not
//! touch is its own: see [`own_mux_session`].

use anyhow::{anyhow, Result};

/// The mux session xmux is ITSELF running in, as `(mux kind, session name)`, or `None`
/// when it is not inside one.
///
/// Its one use is REFUSAL: mirroring this session would attach a second client to the
/// very session holding xmux, which moves the user's own client and paints xmux inside
/// itself. Nothing else may branch on it.
///
/// zellij and psmux both name the session in the environment. tmux names only the socket
/// and the pane, so it is ASKED - one `display-message` at startup, run with the mux
/// environment left intact, because the ambient session is exactly what is being asked
/// about. A tmux that cannot answer leaves the session unknown, and an unknown session
/// blocks nothing.
pub fn own_mux_session() -> Option<(String, String)> {
    let (kind, name) = own_mux_from_env(
        std::env::var("ZELLIJ").ok().as_deref(),
        std::env::var("ZELLIJ_SESSION_NAME").ok().as_deref(),
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("PSMUX_SESSION").ok().as_deref(),
    )?;
    let name = match name {
        Some(n) => n,
        None => tmux_session_name()?,
    };
    Some((kind.to_string(), name))
}

/// The pure core of [`own_mux_session`]: which mux this process is inside, and the
/// session name when the environment carries it.
///
/// Each mux is recognized by ITS OWN inside-marker before its session variable is read,
/// because those variables outlive the shell that set them: a psmux pane started from a
/// zellij session still carries `ZELLIJ_SESSION_NAME`, and reading it there would name
/// the wrong session entirely.
fn own_mux_from_env(
    zellij: Option<&str>,
    zellij_session: Option<&str>,
    tmux: Option<&str>,
    psmux_session: Option<&str>,
) -> Option<(&'static str, Option<String>)> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    if set(zellij).is_some() {
        return Some(("zellij", set(zellij_session).map(str::to_string)));
    }
    if set(tmux).is_some() {
        // psmux sets `TMUX` for tmux-compat, so the psmux variable is what tells the two
        // apart - and it carries the answer, so no tmux family member needs asking twice.
        return Some(match set(psmux_session) {
            Some(name) => ("psmux", Some(name.to_string())),
            None => ("tmux", None),
        });
    }
    None
}

/// Asks the ambient tmux which session this process is in. The mux environment is left
/// as it is: the question is about the session that environment names.
fn tmux_session_name() -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#{session_name}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
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
    fn each_mux_is_recognized_by_its_own_marker() {
        // zellij first: it is the only one that sets `ZELLIJ`.
        assert_eq!(
            own_mux_from_env(Some("0"), Some("friendly-duck"), None, None),
            Some(("zellij", Some("friendly-duck".to_string())))
        );
        // psmux sets `TMUX` too, so `PSMUX_SESSION` is what tells the family apart.
        assert_eq!(
            own_mux_from_env(
                None,
                None,
                Some("/tmp/psmux-8648/default,60836,0"),
                Some("xmus")
            ),
            Some(("psmux", Some("xmus".to_string())))
        );
        // tmux names no session: it has to be asked.
        assert_eq!(
            own_mux_from_env(None, None, Some("/tmp/tmux-1000/default,1234,0"), None),
            Some(("tmux", None))
        );
        // Outside every mux.
        assert_eq!(own_mux_from_env(None, None, None, None), None);
        assert_eq!(own_mux_from_env(Some(""), None, Some(""), None), None);
    }

    #[test]
    fn a_stale_session_variable_does_not_name_the_wrong_session() {
        // A psmux pane started from a zellij session inherits `ZELLIJ_SESSION_NAME`
        // while `ZELLIJ` itself is gone. Reading it there would refuse to mirror a
        // session xmux is not in, and keep mirroring the one it IS in.
        assert_eq!(
            own_mux_from_env(
                None,
                Some("friendly-duck"),
                Some("/tmp/psmux/x,1,0"),
                Some("xmus")
            ),
            Some(("psmux", Some("xmus".to_string()))),
        );
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
