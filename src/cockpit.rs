//! The cockpit: a persistent supervisor that owns the terminal, runs one
//! mux-client child at a time, and serves a control socket so the in-mux popup
//! can ask it to re-attach a session on another server — the cross-host switch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::control;
use crate::session::{self, Session};

/// A target the cockpit attaches: a session and an optional window to land on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub session: Session,
    pub window: Option<i64>,
}

/// The single well-known pointer file naming the live cockpit's control socket.
pub fn cockpit_pointer_path(xmux_dir: &Path) -> PathBuf {
    xmux_dir.join("cockpit")
}

/// Records the cockpit socket path so the popup can find a live cockpit.
pub fn write_cockpit_pointer(xmux_dir: &Path, socket_path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(xmux_dir)?;
    std::fs::write(
        cockpit_pointer_path(xmux_dir),
        socket_path.to_string_lossy().as_bytes(),
    )
}

/// Reads the recorded cockpit socket path, if a pointer exists and is non-empty.
/// Liveness is proven by dialing the socket, not by this read.
pub fn read_cockpit_pointer(xmux_dir: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(cockpit_pointer_path(xmux_dir)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Removes the pointer file (on cockpit exit).
pub fn remove_cockpit_pointer(xmux_dir: &Path) {
    let _ = std::fs::remove_file(cockpit_pointer_path(xmux_dir));
}

/// Handles one cockpit control request line. Returns the reply payload and, for a
/// valid `switch <source>/<name>` naming a known source, the target the loop
/// should attach next.
pub fn dispatch_cockpit(
    line: &str,
    known_source: &dyn Fn(&str) -> bool,
) -> (String, Option<Target>) {
    let req = control::parse_request(line);
    match req.verb.as_str() {
        "ping" => ("pong".to_string(), None),
        "switch" => match session::parse_target(req.arg.trim()) {
            Ok(s) if known_source(&s.source) => (
                "ok".to_string(),
                Some(Target {
                    session: s,
                    window: None,
                }),
            ),
            Ok(s) => (format!("err: unknown source {:?}", s.source), None),
            Err(e) => (format!("err: {e}"), None),
        },
        _ => ("err: unknown command".to_string(), None),
    }
}

/// What the popup does with a pick.
#[derive(Debug, PartialEq, Eq)]
pub enum PopupAction {
    /// Same-server pick: `switch-client` in place (instant).
    SwitchClient,
    /// Cross-server pick with a cockpit pointer present: signal it to re-attach.
    SignalCockpit,
    /// Cross-server pick with no cockpit pointer: cannot cross hosts from here.
    NoCockpit,
}

/// Decides the popup action from the pick and whether a cockpit pointer exists.
pub fn decide_popup_action(
    chosen: &Session,
    local_source: &str,
    cockpit_available: bool,
) -> PopupAction {
    if chosen.source == local_source {
        PopupAction::SwitchClient
    } else if cockpit_available {
        PopupAction::SignalCockpit
    } else {
        PopupAction::NoCockpit
    }
}

/// Hands the terminal to the attach for a target and returns when the child exits.
#[async_trait]
pub trait Attacher: Send + Sync {
    async fn attach(&self, target: &Target);
}

/// Runs the full-screen picker; returns the chosen target, or `None` to quit.
#[async_trait]
pub trait Picker: Send + Sync {
    async fn pick(&self) -> Option<Target>;
}

/// A recorded cross-server switch, shared between the accept task and the loop.
pub type Pending = Arc<Mutex<Option<Target>>>;

/// The cockpit supervisor loop. Attaches the current target; when the child
/// exits, a recorded switch re-attaches with no picker (the seamless cross-host
/// switch), otherwise the picker chooses the next target (or quits).
pub async fn cockpit_loop(
    attacher: &dyn Attacher,
    picker: &dyn Picker,
    pending: Pending,
    initial: Option<Target>,
) -> i32 {
    let mut target = match initial {
        Some(t) => t,
        None => match picker.pick().await {
            Some(t) => t,
            None => return 0,
        },
    };
    loop {
        attacher.attach(&target).await;
        let next = pending.lock().unwrap().take();
        target = match next {
            Some(t) => t,
            None => match picker.pick().await {
                Some(t) => t,
                None => return 0,
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xmux-cockpit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pointer_round_trip_and_absent() {
        let dir = tmp("ptr");
        assert!(read_cockpit_pointer(&dir).is_none(), "absent pointer is None");
        let sock = dir.join("cockpit-123.sock");
        write_cockpit_pointer(&dir, &sock).unwrap();
        assert_eq!(read_cockpit_pointer(&dir), Some(sock));
        remove_cockpit_pointer(&dir);
        assert!(read_cockpit_pointer(&dir).is_none(), "removed pointer is None");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_switch_ping_and_errors() {
        let known = |s: &str| matches!(s, "local" | "jupiter06");
        let known: &dyn Fn(&str) -> bool = &known;

        assert_eq!(dispatch_cockpit("ping", known).0, "pong");

        let (reply, target) = dispatch_cockpit("switch jupiter06/api", known);
        assert_eq!(reply, "ok");
        let t = target.expect("a valid switch yields a target");
        assert_eq!(t.session.source, "jupiter06");
        assert_eq!(t.session.name, "api");
        assert_eq!(t.window, None);

        // Unknown source → err, no target.
        let (reply, target) = dispatch_cockpit("switch nope/x", known);
        assert!(reply.starts_with("err:"), "{reply}");
        assert!(target.is_none());

        // Malformed address → err, no target.
        let (reply, target) = dispatch_cockpit("switch noslash", known);
        assert!(reply.starts_with("err:"), "{reply}");
        assert!(target.is_none());

        // Unknown verb.
        assert_eq!(dispatch_cockpit("bogus", known).0, "err: unknown command");
    }

    #[test]
    fn popup_decision_table() {
        use crate::session::{Session, LOCAL_SOURCE};
        let local = Session {
            source: LOCAL_SOURCE.into(),
            name: "w".into(),
            ..Default::default()
        };
        let remote = Session {
            source: "jupiter06".into(),
            name: "api".into(),
            ..Default::default()
        };

        assert_eq!(
            decide_popup_action(&local, LOCAL_SOURCE, false),
            PopupAction::SwitchClient
        );
        assert_eq!(
            decide_popup_action(&local, LOCAL_SOURCE, true),
            PopupAction::SwitchClient
        );
        assert_eq!(
            decide_popup_action(&remote, LOCAL_SOURCE, true),
            PopupAction::SignalCockpit
        );
        assert_eq!(
            decide_popup_action(&remote, LOCAL_SOURCE, false),
            PopupAction::NoCockpit
        );
    }

    fn target(source: &str, name: &str) -> Target {
        Target {
            session: Session {
                source: source.into(),
                name: name.into(),
                ..Default::default()
            },
            window: None,
        }
    }

    #[tokio::test]
    async fn loop_reattaches_on_pending_then_picks_on_bare_exit() {
        use std::sync::Mutex;

        // Attacher logs each target and injects a pending switch "during" each attach.
        struct ScriptAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
            inject: Mutex<Vec<Option<Target>>>,
        }
        #[async_trait::async_trait]
        impl Attacher for ScriptAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                let inj = self.inject.lock().unwrap().remove(0);
                *self.pending.lock().unwrap() = inj;
            }
        }
        struct QuitPicker;
        #[async_trait::async_trait]
        impl Picker for QuitPicker {
            async fn pick(&self) -> Option<Target> {
                None
            }
        }

        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = ScriptAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
            inject: Mutex::new(vec![Some(target("jupiter06", "api")), None]),
        };
        let rc = cockpit_loop(
            &attacher,
            &QuitPicker,
            pending.clone(),
            Some(target("local", "work")),
        )
        .await;
        assert_eq!(rc, 0);
        assert_eq!(
            *attacher.log.lock().unwrap(),
            vec!["local/work", "jupiter06/api"]
        );
    }

    #[tokio::test]
    async fn loop_runs_picker_first_when_no_initial_target() {
        use std::sync::Mutex;
        struct RecordAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
        }
        #[async_trait::async_trait]
        impl Attacher for RecordAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                *self.pending.lock().unwrap() = None;
            }
        }
        struct ScriptPicker(Mutex<Vec<Option<Target>>>);
        #[async_trait::async_trait]
        impl Picker for ScriptPicker {
            async fn pick(&self) -> Option<Target> {
                self.0.lock().unwrap().remove(0)
            }
        }
        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = RecordAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
        };
        let picker = ScriptPicker(Mutex::new(vec![Some(target("local", "work")), None]));
        let rc = cockpit_loop(&attacher, &picker, pending.clone(), None).await;
        assert_eq!(rc, 0);
        assert_eq!(*attacher.log.lock().unwrap(), vec!["local/work"]);
    }
}
