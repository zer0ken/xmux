//! The picker control socket: a per-pid local socket the headless driver dials to
//! inject keys/text and dump the rendered switcher screen. Each request line is
//! dispatched into the cockpit's command channel; the cockpit's `select!` loop
//! folds the [`Cmd`]s in. The `dump_switcher` helper flattens a switcher render to
//! text for the control channel's `dump` reply.

use std::path::PathBuf;

use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::ListenerOptions;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::control;
use crate::ui::switcher::Switcher;

/// The cockpit's state kind, as seen from the control channel. Mirrors
/// [`crate::proxy::app::AppState`] but lives here so `Cmd` stays in this crate
/// without pulling in the full proxy tree.
pub enum AppStateKind {
    Overlay,
    Passthrough,
}

/// A unit of work the cockpit loop processes, from the control socket.
pub enum Cmd {
    Key(KeyEvent),
    /// A control-channel `dump` request: reply with the rendered screen.
    Dump(oneshot::Sender<String>),
    /// Set the cockpit's overlay/passthrough state.
    SetState(AppStateKind),
    /// Forward raw bytes through the Passthrough input path to the active pane.
    Keys(Vec<u8>),
}

/// Renders the switcher to an off-screen buffer and flattens it — the payload the
/// control channel's `dump` returns.
pub fn dump_switcher(switcher: &mut Switcher, width: u16, height: u16) -> String {
    dump_overlay(switcher, None, width, height)
}

/// Renders the Overlay view — the switcher with the cursor host's live `grid` (if
/// any) in the terminal-view pane — to an off-screen `TestBackend` and flattens
/// it. So a headless `dump` reflects the same screen the main draw produces,
/// including the live terminal Grid. Runs without a real terminal.
pub fn dump_overlay(
    switcher: &mut Switcher,
    grid: Option<&crate::proxy::screen::Grid>,
    width: u16,
    height: u16,
) -> String {
    let w = width.max(1);
    let h = height.max(1);
    let mut term = match Terminal::new(TestBackend::new(w, h)) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if term.draw(|f| switcher.render(f, grid, false, crate::ui::switcher::TREE_WIDTH)).is_err() {
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

/// Parses a hex string (e.g. `"1b5b41"`) into bytes. Returns `Err` with an
/// `"err: ..."` message on odd length or non-hex characters.
fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("err: odd-length hex string".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("err: invalid hex '{}'", &s[i..i + 2]))
        })
        .collect()
}

async fn dispatch(line: &str, cmd_tx: &mpsc::Sender<Cmd>) -> String {
    let req = control::parse_request(line);
    match req.verb.as_str() {
        "ping" => "pong".into(),
        "dump" => {
            let (tx, rx) = oneshot::channel();
            if cmd_tx.send(Cmd::Dump(tx)).await.is_err() {
                return String::new();
            }
            rx.await.unwrap_or_default()
        }
        "key" => match control::parse_key(&req.arg) {
            Some(ev) => {
                let _ = cmd_tx.send(Cmd::Key(ev)).await;
                "ok".into()
            }
            None => "err: unknown key".into(),
        },
        "text" => {
            for c in req.arg.chars() {
                let ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                let _ = cmd_tx.send(Cmd::Key(ev)).await;
            }
            "ok".into()
        }
        "passthrough" => {
            let _ = cmd_tx.send(Cmd::SetState(AppStateKind::Passthrough)).await;
            "ok".into()
        }
        "overlay" => {
            let _ = cmd_tx.send(Cmd::SetState(AppStateKind::Overlay)).await;
            "ok".into()
        }
        "keys" => match parse_hex(req.arg.trim()) {
            Ok(bytes) => {
                let _ = cmd_tx.send(Cmd::Keys(bytes)).await;
                "ok".into()
            }
            Err(e) => e,
        },
        _ => "err: unknown command".into(),
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
        // key down → a Cmd::Key flows
        let r = dispatch("key down", &tx).await;
        assert_eq!(r, "ok");
        assert!(matches!(rx.recv().await, Some(Cmd::Key(_))));
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
    async fn dump_switcher_flattens_buffer() {
        let mut sw = Switcher::new(sample());
        let out = dump_switcher(&mut sw, 100, 30);
        assert!(out.contains("editor"));
        // The dump renders the full overlay (tree + footer); the footer's nav hint
        // is always present (the chrome titles were removed).
        assert!(out.contains("quit"), "footer hint present:\n{out}");
    }

    #[tokio::test]
    async fn dump_overlay_renders_the_live_grid() {
        // A dump with a live grid must include both the tree AND the grid content
        // (the terminal-view pane), so a headless `dump` reflects the live screen.
        let mut sw = Switcher::new(sample());
        let mut grid = crate::proxy::screen::Grid::new(30, 100);
        grid.feed(b"LIVEGRID");
        let out = dump_overlay(&mut sw, Some(&grid), 100, 30);
        assert!(out.contains("editor"), "tree still rendered:\n{out}");
        assert!(out.contains("LIVEGRID"), "live grid content rendered:\n{out}");
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
        // standing in for the cockpit loop: it answers `dump` and applies keys. It
        // exits when the channel closes (all senders dropped).
        let mut sw = Switcher::new(sample());
        let consumer = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Cmd::Key(k) => sw.handle_key(k),
                    Cmd::Dump(reply) => {
                        let _ = reply.send(dump_switcher(&mut sw, 100, 30));
                    }
                    Cmd::SetState(_) | Cmd::Keys(_) => {}
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
            client.do_cmd("key fnord").await.unwrap(),
            "err: unknown key"
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

    #[tokio::test]
    async fn dispatch_state_and_keys_verbs() {
        let (tx, mut rx) = mpsc::channel::<Cmd>(8);
        assert_eq!(dispatch("passthrough", &tx).await, "ok");
        assert!(matches!(
            rx.recv().await,
            Some(Cmd::SetState(AppStateKind::Passthrough))
        ));
        assert_eq!(dispatch("overlay", &tx).await, "ok");
        assert!(matches!(
            rx.recv().await,
            Some(Cmd::SetState(AppStateKind::Overlay))
        ));
        assert_eq!(dispatch("keys 1b5b41", &tx).await, "ok"); // ESC [ A
        assert!(matches!(rx.recv().await, Some(Cmd::Keys(b)) if b == vec![0x1b, 0x5b, 0x41]));
    }

    #[tokio::test]
    async fn dispatch_keys_rejects_invalid_hex() {
        let (tx, mut rx) = mpsc::channel::<Cmd>(8);
        // Odd-length hex is rejected; no Cmd is sent.
        let r = dispatch("keys abc", &tx).await;
        assert!(r.starts_with("err:"), "expected err, got: {r}");
        // Non-hex characters are rejected.
        let r = dispatch("keys zz", &tx).await;
        assert!(r.starts_with("err:"), "expected err, got: {r}");
        // Channel must be empty (no Cmd was sent for either bad input).
        assert!(rx.try_recv().is_err(), "no Cmd should be sent on parse error");
    }
}
