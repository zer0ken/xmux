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
use crate::session::{Session, WindowPanes};
use crate::ui::switcher::{run_op, OpResult, Ops, PreviewTarget, SwitchResult, Switcher};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A unit of work the event loop processes, from any source.
pub enum Cmd {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Host terminal resized (cols, rows) — re-layout the picker.
    Resize(u16, u16),
    /// A freshly captured preview (target, text) — `None` ⇒ capture failed.
    Preview(PreviewTarget, Option<String>),
    /// One source's `list-sessions` outcome, streamed in as it returns. `err` set
    /// ⇒ the host was unreachable.
    SourceResult {
        source: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// One session's `list-panes` outcome, streamed in independently of the rest.
    Panes {
        address: String,
        panes: Vec<WindowPanes>,
    },
    /// A control-channel `dump` request: reply with the rendered screen.
    Dump(oneshot::Sender<String>),
    /// A slow (create/rename/kill) op finished off-loop; fold its result in.
    OpDone(OpResult),
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
    if !switcher.preview_capturable() {
        return;
    }
    let tgt = switcher.preview_target();
    let ops = ops.clone();
    let tx = cmd_tx.clone();
    tokio::spawn(async move {
        let text = ops.capture(&tgt.source, &tgt.target).await.ok();
        let _ = tx.send(Cmd::Preview(tgt, text)).await;
    });
}

/// Spawns one detached `list-sessions` probe per source; each streams its outcome
/// back as a [`Cmd::SourceResult`] the moment it returns, so a fast host never
/// waits on a slow one. The first `terminal.draw` runs before these are polled, so
/// the skeleton paints instantly.
fn spawn_probes(ops: &Arc<dyn Ops>, cmd_tx: &mpsc::Sender<Cmd>) {
    for source in ops.sources() {
        let ops = ops.clone();
        let tx = cmd_tx.clone();
        tokio::spawn(async move {
            let (sessions, err) = match ops.list_sessions(&source).await {
                Ok(s) => (s, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
            let _ = tx
                .send(Cmd::SourceResult {
                    source,
                    sessions,
                    err,
                })
                .await;
        });
    }
}

/// Spawns one detached `list-panes` probe per session of a freshly reachable
/// host; each streams its windows/panes back as a [`Cmd::Panes`] independently.
fn spawn_panes(ops: &Arc<dyn Ops>, cmd_tx: &mpsc::Sender<Cmd>, sessions: Vec<Session>) {
    for sess in sessions {
        let ops = ops.clone();
        let tx = cmd_tx.clone();
        tokio::spawn(async move {
            let panes = ops.panes(&sess).await.unwrap_or_default();
            let _ = tx
                .send(Cmd::Panes {
                    address: sess.address(),
                    panes,
                })
                .await;
        });
    }
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
        // The very first draw paints the host skeletons before any probe is
        // polled (the runtime is current-thread; spawned probes run at the await
        // below), so the first frame is instant.
        terminal.draw(|f| switcher.render(f))?;
        if switcher.should_exit() {
            break;
        }
        // A target change (initial build or cursor move) kicks an immediate fetch.
        if switcher.take_poll_kick() {
            spawn_capture(switcher, &ops, &cmd_tx);
        }
        // The seed (and each `r` re-scan) kicks one streaming probe per source.
        if switcher.take_rescan_kick() {
            spawn_probes(&ops, &cmd_tx);
        }

        tokio::select! {
            maybe = cmd_rx.recv() => {
                let Some(cmd) = maybe else { break };
                match cmd {
                    Cmd::Key(k) => {
                        switcher.handle_key(k);
                        // A create/rename/kill runs OFF the loop so a slow ssh
                        // round-trip never freezes rendering, streaming, or ctl.
                        if let Some(op) = switcher.take_pending_op() {
                            let ops = ops.clone();
                            let tx = cmd_tx.clone();
                            tokio::spawn(async move {
                                let result = run_op(&op, ops.as_ref()).await;
                                let _ = tx.send(Cmd::OpDone(result)).await;
                            });
                        }
                    }
                    Cmd::Mouse(m) => handle_mouse(switcher, m, &mut last_click),
                    Cmd::Resize(cols, rows) => {
                        let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, cols, rows));
                    }
                    Cmd::Preview(tgt, text) => switcher.apply_capture(&tgt, text),
                    Cmd::SourceResult { source, sessions, err } => {
                        let reachable = err.is_none();
                        switcher.apply_source_result(source, sessions.clone(), err);
                        // A reachable host's sessions each get their panes fetched.
                        if reachable {
                            spawn_panes(&ops, &cmd_tx, sessions);
                        }
                    }
                    Cmd::Panes { address, panes } => switcher.apply_panes(address, panes),
                    Cmd::Dump(reply) => {
                        // A transient size-query failure must not kill the switcher;
                        // fall back to a sane default so the dump still renders.
                        let size = terminal
                            .size()
                            .unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let _ = reply.send(dump_switcher(switcher, size.width, size.height));
                    }
                    Cmd::OpDone(result) => switcher.apply_op_result(result),
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
    ops: Arc<dyn Ops>,
    control: Option<PathBuf>,
) -> anyhow::Result<SwitchResult> {
    // Seed host skeletons from the resolved source list — no probing — so the
    // first frame paints immediately; the event loop streams the rest in.
    let mut switcher = Switcher::from_sources(ops.sources());
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(256);

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;

    let events = tokio::spawn(read_events(cmd_tx.clone()));
    let control_handle = control.and_then(|p| serve_control(p, cmd_tx.clone()));

    let result = event_loop(&mut terminal, &mut switcher, ops, cmd_tx.clone(), cmd_rx).await;

    events.abort();
    drop(control_handle); // abort the accept loop and remove the socket
    result?;
    Ok(switcher.result())
}

/// Like `run_switcher` but the CALLER owns the terminal (raw mode, screen
/// buffer) and the input source. Used by the PTY proxy overlay: no
/// `TerminalGuard` (no alt-screen toggle) and no `read_events` (the proxy feeds
/// `Cmd::Key`/`Cmd::Resize` over `cmd_tx`).
pub async fn run_picker_fed(
    ops: Arc<dyn Ops>,
    cmd_tx: mpsc::Sender<Cmd>,
    cmd_rx: mpsc::Receiver<Cmd>,
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> anyhow::Result<SwitchResult> {
    let mut switcher = Switcher::from_sources(ops.sources());
    term.clear()?;
    event_loop(term, &mut switcher, ops, cmd_tx, cmd_rx).await?;
    Ok(switcher.result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Pane, Session, WindowPanes};
    use crate::ui::switcher::Scan;
    use crate::ui::tree::Group;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use std::collections::HashMap;

    struct NoopOps;

    #[async_trait::async_trait]
    impl Ops for NoopOps {
        fn sources(&self) -> Vec<String> {
            // No sources ⇒ the loop kicks no probes, leaving a switcher built from
            // a complete snapshot untouched (these tests don't exercise streaming).
            Vec::new()
        }
        async fn list_sessions(&self, _source: &str) -> anyhow::Result<Vec<Session>> {
            Ok(Vec::new())
        }
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
        async fn panes(&self, _s: &Session) -> anyhow::Result<Vec<WindowPanes>> {
            Ok(Vec::new())
        }
        async fn capture(&self, _source: &str, _target: &str) -> anyhow::Result<String> {
            Ok(String::new())
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

    /// Ops that stream canned per-source sessions and per-session panes, mimicking
    /// the real probe fan-out the event loop kicks on start.
    struct StreamOps {
        sources: Vec<String>,
        sessions: HashMap<String, Vec<Session>>,
        panes: HashMap<String, Vec<WindowPanes>>,
    }

    impl StreamOps {
        /// A local host plus a remote host, the remote session carrying pane detail.
        fn local_and_remote() -> Self {
            let mut sessions = HashMap::new();
            sessions.insert(
                "local".to_string(),
                vec![Session {
                    source: "local".into(),
                    name: "editor".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                }],
            );
            sessions.insert(
                "remote".to_string(),
                vec![Session {
                    source: "remote".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 50,
                }],
            );
            let mut panes = HashMap::new();
            panes.insert(
                "remote/api".to_string(),
                vec![WindowPanes {
                    index: 1,
                    name: "serve".into(),
                    active: true,
                    panes: vec![Pane {
                        index: 1,
                        active: true,
                        command: "node".into(),
                    }],
                }],
            );
            StreamOps {
                sources: vec!["local".into(), "remote".into()],
                sessions,
                panes,
            }
        }
    }

    #[async_trait::async_trait]
    impl Ops for StreamOps {
        fn sources(&self) -> Vec<String> {
            self.sources.clone()
        }
        async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>> {
            Ok(self.sessions.get(source).cloned().unwrap_or_default())
        }
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
        async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>> {
            Ok(self.panes.get(&s.address()).cloned().unwrap_or_default())
        }
        async fn capture(&self, _source: &str, _target: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn event_loop_streams_source_result() {
        // A `Cmd::SourceResult` flowing through the loop turns a scanning host
        // into its sessions.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::from_sources(vec!["local".into()]);
        let (tx, rx) = mpsc::channel(16);
        tx.send(Cmd::SourceResult {
            source: "local".into(),
            sessions: vec![Session {
                source: "local".into(),
                name: "editor".into(),
                windows: 1,
                attached: false,
                last_attached: 100,
            }],
            err: None,
        })
        .await
        .unwrap();
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Cmd::Dump(reply_tx)).await.unwrap();
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
            "a SourceResult must stream the session into the tree:\n{dump}"
        );
    }

    #[tokio::test]
    async fn event_loop_kicks_probes_on_start() {
        // On start the loop kicks one `list-sessions` probe per source; a reachable
        // host's sessions then get their panes fetched — all streaming in without a
        // key/poll nudge, starting from a bare host skeleton.
        let (tx, rx) = mpsc::channel::<Cmd>(64);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::from_sources(vec!["local".into(), "remote".into()]);
        let ops: Arc<dyn Ops> = Arc::new(StreamOps::local_and_remote());
        let tx2 = tx.clone();
        let loop_task = tokio::spawn(async move {
            event_loop(&mut term, &mut sw, ops, tx2, rx).await.unwrap();
        });

        let mut got_remote = false;
        let mut got_panes = false;
        for _ in 0..200 {
            let (rtx, rrx) = oneshot::channel();
            if tx.send(Cmd::Dump(rtx)).await.is_err() {
                break;
            }
            let dump = rrx.await.unwrap_or_default();
            got_remote |= dump.contains("api");
            got_panes |= dump.contains("window 1: serve");
            if got_remote && got_panes {
                break;
            }
        }
        assert!(
            got_remote,
            "the remote host's sessions must stream in on start"
        );
        assert!(
            got_panes,
            "each reachable session's panes must stream in on start"
        );

        tx.send(Cmd::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
        loop_task.await.unwrap();
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
    async fn event_loop_cancel_leaves_no_choice() {
        // UC-3 / FR-B5: surveying the list and cancelling (q) leaves no chosen
        // session — the current session is untouched.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample());
        let (tx, rx) = mpsc::channel(16);
        tx.send(Cmd::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();
        assert!(
            sw.result().chosen.is_none(),
            "cancel must leave no chosen session"
        );
    }

    #[tokio::test]
    async fn event_loop_filter_then_enter_attaches_visible() {
        // UC-4 / FR-B6, end-to-end through the live event loop: filter to one of
        // several sessions, Enter applies the filter, Enter attaches the VISIBLE
        // (filtered) session — never the attached/most-recent filtered-out one.
        let scan = Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![
                    Session {
                        source: "local".into(),
                        name: "editor".into(),
                        windows: 1,
                        attached: true,
                        last_attached: 999, // most-recent + attached
                    },
                    Session {
                        source: "local".into(),
                        name: "build".into(),
                        windows: 1,
                        attached: false,
                        last_attached: 10,
                    },
                ],
            }],
            panes: Default::default(),
        };
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(scan);
        let (tx, rx) = mpsc::channel(32);
        for k in ['/', 'b', 'u', 'i', 'l', 'd'] {
            tx.send(Cmd::Key(KeyEvent::new(
                KeyCode::Char(k),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
        }
        tx.send(Cmd::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
            .await
            .unwrap(); // apply filter
        tx.send(Cmd::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
            .await
            .unwrap(); // attach the filtered session
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();
        assert_eq!(
            sw.result().chosen.as_ref().map(|s| s.name.as_str()),
            Some("build"),
            "filter+Enter through the loop must attach the visible (filtered) session"
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
    async fn resize_cmd_is_handled_then_quit() {
        let ops: Arc<dyn Ops> = Arc::new(NoopOps);
        let (tx, rx) = mpsc::channel::<Cmd>(16);
        let mut switcher = Switcher::from_sources(ops.sources());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        // a resize then a quit key
        tx.send(Cmd::Resize(100, 40)).await.unwrap();
        tx.send(Cmd::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)))
            .await
            .unwrap();
        let r = event_loop(&mut term, &mut switcher, ops, tx.clone(), rx).await;
        assert!(r.is_ok(), "Cmd::Resize must be handled without error");
    }

    #[tokio::test]
    async fn dump_switcher_flattens_buffer() {
        let mut sw = Switcher::new(sample());
        let out = dump_switcher(&mut sw, 100, 30);
        assert!(out.contains("editor"));
        assert!(out.contains("Hosts · Sessions · Windows · Panes"));
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
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
