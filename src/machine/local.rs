//! The local machine transport: runs a mux argv on this machine, injecting
//! `-S <socket>` to target a non-default mux server. It issues no remote shell
//! command, so it uses none of `super::vocab`.

use super::Transport;
use crate::session::LOCAL_SOURCE;

/// The local machine. `socket` targets a non-default mux server (`-S <socket>`,
/// parsed from `$TMUX`); `None` ⇒ the default socket.
#[derive(Clone, Debug)]
pub struct Local {
    pub socket: Option<String>,
}

impl Transport for Local {
    fn host_id(&self) -> &str {
        LOCAL_SOURCE
    }

    fn exec_argv(&self, _tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        let mut args: Vec<String> = Vec::new();
        if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
            args.push("-S".into());
            args.push(sock.to_string());
        }
        args.extend_from_slice(&mux_argv[1..]);
        (mux_argv[0].clone(), args)
    }

    /// A LOCAL interactive attach hands the terminal to the bare attach argv (with
    /// `-S <socket>` injection) and IGNORES `pre_select` — a local source pre-selects
    /// the window with a separate instant command.
    fn interactive_attach_argv(
        &self,
        mux_attach_argv: &[String],
        _pre_select: Option<&[String]>,
    ) -> (String, Vec<String>) {
        self.exec_argv(true, mux_attach_argv)
    }

    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        let mut v = vec![mux_control_argv[0].clone()];
        if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
            v.push("-S".into());
            v.push(sock.to_string());
        }
        v.extend_from_slice(&mux_control_argv[1..]);
        v
    }

    fn clone_box(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(socket: Option<&str>) -> Local {
        Local {
            socket: socket.map(str::to_string),
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
        let (n, a) = local(None).exec_argv(false, &argv(&["psmux", "list-sessions", "-F", "x"]));
        assert_eq!(n, "psmux");
        assert_eq!(a, argv(&["list-sessions", "-F", "x"]));

        let (n, a) = local(Some("/tmp/tmux-1000/work"))
            .exec_argv(false, &argv(&["tmux", "list-sessions", "-F", "x"]));
        assert_eq!(n, "tmux");
        assert_eq!(
            a,
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
    fn interactive_attach_local_ignores_pre_select_and_injects_socket() {
        // A LOCAL interactive attach hands the terminal to a bare mux attach argv (the
        // window is pre-selected by a separate instant local command, so the pre-select
        // is ignored here). A non-default socket is injected via -S, exactly as exec_argv.
        let mux_attach = argv(&["psmux", "new-session", "-A", "-s", "dev"]);
        let (n, a) = local(None).interactive_attach_argv(&mux_attach, None);
        assert_eq!(n, "psmux");
        assert_eq!(a, argv(&["new-session", "-A", "-s", "dev"]));
        // A pre-select is ignored for local (it happens via a separate command).
        let (n, a) = local(None).interactive_attach_argv(&mux_attach, Some(&argv(&["psmux", "x"])));
        assert_eq!(n, "psmux");
        assert_eq!(a, argv(&["new-session", "-A", "-s", "dev"]));
        // Non-default socket is injected before the attach args.
        let (n, a) = local(Some("/tmp/tmux-1000/work"))
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]), None);
        assert_eq!(n, "tmux");
        assert_eq!(
            a,
            argv(&["-S", "/tmp/tmux-1000/work", "attach", "-t", "api"])
        );
    }
}
