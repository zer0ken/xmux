//! The picker control socket: a per-pid local socket the headless driver dials to
//! inject keys/text and dump the rendered switcher screen. Each request line is
//! dispatched into the app's command channel; the app's `select!` loop
//! folds the [`Cmd`]s in. The `dump_switcher` helper flattens a switcher render to
//! text for the control channel's `dump` reply.

use std::path::PathBuf;

use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::ListenerOptions;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyEvent;
use ratatui::Terminal;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::control;
use crate::ui::switcher::Switcher;

/// A unit of work the app loop processes, from the control socket.
pub enum Cmd {
    /// A resolved domain action — folded in at the app's single `State::apply` site.
    Op(crate::model::Action),
    /// A control-channel `status` request: reply with the focus + selection line.
    Status(oneshot::Sender<String>),
    /// A control-channel `dump` request: reply with the rendered screen.
    Dump(oneshot::Sender<String>),
    /// Unstable/test-only: inject a raw key event into the switcher.
    RawKey(KeyEvent),
    /// Unstable/test-only: forward raw bytes to the focused pane.
    RawBytes(Vec<u8>),
}

/// Renders the switcher to an off-screen buffer and flattens it — the payload the
/// control channel's `dump` returns.
pub fn dump_switcher(
    switcher: &mut Switcher,
    state: &crate::state::State,
    width: u16,
    height: u16,
) -> String {
    dump_screen(switcher, None, width, height, state)
}

/// Renders the tree-focus view — the switcher with the cursor host's live `grid` (if
/// any) in the terminal-view pane — to an off-screen `TestBackend` and flattens
/// it. So a headless `dump` reflects the same screen the main draw produces,
/// including the live terminal Grid. Runs without a real terminal.
pub fn dump_screen(
    switcher: &mut Switcher,
    grid: Option<&crate::display::grid::Grid>,
    width: u16,
    height: u16,
    state: &crate::state::State,
) -> String {
    let w = width.max(1);
    let h = height.max(1);
    let mut term = match Terminal::new(TestBackend::new(w, h)) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if term
        .draw(|f| switcher.render(f, grid, false, crate::ui::switcher::TREE_WIDTH, state))
        .is_err()
    {
        return String::new();
    }
    flatten_buffer(term.backend().buffer())
}

