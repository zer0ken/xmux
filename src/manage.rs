//! Performs the mux operations xmux itself issues - create a session, and read a
//! host's panes / options - directly against the live mux on a host. Each function composes the two orthogonal axes: the
//! MUX axis (`Host::mux`'s `*_plan`) supplies the mux argv and the MACHINE axis
//! (`Host::transport`'s `exec_argv`) lowers it for local-vs-ssh execution, then it
//! runs via an injected runner — exactly like `mux::enumerate_via_list_sessions`.
//! Nothing is cached and no state is held. Off-loop `Ops` assemble a value host from
//! config and pass the source's runner.

use crate::model::Host;
use crate::session::WindowPanes;
use crate::source::{RunError, Runner};

/// Composes a mux argv (from the host's `Mux`) through the machine `Transport` and
/// runs it via the injected runner, returning stdout.
async fn run_plan(
    host: &Host,
    runner: &dyn Runner,
    mux_argv: &[String],
) -> Result<Vec<u8>, RunError> {
    let (name, args) = host.transport.exec_argv(false, mux_argv);
    runner.run(&name, &args).await
}

/// Creates a DETACHED session on the host and returns its assigned name (the mux
/// prints it; tmux auto-names when `name` is empty). The trailing whitespace is
/// trimmed. A mux that creates SILENTLY (zellij's `attach -b` prints nothing) yields
/// the requested name, which is the name it just created.
pub async fn create(host: &Host, runner: &dyn Runner, name: &str) -> Result<String, RunError> {
    let out = run_plan(host, runner, &host.mux.new_session_plan(name)).await?;
    let printed = String::from_utf8_lossy(&out).trim().to_string();
    Ok(if printed.is_empty() {
        name.to_string()
    } else {
        printed
    })
}

/// Returns the host session's windows-with-panes (for the tree's child loading
/// and active-pane resolution).
pub async fn panes(
    host: &Host,
    runner: &dyn Runner,
    name: &str,
) -> Result<Vec<WindowPanes>, RunError> {
    let out = run_plan(host, runner, &host.mux.list_panes_plan(name)).await?;
    Ok(host.mux.parse_panes(&String::from_utf8_lossy(&out)))
}

/// Reads one global mux server option's trimmed value (`show -gv <name>`). Used to
/// match the view border colours to the displayed session's live `pane-*-border-style`.
/// A mux with no server options returns an EMPTY plan, and the value is empty without
/// a command being run.
pub async fn show_option(host: &Host, runner: &dyn Runner, name: &str) -> Result<String, RunError> {
    let plan = host.mux.show_option_plan(name);
    if plan.is_empty() {
        return Ok(String::new());
    }
    let out = run_plan(host, runner, &plan).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux;
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

    /// A LOCAL psmux host: its ops route through `Mux` (mux argv) x `Transport`
    /// (local `-S`), run via the injected runner (`name = binary`, `args = the mux
    /// argv WITHOUT the leading binary`).
    fn local_host() -> Host {
        Host::new(crate::machine::local(None), crate::mux::for_binary("psmux"))
    }

    /// A REMOTE tmux host: its ops route through `Mux` (mux argv) x `Transport`
    /// (ssh wrapping), so the recorded command is `ssh … "<tmux …>"` with the mux argv
    /// joined per-arg-quoted as the trailing remote command.
    fn remote_host() -> Host {
        Host::new(
            crate::machine::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        )
    }

    #[tokio::test]
    async fn create_named_trims_and_targets() {
        let fr = RecordingRunner::new("myname\n", false);
        let got = create(&local_host(), fr.as_ref(), "x").await.unwrap();
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
        let got = create(&local_host(), fr.as_ref(), "").await.unwrap();
        assert_eq!(got, "0");
        assert!(!fr.args().iter().any(|a| a == "-s"), "{:?}", fr.args());
    }

    #[tokio::test]
    async fn create_error_returns_err() {
        let fr = RecordingRunner::new("ignored\n", true);
        assert!(create(&local_host(), fr.as_ref(), "x").await.is_err());
    }

    #[tokio::test]
    async fn panes_parses_and_targets() {
        let fr = RecordingRunner::new("1\t1\t1\t1\tbash\tshell\n2\t0\t1\t1\ttail\tlogs\n", false);
        let got = panes(&local_host(), fr.as_ref(), "x").await.unwrap();
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
        assert!(panes(&local_host(), fr.as_ref(), "x").await.is_err());
    }

    // Each op composes the Mux plan through the Transport and runs it via the
    // injected runner: for a REMOTE host the recorded command is `ssh …` and the
    // trailing arg is the mux argv joined per-arg-quoted.
    #[tokio::test]
    async fn create_remote_wraps_new_session_in_ssh() {
        let fr = RecordingRunner::new("api\n", false);
        let got = create(&remote_host(), fr.as_ref(), "api").await.unwrap();
        assert_eq!(got, "api");
        assert_eq!(fr.name(), "ssh");
        assert_eq!(
            fr.args().last().unwrap(),
            "tmux new-session -A -d -P -F '#{session_name}' -s api"
        );
    }

    #[tokio::test]
    async fn panes_remote_wraps_list_panes_in_ssh() {
        let fr = RecordingRunner::new("0\t1\t0\t1\tbash\twork\n", false);
        panes(&remote_host(), fr.as_ref(), "work").await.unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(
            fr.args().last().unwrap(),
            &format!("tmux list-panes -s -t work -F '{}'", mux::PANE_FORMAT)
        );
    }
}
