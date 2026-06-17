//! Performs lifecycle operations (create, kill, rename, inspect) directly against
//! the live mux on a source. Each function builds a mux argv and runs it through
//! [`Source::run`]; nothing is cached and no state is held.

use crate::mux;
use crate::session::WindowPanes;
use crate::source::{RunError, Source};

/// Creates-or-attaches a DETACHED session on the source and returns its assigned
/// name (the mux prints it; auto-named when `name` is empty). The trailing
/// whitespace is trimmed.
pub async fn create(s: &Source, name: &str) -> Result<String, RunError> {
    let out = s.run(&mux::new_session(&s.binary, name)).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Kills a session by name.
pub async fn kill(s: &Source, name: &str) -> Result<(), RunError> {
    s.run(&mux::kill_session(&s.binary, name)).await?;
    Ok(())
}

/// Renames a session.
pub async fn rename(s: &Source, old_name: &str, new_name: &str) -> Result<(), RunError> {
    s.run(&mux::rename_session(&s.binary, old_name, new_name))
        .await?;
    Ok(())
}

/// Returns the source session's windows-with-panes (for the tree's child loading
/// and active-pane resolution).
pub async fn panes(s: &Source, name: &str) -> Result<Vec<WindowPanes>, RunError> {
    let out = s.run(&mux::list_panes(&s.binary, name)).await?;
    Ok(mux::parse_panes(&String::from_utf8_lossy(&out)))
}

/// Returns the visible content of a target pane (a `"session"`,
/// `"session:window"`, or `"session:window.pane"` target) — the live preview
/// source.
pub async fn capture(s: &Source, target: &str) -> Result<String, RunError> {
    let out = s.run(&mux::capture_pane(&s.binary, target)).await?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Makes a window active in its session (used before attach so the client lands
/// on the chosen window).
pub async fn select_window(s: &Source, sess: &str, window: i64) -> Result<(), RunError> {
    s.run(&mux::select_window(
        &s.binary,
        &mux::window_target(sess, window),
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Runner;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Records the command it was asked to run and returns canned results. For a
    /// LOCAL source it receives `name = binary` and `args = the mux argv WITHOUT
    /// the leading binary`.
    struct RecordingRunner {
        out: Vec<u8>,
        fail: bool,
        recorded: Mutex<Option<(String, Vec<String>)>>,
    }

    impl RecordingRunner {
        fn new(out: &str, fail: bool) -> Arc<Self> {
            Arc::new(RecordingRunner {
                out: out.as_bytes().to_vec(),
                fail,
                recorded: Mutex::new(None),
            })
        }
        fn name(&self) -> String {
            self.recorded.lock().unwrap().as_ref().unwrap().0.clone()
        }
        fn args(&self) -> Vec<String> {
            self.recorded.lock().unwrap().as_ref().unwrap().1.clone()
        }
    }

    #[async_trait]
    impl Runner for RecordingRunner {
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            *self.recorded.lock().unwrap() = Some((name.to_string(), args.to_vec()));
            if self.fail {
                Err(RunError::Other("boom".into()))
            } else {
                Ok(self.out.clone())
            }
        }
    }

    fn local_source(r: Arc<dyn Runner>) -> Source {
        Source {
            alias: "local".into(),
            binary: "psmux".into(),
            remote: false,
            control_path: String::new(),
            os: "linux".into(),
            socket: None,
            runner: Some(r),
        }
    }

    #[tokio::test]
    async fn create_named_trims_and_targets() {
        let fr = RecordingRunner::new("myname\n", false);
        let got = create(&local_source(fr.clone()), "x").await.unwrap();
        assert_eq!(got, "myname");
        assert_eq!(fr.name(), "psmux");
        assert_eq!(
            fr.args(),
            vec![
                "new-session",
                "-A",
                "-d",
                "-P",
                "-F",
                "#{session_name}",
                "-s",
                "x"
            ]
        );
    }

    #[tokio::test]
    async fn create_auto_name_omits_target() {
        let fr = RecordingRunner::new("0\n", false);
        let got = create(&local_source(fr.clone()), "").await.unwrap();
        assert_eq!(got, "0");
        assert!(!fr.args().iter().any(|a| a == "-s"), "{:?}", fr.args());
    }

    #[tokio::test]
    async fn create_error_returns_err() {
        let fr = RecordingRunner::new("ignored\n", true);
        assert!(create(&local_source(fr), "x").await.is_err());
    }

    #[tokio::test]
    async fn kill_targets_and_propagates_error() {
        let fr = RecordingRunner::new("", false);
        kill(&local_source(fr.clone()), "x").await.unwrap();
        assert_eq!(fr.args(), vec!["kill-session", "-t", "x"]);

        let fe = RecordingRunner::new("", true);
        assert!(kill(&local_source(fe), "x").await.is_err());
    }

    #[tokio::test]
    async fn rename_targets() {
        let fr = RecordingRunner::new("", false);
        rename(&local_source(fr.clone()), "old", "new")
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["rename-session", "-t", "old", "new"]);
    }

    #[tokio::test]
    async fn panes_parses_and_targets() {
        let fr = RecordingRunner::new("1\t1\t1\t1\tbash\tshell\n2\t0\t1\t1\ttail\tlogs\n", false);
        let got = panes(&local_source(fr.clone()), "x").await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].index, 1);
        assert_eq!(got[0].name, "shell");
        assert!(got[0].active);
        assert_eq!(got[0].panes[0].command, "bash");
        assert_eq!(got[1].index, 2);
        assert_eq!(got[1].name, "logs");
        assert!(!got[1].active);
        assert_eq!(got[1].panes[0].command, "tail");
        assert_eq!(
            fr.args(),
            vec!["list-panes", "-s", "-t", "x", "-F", mux::PANE_FORMAT]
        );
    }

    #[tokio::test]
    async fn panes_error_returns_err() {
        let fr = RecordingRunner::new("", true);
        assert!(panes(&local_source(fr), "x").await.is_err());
    }

    #[tokio::test]
    async fn capture_targets() {
        let fr = RecordingRunner::new("$ npm run dev\nReady\n", false);
        let got = capture(&local_source(fr.clone()), "editor:1.0")
            .await
            .unwrap();
        assert_eq!(got, "$ npm run dev\nReady\n");
        assert_eq!(
            fr.args(),
            vec!["capture-pane", "-p", "-e", "-t", "editor:1.0"]
        );
    }

    #[tokio::test]
    async fn select_window_targets() {
        let fr = RecordingRunner::new("", false);
        select_window(&local_source(fr.clone()), "editor", 3)
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["select-window", "-t", "editor:3"]);
    }

}
