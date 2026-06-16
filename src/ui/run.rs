//! The async event loop that runs the switcher: a tokio `select!` over a unified
//! command channel (real terminal key/mouse events, control-channel injections,
//! and preview results) and a 1s preview-poll interval. The core [`event_loop`]
//! is backend-generic so it is driveable headlessly; [`run_switcher`] adds the
//! real-terminal setup/teardown and the crossterm event reader.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::ListenerOptions;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::control;
use crate::ui::switcher::{Ops, PreviewTarget, Scan, SwitchResult, Switcher};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A unit of work the event loop processes, from any source.
pub enum Cmd {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A freshly captured preview (target, text) — `None` ⇒ capture failed.
    Preview(PreviewTarget, Option<String>),
    /// A control-channel `dump` request: reply with the rendered screen.
    Dump(oneshot::Sender<String>),
}

/// Renders the switcher to an off-screen buffer and flattens it — the payload the
/// control channel's `dump` returns.
pub fn dump_switcher(switcher: &mut Switcher, width: u16, height: u16) -> String {
    let w = width.max(1);
    let h = height.max(1);
    let mut term = match Terminal::new(TestBackend::new(w, h)) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if term.draw(|f| switcher.render(f)).is_err() {
        return String::new();
    }
    let buf = term.backend().buffer();
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

fn spawn_capture(switcher: &Switcher, ops: &Arc<dyn Ops>, cmd_tx: &mpsc::Sender<Cmd>) {
    let tgt = switcher.preview_target();
    if tgt.target.is_empty() {
        return;
    }
    let ops = ops.clone();
    let tx = cmd_tx.clone();
    tokio::spawn(async move {
        let text = ops.capture(&tgt.source, &tgt.target).await.ok();
        let _ = tx.send(Cmd::Preview(tgt, text)).await;
    });
}

fn handle_mouse(
    switcher: &mut Switcher,
    m: MouseEvent,
    last_click: &mut Option<(Instant, u16, u16)>,
) {
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let now = Instant::now();
            let is_double = last_click.is_some_and(|(t, c, r)| {
                now.duration_since(t) < DOUBLE_CLICK && c == m.column && r == m.row
            });
            if is_double {
                switcher.mouse_attach(m.column, m.row);
                *last_click = None;
            } else {
                switcher.mouse_select(m.column, m.row);
                *last_click = Some((now, m.column, m.row));
            }
        }
        MouseEventKind::ScrollDown => switcher.mouse_scroll(true),
        MouseEventKind::ScrollUp => switcher.mouse_scroll(false),
        _ => {}
    }
}

/// The backend-generic core loop. Draws after every change, polls the preview on
/// an interval and on cursor change, and exits when the switcher signals it.
pub async fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    switcher: &mut Switcher,
    ops: Arc<dyn Ops>,
    cmd_tx: mpsc::Sender<Cmd>,
    mut cmd_rx: mpsc::Receiver<Cmd>,
) -> anyhow::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_click: Option<(Instant, u16, u16)> = None;

    loop {
        terminal.draw(|f| switcher.render(f))?;
        if switcher.should_exit() {
            break;
        }
        // A target change (initial build or cursor move) kicks an immediate fetch.
        if switcher.take_poll_kick() {
            spawn_capture(switcher, &ops, &cmd_tx);
        }

        tokio::select! {
            maybe = cmd_rx.recv() => {
                let Some(cmd) = maybe else { break };
                match cmd {
                    Cmd::Key(k) => switcher.handle_key(k, ops.as_ref()).await,
                    Cmd::Mouse(m) => handle_mouse(switcher, m, &mut last_click),
                    Cmd::Preview(tgt, text) => switcher.apply_capture(&tgt, text),
                    Cmd::Dump(reply) => {
                        let size = terminal.size()?;
                        let _ = reply.send(dump_switcher(switcher, size.width, size.height));
                    }
                }
            }
            _ = poll.tick() => {
                spawn_capture(switcher, &ops, &cmd_tx);
            }
        }
    }
    Ok(())
}

