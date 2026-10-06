//! The local machine transport: runs a mux argv on this machine, injecting
//! `-S <socket>` to target a non-default mux server. It issues no remote shell
//! command, so it uses none of `super::vocab`.

use super::Transport;
use crate::session::LOCAL_SOURCE;

/// The local machine. `socket` targets a non-default mux server (`-S <socket>`,
/// parsed from `$TMUX`); `None` ⇒ the default socket. `id` is the SOURCE id this
/// transport answers as: bare `local` when this machine serves one mux, `local:<mux>` when
/// it serves several, so two local sources on the same box stay distinct keys.
///
/// It injects the socket it is GIVEN and asks nothing about it. Naming no mux, it cannot
/// know whether the mux it wraps understands a socket flag, so whether a socket is passed
/// at all is decided by the composition sites that know the mux; a socket that arrives
/// here is one the mux has already been found to take.
#[derive(Clone, Debug)]
pub struct Local {
    pub id: String,
    pub socket: Option<String>,
}

impl Default for Local {
    fn default() -> Self {
        Local {
            id: LOCAL_SOURCE.to_string(),
            socket: None,
        }
    }
}

impl Transport for Local {
    fn host_id(&self) -> &str {
        &self.id
    }

    /// This box's local mux registry is authoritative for a local host (registry-merge
    /// enumeration + a local `list-clients` tty probe). A local attach spawns the mux
    /// binary directly, so `runs_through_shell` stays the default `false`.
    fn local_registry_scope(&self) -> bool {
        true
    }

    /// A local command is a local process: there is no connection to open.
    fn reuses_connection(&self) -> bool {
        true
    }

    fn exec_argv(&self, _tty: bool, mux_argv: &[String]) -> super::CommandSpec {
        let mut args: Vec<String> = Vec::new();
        if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
            args.push("-S".into());
            args.push(sock.to_string());
        }
        args.extend_from_slice(&mux_argv[1..]);
        super::CommandSpec::new(mux_argv[0].clone(), args)
    }

    /// A LOCAL interactive attach hands the terminal to the bare attach argv (with
    /// `-S <socket>` injection).
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> super::CommandSpec {
        self.exec_argv(true, mux_attach_argv)
    }

    fn control_argv(&self, mux_control_argv: &[String]) -> super::CommandSpec {
        let mut v = vec![mux_control_argv[0].clone()];
        if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
            v.push("-S".into());
            v.push(sock.to_string());
        }
        v.extend_from_slice(&mux_control_argv[1..]);
        super::CommandSpec::from_argv(v)
    }

    /// A local `-CC` control child is spawned with the mux binary directly and, on
    /// Unix, dies at once on pipe stdio (`tcgetattr failed: Inappropriate ioctl for
    /// device`), so the spawner must give it a pty - the local equivalent of the
    /// remote `-tt`. Windows has no native tmux to run this path, so it stays on
    /// pipes there.
    fn control_needs_pty(&self) -> bool {
        cfg!(unix)
    }

    fn clone_box(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }

    fn clone_as(&self, id: &str) -> Box<dyn Transport> {
        Box::new(Self {
            id: id.to_string(),
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(socket: Option<&str>) -> Local {
        Local {
            socket: socket.map(str::to_string),
            ..Local::default()
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_id_is_local_and_not_remote() {
        assert_eq!(local(None).host_id(), "local");
        assert!(!local(Some("/x")).is_remote());
    }

    #[test]
    fn exec_argv_local_plain_and_socket() {
        let command = local(None).exec_argv(false, &argv(&["psmux", "list-sessions", "-F", "x"]));
        assert_eq!(command.program(), "psmux");
        assert_eq!(command.args(), argv(&["list-sessions", "-F", "x"]));

        let command = local(Some("/tmp/tmux-1000/work"))
            .exec_argv(false, &argv(&["tmux", "list-sessions", "-F", "x"]));
        assert_eq!(command.program(), "tmux");
        assert_eq!(
            command.args(),
            argv(&["-S", "/tmp/tmux-1000/work", "list-sessions", "-F", "x"])
        );
    }

    #[test]
    fn control_argv_local_injects_socket_before_cc() {
        // The mux control argv is `[bin, -CC, attach]`; local splices -S after the binary.
        assert_eq!(
            local(None).control_argv(&argv(&["psmux", "-CC", "attach"])),
            argv(&["psmux", "-CC", "attach"])
        );
        assert_eq!(
            local(Some("/tmp/tmux-1000/work")).control_argv(&argv(&["tmux", "-CC", "attach"])),
            argv(&["tmux", "-S", "/tmp/tmux-1000/work", "-CC", "attach"])
        );
    }

    #[test]
    fn control_needs_pty_is_true_only_for_a_unix_local_host() {
        // A local `-CC` control child is spawned with the mux binary directly and
        // on Unix dies on pipe stdio, so it needs a pty of its own; ssh's `-tt`
        // and WSL's `script` wrapper already provide one on their paths.
        assert_eq!(local(None).control_needs_pty(), cfg!(unix));
        assert_eq!(
            local(Some("/tmp/tmux-1000/work")).control_needs_pty(),
            cfg!(unix)
        );
        assert!(
            !crate::transport::ssh("prod".into(), String::new(), "linux".into())
                .control_needs_pty()
        );
        assert!(!crate::transport::wsl("Ubuntu".into()).control_needs_pty());
    }

    #[test]
    fn interactive_attach_local_injects_socket() {
        // A LOCAL interactive attach hands the terminal to a bare mux attach argv. A
        // non-default socket is injected via -S, exactly as exec_argv.
        let mux_attach = argv(&["psmux", "new-session", "-A", "-s", "dev"]);
        let command = local(None).interactive_attach_argv(&mux_attach);
        assert_eq!(command.program(), "psmux");
        assert_eq!(command.args(), argv(&["new-session", "-A", "-s", "dev"]));
        // Non-default socket is injected before the attach args.
        let command = local(Some("/tmp/tmux-1000/work"))
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]));
        assert_eq!(command.program(), "tmux");
        assert_eq!(
            command.args(),
            argv(&["-S", "/tmp/tmux-1000/work", "attach", "-t", "api"])
        );
    }
}
