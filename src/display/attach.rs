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
/// The kind comes from the inside-marker env vars (`$ZELLIJ`, `$TMUX`) that the muxes
/// themselves set. The session NAME is not trusted from an env string; tmux and psmux
/// are asked of their server with one `display-message` at startup, run with the mux
/// environment left intact, because the ambient session is exactly what is being asked
/// about. zellij names its session only in the environment, so there its name is read
/// from `$ZELLIJ_SESSION_NAME`. A query that cannot answer leaves the session unknown,
/// and an unknown session blocks nothing. For psmux the server answer is preferred, but
/// a server the client cannot reach falls back to the session name psmux put in the
/// environment, so a degraded client still refuses rather than painting itself.
pub fn own_mux_session() -> Option<(String, String)> {
    let kind = own_mux_kind(
        std::env::var("ZELLIJ").ok().as_deref(),
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("PSMUX_SESSION").ok().as_deref(),
    )?;
    let name = match kind {
        MuxKind::Zellij => non_empty(std::env::var("ZELLIJ_SESSION_NAME").ok()),
        MuxKind::Tmux => mux_session_name("tmux"),
        MuxKind::Psmux => {
            mux_session_name("psmux").or_else(|| non_empty(std::env::var("PSMUX_SESSION").ok()))
        }
    }?;
    Some((kind.as_str().to_string(), name))
}

/// The mux kind xmux runs inside, read from the inside markers.
///
/// Each mux is recognized by ITS OWN inside-marker before any session variable is read,
/// because those variables outlive the shell that set them: a psmux pane started from a
/// zellij session still carries `ZELLIJ_SESSION_NAME`, and reading it there would name
/// the wrong session entirely. `PSMUX_SESSION` is read only to tell psmux apart from
/// tmux (psmux sets `$TMUX` for tmux-compat); its VALUE is never trusted for the name.
#[derive(PartialEq, Debug)]
enum MuxKind {
    Zellij,
    Tmux,
    Psmux,
}

impl MuxKind {
    fn as_str(&self) -> &'static str {
        match self {
            MuxKind::Zellij => "zellij",
            MuxKind::Tmux => "tmux",
            MuxKind::Psmux => "psmux",
        }
    }
}

/// The pure core of [`own_mux_session`]: which mux this process is inside.
fn own_mux_kind(
    zellij: Option<&str>,
    tmux: Option<&str>,
    psmux_session: Option<&str>,
) -> Option<MuxKind> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    if set(zellij).is_some() {
        return Some(MuxKind::Zellij);
    }
    if set(tmux).is_some() {
        // psmux sets `TMUX` for tmux-compat, so `PSMUX_SESSION` is what tells the two
        // apart. It only DISCRIMINATES the kind; the name is asked of the server.
        return Some(if set(psmux_session).is_some() {
            MuxKind::Psmux
        } else {
            MuxKind::Tmux
        });
    }
    None
}

/// Asks the mux server which session the ambient client is in. The mux environment is
/// left as it is: the question is about the session that environment names, so `$TMUX`
/// stays to route the query to the right server.
fn mux_session_name(binary: &str) -> Option<String> {
    let out = std::process::Command::new(binary)
        .args(["display-message", "-p", "#{session_name}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
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
        assert_eq!(own_mux_kind(Some("0"), None, None), Some(MuxKind::Zellij));
        // psmux sets `TMUX` too, so `PSMUX_SESSION` is what tells the kinds apart.
        assert_eq!(
            own_mux_kind(None, Some("/tmp/psmux-8648/default,60836,0"), Some("xmus")),
            Some(MuxKind::Psmux)
        );
        // tmux names only the socket and pane.
        assert_eq!(
            own_mux_kind(None, Some("/tmp/tmux-1000/default,1234,0"), None),
            Some(MuxKind::Tmux)
        );
        // Outside every mux.
        assert_eq!(own_mux_kind(None, None, None), None);
        assert_eq!(own_mux_kind(Some(""), Some(""), None), None);
    }

    #[test]
    fn each_kind_names_its_mux() {
        assert_eq!(MuxKind::Zellij.as_str(), "zellij");
        assert_eq!(MuxKind::Tmux.as_str(), "tmux");
        assert_eq!(MuxKind::Psmux.as_str(), "psmux");
    }

    #[test]
    fn a_stale_session_variable_does_not_name_the_wrong_session() {
        // A psmux pane started from a zellij session inherits `ZELLIJ_SESSION_NAME`
        // while `ZELLIJ` itself is gone. Reading it there would refuse to mirror a
        // session xmux is not in, and keep mirroring the one it IS in.
        assert_eq!(
            own_mux_kind(None, Some("/tmp/psmux/x,1,0"), Some("xmus")),
            Some(MuxKind::Psmux),
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