/// Reads crossterm events and forwards key presses and mouse events into the
/// command channel until the channel closes or the stream ends.
async fn read_events(cmd_tx: mpsc::Sender<Cmd>) {
    let mut stream = EventStream::new();
    while let Some(Ok(event)) = stream.next().await {
        let cmd = match event {
            // Windows reports Press and Release; only act on Press.
            Event::Key(k) if k.kind == KeyEventKind::Press => Cmd::Key(k),
            Event::Mouse(m) => Cmd::Mouse(m),
            _ => continue,
        };
        if cmd_tx.send(cmd).await.is_err() {
            return;
        }
    }
}

/// A RAII guard that restores the terminal on drop (so a panic mid-loop does not
/// leave the terminal in raw mode / the alternate screen).
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        let _ = disable_raw_mode();
    }
}

/// A running control server: the accept-loop task plus the socket path to clean
/// up on shutdown.
pub struct ControlHandle {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
}

impl ControlHandle {
    fn shutdown(self) {
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
        _ => "err: unknown command".into(),
    }
}

/// Runs one interactive switcher session on the real terminal, polling the live
/// preview for as long as it is open. When `control` is `Some(path)`, a control
/// socket is served at that path for the session's lifetime.
pub async fn run_switcher(
    scan: Scan,
    ops: Arc<dyn Ops>,
    control: Option<PathBuf>,
) -> anyhow::Result<SwitchResult> {
    let mut switcher = Switcher::new(scan);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(256);

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;

    let events = tokio::spawn(read_events(cmd_tx.clone()));
    let control_handle = control.and_then(|p| serve_control(p, cmd_tx.clone()));

    let result = event_loop(&mut terminal, &mut switcher, ops, cmd_tx.clone(), cmd_rx).await;

    events.abort();
    if let Some(h) = control_handle {
        h.shutdown();
    }
    result?;
    Ok(switcher.result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::ui::switcher::Scan;
    use crate::ui::tree::Group;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    struct NoopOps;

    #[async_trait::async_trait]
    impl Ops for NoopOps {
        async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session> {
            Ok(Session {
                source: source.into(),
                name: name.into(),
                windows: 1,
                ..Default::default()
            })
        }
        async fn kill(&self, _s: &Session) -> anyhow::Result<()> {
            Ok(())
        }
        async fn rename(&self, _s: &Session, _n: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn panes(&self, _s: &Session) -> anyhow::Result<Vec<crate::session::WindowPanes>> {
            Ok(Vec::new())
        }
        async fn capture(&self, _source: &str, _target: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn refresh(&self) -> anyhow::Result<Scan> {
            Ok(Scan::default())
        }
    }

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
    async fn event_loop_key_attaches_then_exits() {
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample());
        let (tx, rx) = mpsc::channel(16);
        tx.send(Cmd::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
            .await
            .unwrap();
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();
        assert_eq!(
            sw.result().chosen.as_ref().map(|s| s.name.as_str()),
            Some("editor")
        );
    }

    #[tokio::test]
    async fn event_loop_dump_renders_screen() {
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample());
        let (tx, rx) = mpsc::channel(16);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Cmd::Dump(reply_tx)).await.unwrap();
        // Then quit so the loop exits.
        tx.send(Cmd::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();
        let dump = reply_rx.await.unwrap();
        assert!(
            dump.contains("editor"),
            "dump should render the tree:\n{dump}"
        );
        assert!(
            dump.contains("xmux"),
            "dump should render the header:\n{dump}"
        );
    }

    #[tokio::test]
    async fn dump_switcher_flattens_buffer() {
        let mut sw = Switcher::new(sample());
        let out = dump_switcher(&mut sw, 100, 30);
        assert!(out.contains("editor"));
        assert!(out.contains("Hosts · Sessions · Windows · Panes"));
    }

    #[tokio::test]
    async fn control_end_to_end() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = control::socket_path(&dir, std::process::id());

        let (tx, rx) = mpsc::channel::<Cmd>(64);
        let handle = serve_control(sock.clone(), tx.clone()).expect("bind control socket");

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample());
        let ops: Arc<dyn Ops> = Arc::new(NoopOps);
        let tx2 = tx.clone();
        let loop_task = tokio::spawn(async move {
            event_loop(&mut term, &mut sw, ops, tx2, rx).await.unwrap();
            sw.result()
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
        // Quit so the loop exits.
        assert_eq!(client.do_cmd("key q").await.unwrap(), "ok");

        let result = loop_task.await.unwrap();
        assert!(result.chosen.is_none(), "quit must leave no choice");
        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
