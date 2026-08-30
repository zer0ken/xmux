//! Performs the mux operations xmux itself issues - create a session, and read a
//! host's options - directly against the live mux on a host. Each function composes the two orthogonal axes: the
//! MUX axis (`Host::mux`'s `*_plan`) supplies the mux argv and the MACHINE axis
//! (`Host::transport`'s `exec_argv`) lowers it for local-vs-ssh execution, then it
//! runs via an injected runner - exactly like `mux::enumerate_via_list_sessions`.
//! Nothing is cached and no state is held. Off-loop `Ops` assemble a value host from
//! config and pass the source's runner.

use crate::model::source::{RunError, Runner};
use crate::model::Host;

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

/// Creates a DETACHED session on the host and returns its name. A mux that names its
/// own creations ([`Mux::assigns_new_session_name`]) auto-names an empty request and
/// prints the final name, so its stdout is read back (trailing whitespace trimmed). A
/// mux that does not gets a name chosen HERE when the request is empty, and its stdout
/// is never read as a name: a silent create can still put NOISE on stdout (a shell
/// banner, an motd), and adopting it would name a session that does not exist.
///
/// [`Mux::assigns_new_session_name`]: crate::mux::Mux::assigns_new_session_name
pub async fn create(host: &Host, runner: &dyn Runner, name: &str) -> Result<String, RunError> {
    if host.mux.assigns_new_session_name() {
        let out = run_plan(host, runner, &host.mux.new_session_plan(name)).await?;
        let printed = String::from_utf8_lossy(&out).trim().to_string();
        return Ok(if printed.is_empty() {
            name.to_string()
        } else {
            printed
        });
    }
    let name = if name.is_empty() {
        pick_session_name(host, runner).await?
    } else {
        name.to_string()
    };
    run_plan(host, runner, &host.mux.new_session_plan(&name)).await?;
    Ok(name)
}

/// The name an empty create request gets on a mux that cannot name its own: the first
/// name of the shared `<adjective>-<noun>` walk (the same one instance naming uses)
/// that no session on the host holds. The taken set is the host's LIVE listing rather
/// than xmux's inventory, so a session created outside xmux since the last poll cannot
/// be collided with. Falls back to counting numerals after a full pass, so a name
/// always comes back.
async fn pick_session_name(host: &Host, runner: &dyn Runner) -> Result<String, RunError> {
    let taken: std::collections::HashSet<String> = host
        .mux
        .enumerate(host.transport.as_ref(), runner)
        .await?
        .into_iter()
        .map(|s| s.name)
        .collect();
    for n in 0..super::control::nth_name_total() {
        let name = super::control::nth_name(n);
        if !taken.contains(&name) {
            return Ok(name);
        }
    }
    Ok((taken.len()..)
        .map(|i| i.to_string())
        .find(|name| !taken.contains(name))
        .expect("an unbounded numeral walk always leaves the finite taken set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::source::Runner;
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
        Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
        )
    }

    /// A REMOTE tmux host: its ops route through `Mux` (mux argv) x `Transport`
    /// (ssh wrapping), so the recorded command is `ssh … "<tmux …>"` with the mux argv
    /// joined per-arg-quoted as the trailing remote command.
    fn remote_host() -> Host {
        Host::new(
            crate::transport::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
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

    /// Answers a SEQUENCE of commands in order, recording each: a create on a mux
    /// that cannot name its own sessions runs the host listing first and the create
    /// second.
    struct SeqRunner {
        outs: Mutex<std::collections::VecDeque<Result<Vec<u8>, RunError>>>,
        recorded: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl SeqRunner {
        fn new(outs: Vec<Result<Vec<u8>, RunError>>) -> Self {
            SeqRunner {
                outs: Mutex::new(outs.into_iter().collect()),
                recorded: Mutex::new(Vec::new()),
            }
        }
        fn commands(&self) -> Vec<(String, Vec<String>)> {
            self.recorded.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Runner for SeqRunner {
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            self.recorded
                .lock()
                .unwrap()
                .push((name.to_string(), args.to_vec()));
            self.outs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    /// A LOCAL zellij host: a mux that cannot name its own sessions (its create prints
    /// nothing and its binary refuses an empty name).
    fn zellij_host() -> Host {
        Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("zellij").unwrap(),
        )
    }

    #[tokio::test]
    async fn create_on_a_silent_mux_never_reads_stdout_as_a_name() {
        // Windows zellij leaks the pane shell's banner onto the caller's stdout; a
        // login shell can put an motd there. None of it is a session name.
        let fr = SeqRunner::new(vec![Ok(b"PowerShell 7.6.5\nPS C:\\Users\\me> ".to_vec())]);
        let got = create(&zellij_host(), &fr, "dev").await.unwrap();
        assert_eq!(got, "dev");
        let cmds = fr.commands();
        assert_eq!(cmds.len(), 1, "a named create runs no listing: {cmds:?}");
        assert_eq!(cmds[0].0, "zellij");
        assert_eq!(cmds[0].1, vec!["attach", "-b", "dev"]);
    }

    #[tokio::test]
    async fn create_empty_on_a_silent_mux_names_the_session_itself() {
        // The walk starts at its first name and skips what the LIVE listing holds:
        // "amber-otter" is taken, so the create gets "amber-heron".
        let fr = SeqRunner::new(vec![
            Ok(b"amber-otter [Created 5s ago] \n".to_vec()),
            Ok(Vec::new()),
        ]);
        let got = create(&zellij_host(), &fr, "").await.unwrap();
        assert_eq!(got, "amber-heron");
        let cmds = fr.commands();
        assert_eq!(cmds.len(), 2, "listing then create: {cmds:?}");
        assert_eq!(cmds[0].1, vec!["list-sessions", "-n"]);
        assert_eq!(cmds[1].1, vec!["attach", "-b", "amber-heron"]);
    }

    #[tokio::test]
    async fn create_empty_on_an_idle_silent_mux_takes_the_walks_first_name() {
        // zellij reports an idle host as an error-shaped "no sessions" message; the
        // listing classifies it as reachable-but-empty, so the walk is unobstructed.
        let fr = SeqRunner::new(vec![
            Err(RunError::Exit {
                stderr: "No active zellij sessions found.".into(),
                code: 1,
            }),
            Ok(Vec::new()),
        ]);
        let got = create(&zellij_host(), &fr, "").await.unwrap();
        assert_eq!(got, "amber-otter");
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
}
