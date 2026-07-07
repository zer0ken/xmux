//! Performs lifecycle operations (create, kill, rename, inspect) directly against
//! the live mux on a host. Each function composes the two orthogonal axes: the
//! MUX axis (`Host::mux`'s `*_plan`) supplies the mux argv and the MACHINE axis
//! (`Host::transport`'s `exec_argv`) lowers it for local-vs-ssh execution, then it
//! runs via an injected runner — exactly like `mux::enumerate_via_list_sessions`.
//! Nothing is cached and no state is held. Off-loop `Ops` assemble a value host from
//! config and pass the source's runner.

use crate::model::Host;
use crate::mux;
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

/// Creates-or-attaches a DETACHED session on the host and returns its assigned
/// name (the mux prints it; auto-named when `name` is empty). The trailing
/// whitespace is trimmed.
pub async fn create(host: &Host, runner: &dyn Runner, name: &str) -> Result<String, RunError> {
    let out = run_plan(host, runner, &host.mux.new_session_plan(name)).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Kills a session by name.
pub async fn kill(host: &Host, runner: &dyn Runner, name: &str) -> Result<(), RunError> {
    run_plan(host, runner, &host.mux.kill_session_plan(name)).await?;
    Ok(())
}

/// Renames a session.
pub async fn rename(
    host: &Host,
    runner: &dyn Runner,
    old_name: &str,
    new_name: &str,
) -> Result<(), RunError> {
    run_plan(
        host,
        runner,
        &host.mux.rename_session_plan(old_name, new_name),
    )
    .await?;
    Ok(())
}

/// Kills a window by `session:window` target.
pub async fn kill_window(host: &Host, runner: &dyn Runner, target: &str) -> Result<(), RunError> {
    run_plan(host, runner, &host.mux.kill_window_plan(target)).await?;
    Ok(())
}

/// Renames a window.
pub async fn rename_window(
    host: &Host,
    runner: &dyn Runner,
    target: &str,
    new_name: &str,
) -> Result<(), RunError> {
    run_plan(host, runner, &host.mux.rename_window_plan(target, new_name)).await?;
    Ok(())
}

/// Returns the host session's windows-with-panes (for the tree's child loading
/// and active-pane resolution).
pub async fn panes(
    host: &Host,
    runner: &dyn Runner,
    name: &str,
) -> Result<Vec<WindowPanes>, RunError> {
    let out = run_plan(host, runner, &host.mux.list_panes_plan(name)).await?;
    Ok(mux::parse_panes(&String::from_utf8_lossy(&out)))
}

/// Reads one global mux server option's trimmed value (`show -gv <name>`). Used to
/// match the view border colours to the displayed session's live `pane-*-border-style`.
pub async fn show_option(host: &Host, runner: &dyn Runner, name: &str) -> Result<String, RunError> {
    let out = run_plan(host, runner, &host.mux.show_option_plan(name)).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Creates a new window in a session (optionally named).
pub async fn new_window(
    host: &Host,
    runner: &dyn Runner,
    session: &str,
    name: &str,
) -> Result<(), RunError> {
    run_plan(host, runner, &host.mux.new_window_plan(session, name)).await?;
    Ok(())
}

/// Splits a window/session target into a new pane (`vertical` → stacked, else
/// side-by-side).
pub async fn split_window(
    host: &Host,
    runner: &dyn Runner,
    target: &str,
    vertical: bool,
) -> Result<(), RunError> {
    run_plan(host, runner, &host.mux.split_window_plan(target, vertical)).await?;
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
    async fn kill_targets_and_propagates_error() {
        let fr = RecordingRunner::new("", false);
        kill(&local_host(), fr.as_ref(), "x").await.unwrap();
        assert_eq!(fr.args(), vec!["kill-session", "-t", "x"]);

        let fe = RecordingRunner::new("", true);
        assert!(kill(&local_host(), fe.as_ref(), "x").await.is_err());
    }

    #[tokio::test]
    async fn rename_targets() {
        let fr = RecordingRunner::new("", false);
        rename(&local_host(), fr.as_ref(), "old", "new")
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["rename-session", "-t", "old", "new"]);
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

    #[tokio::test]
    async fn new_window_targets_session() {
        let fr = RecordingRunner::new("", false);
        new_window(&local_host(), fr.as_ref(), "work", "logs")
            .await
            .unwrap();
        // The trailing `:` on the target keeps a numeric session name unambiguous.
        assert_eq!(fr.args(), vec!["new-window", "-t", "work:", "-n", "logs"]);
    }

    #[tokio::test]
    async fn new_window_auto_name_omits_dash_n() {
        let fr = RecordingRunner::new("", false);
        new_window(&local_host(), fr.as_ref(), "0", "")
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["new-window", "-t", "0:"]);
    }

    #[tokio::test]
    async fn split_window_horizontal_and_vertical() {
        let fr = RecordingRunner::new("", false);
        split_window(&local_host(), fr.as_ref(), "work:1", false)
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["split-window", "-h", "-t", "work:1"]);

        let fv = RecordingRunner::new("", false);
        split_window(&local_host(), fv.as_ref(), "work:1", true)
            .await
            .unwrap();
        assert_eq!(fv.args(), vec!["split-window", "-v", "-t", "work:1"]);
    }

    #[tokio::test]
    async fn kill_window_and_rename_window_target() {
        let fk = RecordingRunner::new("", false);
        kill_window(&local_host(), fk.as_ref(), "api:2")
            .await
            .unwrap();
        assert_eq!(fk.args(), vec!["kill-window", "-t", "api:2"]);

        let fr = RecordingRunner::new("", false);
        rename_window(&local_host(), fr.as_ref(), "api:2", "logs")
            .await
            .unwrap();
        assert_eq!(fr.args(), vec!["rename-window", "-t", "api:2", "logs"]);
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
    async fn kill_remote_wraps_kill_session_in_ssh() {
        let fr = RecordingRunner::new("", false);
        kill(&remote_host(), fr.as_ref(), "old").await.unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(fr.args().last().unwrap(), "tmux kill-session -t old");
    }

    #[tokio::test]
    async fn rename_remote_wraps_rename_session_in_ssh() {
        let fr = RecordingRunner::new("", false);
        rename(&remote_host(), fr.as_ref(), "old", "new")
            .await
            .unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(fr.args().last().unwrap(), "tmux rename-session -t old new");
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

    #[tokio::test]
    async fn new_window_remote_wraps_in_ssh() {
        let fr = RecordingRunner::new("", false);
        new_window(&remote_host(), fr.as_ref(), "work", "logs")
            .await
            .unwrap();
        assert_eq!(fr.name(), "ssh");
        // `work:` carries a `:`, not shell-safe, so remote_command single-quotes it.
        assert_eq!(
            fr.args().last().unwrap(),
            "tmux new-window -t 'work:' -n logs"
        );
    }

    #[tokio::test]
    async fn split_window_remote_wraps_in_ssh() {
        let fr = RecordingRunner::new("", false);
        split_window(&remote_host(), fr.as_ref(), "work:1", true)
            .await
            .unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(
            fr.args().last().unwrap(),
            "tmux split-window -v -t 'work:1'"
        );
    }

    #[tokio::test]
    async fn kill_window_remote_wraps_in_ssh() {
        let fr = RecordingRunner::new("", false);
        kill_window(&remote_host(), fr.as_ref(), "api:2")
            .await
            .unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(fr.args().last().unwrap(), "tmux kill-window -t 'api:2'");
    }

    #[tokio::test]
    async fn rename_window_remote_wraps_in_ssh() {
        let fr = RecordingRunner::new("", false);
        rename_window(&remote_host(), fr.as_ref(), "api:2", "logs")
            .await
            .unwrap();
        assert_eq!(fr.name(), "ssh");
        assert_eq!(
            fr.args().last().unwrap(),
            "tmux rename-window -t 'api:2' logs"
        );
    }
}