/// Flattens a rendered buffer to text (one trimmed line per row).
fn flatten_buffer(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// A running control server: the accept-loop task plus the socket path to clean
/// up on shutdown.
pub struct ControlHandle {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
}

impl Drop for ControlHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds the per-pid control socket at `path` and serves it, forwarding injected
/// keys/text into `cmd_tx` and answering `ping`/`dump`. A bind failure returns
/// `None` (the UI runs without a control channel rather than failing).
pub fn serve_control(path: PathBuf, cmd_tx: mpsc::Sender<Cmd>) -> Option<ControlHandle> {
    let _ = std::fs::remove_file(&path); // remove a stale socket so the bind succeeds
    let name = control::endpoint_name(&path).ok()?;
    let listener = ListenerOptions::new().name(name).create_tokio().ok()?;
    // The ctl socket injects keystrokes into the live app, so it must be
    // owner-only. On unix the bind created a filesystem socket; tighten it to 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    // On Windows the endpoint is a named pipe (no filesystem presence); drop a
    // marker file so `discover` can still find this instance by pid. On unix the
    // bind already created the socket file at `path`.
    #[cfg(windows)]
    let _ = std::fs::write(&path, b"");
    let task = tokio::spawn(accept_loop(listener, cmd_tx));
    Some(ControlHandle { task, path })
}

async fn accept_loop(listener: Listener, cmd_tx: mpsc::Sender<Cmd>) {
    while let Ok(conn) = listener.accept().await {
        tokio::spawn(handle_conn(conn, cmd_tx.clone()));
    }
}

async fn handle_conn(conn: Stream, cmd_tx: mpsc::Sender<Cmd>) {
    let mut buf = BufReader::new(conn);
    loop {
        let mut line = String::new();
        match buf.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let payload = dispatch(&line, &cmd_tx).await;
        if control::write_frame(&mut buf, &payload).await.is_err() {
            return;
        }
    }
}

async fn dispatch(line: &str, cmd_tx: &mpsc::Sender<Cmd>) -> String {
    match crate::control::parse_ctl_op(line) {
        crate::control::CtlRequest::Ping => "pong".into(),
        crate::control::CtlRequest::Dump => {
            let (tx, rx) = oneshot::channel();
            if cmd_tx.send(Cmd::Dump(tx)).await.is_err() {
                return String::new();
            }
            rx.await.unwrap_or_default()
        }
        crate::control::CtlRequest::Status => {
            let (tx, rx) = oneshot::channel();
            if cmd_tx.send(Cmd::Status(tx)).await.is_err() {
                return String::new();
            }
            rx.await.unwrap_or_default()
        }
        crate::control::CtlRequest::Op(op) => {
            let _ = cmd_tx.send(Cmd::Op(op)).await;
            "ok".into()
        }
        crate::control::CtlRequest::RawKey(ev) => {
            let _ = cmd_tx.send(Cmd::RawKey(ev)).await;
            "ok".into()
        }
        crate::control::CtlRequest::RawBytes(b) => {
            let _ = cmd_tx.send(Cmd::RawBytes(b)).await;
            "ok".into()
        }
        crate::control::CtlRequest::Unknown(_) => "err: unknown command".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::ui::switcher::Scan;
    use crate::ui::tree::Group;

    fn sample() -> Scan {
        Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![Session {
                    source: "local".into(),
                    name: "editor".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                }],
            }],
            panes: Default::default(),
        }
    }

    #[tokio::test]
    async fn dispatch_dump_and_key_still_work() {
        let (tx, mut rx) = mpsc::channel::<Cmd>(8);
        // raw:key down → a Cmd::RawKey flows
        let r = dispatch("raw:key down", &tx).await;
        assert_eq!(r, "ok");
        assert!(matches!(rx.recv().await, Some(Cmd::RawKey(_))));
        // dump → a Cmd::Dump flows (answered by a parallel responder)
        let tx2 = tx.clone();
        tokio::spawn(async move {
            if let Some(Cmd::Dump(reply)) = rx.recv().await {
                let _ = reply.send("SCREEN".into());
            }
        });
        assert_eq!(dispatch("dump", &tx2).await, "SCREEN");
    }

    #[tokio::test]
    async fn dispatch_resolves_semantic_verbs_to_op_cmds() {
        use crate::model::{Action, FocusTarget};
        let (tx, mut rx) = mpsc::channel::<Cmd>(8);
        assert_eq!(dispatch("switch jup/api", &tx).await, "ok");
        assert!(
            matches!(rx.recv().await, Some(Cmd::Op(Action::Switch { address })) if address == "jup/api")
        );
        assert_eq!(dispatch("focus tree", &tx).await, "ok");
        assert!(matches!(
            rx.recv().await,
            Some(Cmd::Op(Action::Focus(FocusTarget::Tree)))
        ));
        assert_eq!(dispatch("rescan", &tx).await, "ok");
        assert!(matches!(rx.recv().await, Some(Cmd::Op(Action::Rescan))));
        // raw: keystrokes still flow, but only via the unstable namespace.
        assert_eq!(dispatch("raw:keys 1b5b41", &tx).await, "ok");
        assert!(matches!(rx.recv().await, Some(Cmd::RawBytes(b)) if b == vec![0x1b, 0x5b, 0x41]));
        // the demoted bare verb is rejected
        assert!(dispatch("key down", &tx).await.starts_with("err:"));
    }

    #[tokio::test]
    async fn dump_switcher_flattens_buffer() {
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        let out = dump_switcher(&mut sw, &state, 100, 30);
        assert!(out.contains("editor"));
        // The dump renders the full screen (tree + hint_bar); the hint_bar's nav hint
        // is always present (the screen carries no chrome titles).
        assert!(out.contains("quit"), "hint_bar hint present:\n{out}");
    }

    #[tokio::test]
    async fn dump_screen_renders_the_live_grid() {
        // A dump with a live grid must include both the tree AND the grid content
        // (the terminal-view pane), so a headless `dump` reflects the live screen.
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        let mut grid = crate::display::grid::Grid::new(30, 100);
        grid.feed(b"LIVEGRID");
        let out = dump_screen(&mut sw, Some(&grid), 100, 30, &state);
        assert!(out.contains("editor"), "tree still rendered:\n{out}");
        assert!(
            out.contains("LIVEGRID"),
            "live grid content rendered:\n{out}"
        );
    }

    #[tokio::test]
    async fn control_handle_drop_removes_socket() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-drop-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A distinct pid so the Windows pipe endpoint (xmux-ctl-<pid>) does not
        // collide with the concurrently-running control_end_to_end test.
        let sock = control::socket_path(&dir, std::process::id().wrapping_add(7));
        let (tx, _rx) = mpsc::channel::<Cmd>(8);
        let handle = serve_control(sock.clone(), tx).expect("bind control socket");
        assert!(sock.exists(), "socket/marker present while serving");
        drop(handle);
        assert!(
            !sock.exists(),
            "socket/marker removed when the handle drops"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn control_end_to_end() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = control::socket_path(&dir, std::process::id());

        let (tx, mut rx) = mpsc::channel::<Cmd>(64);
        let handle = serve_control(sock.clone(), tx.clone()).expect("bind control socket");

        // A minimal in-test consumer drives the switcher directly off the channel,
        // standing in for the app loop: it answers `dump` and applies keys. It
        // exits when the channel closes (all senders dropped).
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        let consumer = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Cmd::RawKey(k) => {
                        // This minimal consumer does not run off-loop ops; drop the
                        // commands handle_key returns.
                        let _ = sw.handle_key(k, &mut state);
                    }
                    Cmd::Dump(reply) => {
                        let _ = reply.send(dump_switcher(&mut sw, &state, 100, 30));
                    }
                    Cmd::Status(reply) => {
                        let _ = reply.send("focus=tree target=editor".into());
                    }
                    Cmd::Op(_) | Cmd::RawBytes(_) => {}
                }
            }
        });

        let mut client = control::Client::dial(&sock).await.unwrap();
        assert_eq!(client.do_cmd("ping").await.unwrap(), "pong");
        let dump = client.do_cmd("dump").await.unwrap();
        assert!(
            dump.contains("editor"),
            "dump should render the tree:\n{dump}"
        );
        assert_eq!(
            client.do_cmd("raw:key fnord").await.unwrap(),
            "err: unknown command"
        );
        assert_eq!(
            client.do_cmd("bogus").await.unwrap(),
            "err: unknown command"
        );

        // Close the channel (drop every sender) so the consumer exits.
        drop(client);
        drop(handle);
        drop(tx);
        consumer.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn control_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("xmux-ctl-perm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = control::socket_path(&dir, std::process::id().wrapping_add(11));
        let (tx, _rx) = mpsc::channel::<Cmd>(8);
        let handle = serve_control(sock.clone(), tx).expect("bind control socket");
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "ctl socket must be owner-only (rw-------)");
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
