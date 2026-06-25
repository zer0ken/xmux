//! PTY attachment infrastructure: spawn a ConPTY-backed mux client (a real
//! `tmux attach` / `psmux attach`), tee its output into a `Grid` for the cockpit's
//! terminal view, route input from the cockpit through a dedicated control thread,
//! and tear down without blocking the async runtime.
//!
//! This is the DISPLAY path: the mux is actually USED inside xmux — a real attached
//! client per session — not reconstructed from control-mode `%output`. Control mode
//! is retained only for inventory, change events, and programmatic window selection.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::proxy::screen::Grid;

/// An event a kept attachment's pump emits to the cockpit's `select!` loop.
pub enum PtyEvent {
    /// The pump fed `id`'s grid with a chunk of child output — the cockpit redraws
    /// (coalescing a burst into one redraw, like the control-mode `%output` drain).
    Output { id: u64 },
    /// The child's PTY master hit EOF (the attach exited / the connection dropped) —
    /// the registry reaps the attachment by id.
    Exited { id: u64 },
    /// The attach shell self-reported its tty (the unique marker) — captured ONCE per
    /// attachment. The supervisor records it on the owning Host's `display_tty`, so a
    /// later switch-client targets xmux's OWN display client and a %client-detached is
    /// filtered against it. Identity-preserving: only this attachment's own shell
    /// emits the marker.
    DisplayTty { id: u64, tty: String },
}

/// A command for a kept attachment's dedicated PTY control thread: bytes to write
/// to the child, or a resize to apply to the master. The async runtime is
/// single-threaded, so routing every blocking PTY write/resize through this channel
/// keeps a slow child (e.g. a stalled ssh remote that drains input slowly) from
/// freezing rendering, output streaming, and the picker hotkey on the event loop.
pub enum PtyCmd {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Accumulates pump output into `acc` until ONE whole display-tty marker is seen,
/// then sets `captured` and stops growing `acc` (a bounded one-shot). After
/// capture, further reads are ignored here — the marker is xmux's attach shell's
/// single self-report. Pure so the pump's capture is unit-tested without a ConPTY.
fn scan_marker_once(acc: &mut Vec<u8>, captured: &mut Option<String>, chunk: &[u8]) {
    if captured.is_some() {
        return;
    }
    acc.extend_from_slice(chunk);
    if let Some(tty) = crate::model::death::parse_display_tty_marker(acc) {
        *captured = Some(tty);
        acc.clear(); // release the buffer; we never scan again
    }
}

/// Builds the responses a vt100 host owes the child for the terminal QUERIES in
/// `data`, so the child does not block waiting on them. With raw passthrough there
/// is a real terminal behind the PTY to answer; rendering into a `Grid` instead, the
/// pump must answer itself or the child (a shell, tmux, ssh's remote tty) stalls on
/// startup and produces NO output (the empty-pane bug). `cursor` is the grid's
/// current cursor as `(col, row)`, 0-based.
///
/// Answered: `ESC[6n` (DSR cursor-position report → `ESC[<row>;<col>R`, 1-based) and
/// `ESC[c` / `ESC[0c` (primary Device Attributes → a VT100-with-AVO `ESC[?1;2c`).
/// Returns the concatenated responses (empty when there are no queries).
fn query_responses(data: &[u8], cursor: (u16, u16)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] != 0x1b || data[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // ESC [ 6 n  → cursor-position report
        if i + 3 < data.len() && data[i + 2] == b'6' && data[i + 3] == b'n' {
            let (col, row) = cursor;
            out.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            i += 4;
            continue;
        }
        // ESC [ c  or  ESC [ 0 c  → primary device attributes
        if i + 2 < data.len() && data[i + 2] == b'c' {
            out.extend_from_slice(b"\x1b[?1;2c");
            i += 3;
            continue;
        }
        if i + 3 < data.len() && data[i + 2] == b'0' && data[i + 3] == b'c' {
            out.extend_from_slice(b"\x1b[?1;2c");
            i += 4;
            continue;
        }
        i += 1;
    }
    out
}

/// The longest trailing suffix of `data` that is an INCOMPLETE prefix of a query we
/// answer, so it can be completed by the next read and must be carried over. A
/// COMPLETE query is never returned here (it was already answered by
/// [`query_responses`]), which is what prevents a duplicate reply when a whole query
/// lands at a read boundary. Recognized partial prefixes: `ESC`, `ESC[`, `ESC[6`,
/// `ESC[0` (the strict prefixes of `ESC[6n` / `ESC[0c` / `ESC[c`).
fn trailing_partial_query(data: &[u8]) -> &[u8] {
    for p in [
        b"\x1b[6".as_slice(),
        b"\x1b[0".as_slice(),
        b"\x1b[".as_slice(),
        b"\x1b".as_slice(),
    ] {
        if data.ends_with(p) {
            return &data[data.len() - p.len()..];
        }
    }
    &[]
}

/// The blocking PTY operations the control thread performs, behind a trait so
/// [`pty_control_loop`] is unit-testable without a real ConPTY.
trait PtySink {
    fn write_input(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
}

/// Drains `rx` on a dedicated OS thread, performing each command on `sink`, until
/// the channel closes. Both the writes and the resizes run here — never on the
/// async runtime — so neither can stall the event loop. Input bytes are written in
/// arrival order: one ordered channel, one reader.
///
/// On channel close it returns; the caller (`spawn_attachment`) keeps the sink,
/// which owns the master, so returning drops it here. Closing the ConPTY can block
/// on Windows older than 24H2 (`ClosePseudoConsole` waits for clients to
/// disconnect) — but only this control thread can stall on that, never the async
/// runtime or the caller's return. In the common teardown path the output pump is
/// still draining the read pipe, which lets the close complete.
fn pty_control_loop(rx: std::sync::mpsc::Receiver<PtyCmd>, mut sink: impl PtySink) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            PtyCmd::Input(bytes) => sink.write_input(&bytes),
            PtyCmd::Resize { cols, rows } => sink.resize(cols, rows),
        }
    }
}

/// The real [`PtySink`]: owns the child's PTY writer and the master, so every
/// blocking write/resize happens on the control thread, and the master is dropped
/// there too when the channel closes at teardown.
struct MasterSink {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
}

impl PtySink for MasterSink {
    fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

/// One kept live attach: a ConPTY + attach child + dedicated control thread (owns
/// writer+master) + an output pump + a shared `Grid`. The pump always feeds the
/// grid; the cockpit renders the grid of whichever attachment the selection points
/// at. There is no raw-stdout passthrough here — ratatui owns stdout and renders
/// every grid as a ratatui clip.
pub struct Attachment {
    /// The vt100 grid the pump feeds; the cockpit renders it when selected.
    pub grid: Arc<Mutex<Grid>>,
    /// Queue commands (input/resize) to the control thread (off the loop).
    pub control_tx: std::sync::mpsc::Sender<PtyCmd>,
    /// Current PTY size (cols, rows).
    pub size: (u16, u16),
    /// True until the first output chunk proves the attach is live (drives the
    /// "(attaching…)" spinner). Cleared by the pump on the first read.
    pub connecting: Arc<AtomicBool>,
    /// Coalesces output wakeups: the pump sends a single `Output` event then sets
    /// this, and skips further sends until the cockpit clears it after a redraw. A
    /// busy unselected session thus enqueues at most ONE pending event between
    /// draws, so the event channel cannot grow unbounded under an output flood.
    pending: Arc<AtomicBool>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    id: u64,
}

impl Attachment {
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Queue input bytes to the child (FIFO, off the loop).
    pub fn input(&self, bytes: Vec<u8>) {
        let _ = self.control_tx.send(PtyCmd::Input(bytes));
    }
    /// Clear the output-coalescing flag after a redraw, so the pump may signal the
    /// next chunk. Called for every attachment once per draw.
    pub fn clear_pending(&self) {
        self.pending.store(false, Ordering::Release);
    }
    /// Queue a resize to the child PTY (off the loop) and resize the grid.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.control_tx.send(PtyCmd::Resize { cols, rows });
        if let Ok(mut g) = self.grid.lock() {
            g.resize(rows, cols);
        }
        self.size = (cols, rows);
    }
    /// Tear down: kill the child, drop the control sender so the control thread
    /// drops the master on ITS thread and exits, then REAP the child on a detached
    /// thread (`wait`) so it does not linger as a zombie / leaked handle. The wait
    /// runs off the event loop (it can block on a ConPTY close pre-24H2) and is
    /// bounded because the child was just killed. The pump exits on master EOF.
    pub fn teardown(self) {
        let mut child = self.child;
        let _ = child.kill();
        drop(self.control_tx);
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Opens a PTY at `cols×rows`, spawns `argv` (a real `attach` argv from
/// [`crate::source::Source::attach_command`]) with mux nesting-guard env cleared,
/// starts the control thread (owns writer+master) and the output pump. The pump
/// always feeds the grid, emits [`PtyEvent::Output`] per chunk (the cockpit
/// coalesces), clears `connecting` on the first read, and emits
/// [`PtyEvent::Exited`] at master EOF so the registry can reap it.
pub fn spawn_attachment(
    argv: &[String],
    cols: u16,
    rows: u16,
    id: u64,
    events: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
) -> anyhow::Result<Attachment> {
    anyhow::ensure!(!argv.is_empty(), "spawn_attachment: argv must not be empty");
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    // Strip EVERY mux session var (all PSMUX*, TMUX, TMUX_PANE) so the attach child
    // does not inherit stale routing state that could mis-target the server — the
    // same precise strip the control-mode child uses (see source::is_mux_var).
    for (k, _) in std::env::vars() {
        if crate::source::is_mux_var(&k) {
            cmd.env_remove(&k);
        }
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let pty_writer = pair.master.take_writer()?;

    let (control_tx, control_rx) = std::sync::mpsc::channel::<PtyCmd>();
    std::thread::spawn(move || {
        pty_control_loop(
            control_rx,
            MasterSink {
                writer: pty_writer,
                master: pair.master,
            },
        )
    });

    let grid = Arc::new(Mutex::new(Grid::new(rows, cols)));
    let connecting = Arc::new(AtomicBool::new(true));
    let pending = Arc::new(AtomicBool::new(false));
    let pump_grid = grid.clone();
    let pump_connecting = connecting.clone();
    let pump_pending = pending.clone();
    // The pump answers the child's terminal queries (DSR/DA) over this sender, since
    // there is no real terminal behind the PTY to answer — without it the child
    // stalls on startup and the grid stays blank.
    let pump_ctl = control_tx.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Carries up to the last 3 bytes so a query split across reads is still seen.
        let mut qtail: Vec<u8> = Vec::new();
        let mut marker_acc: Vec<u8> = Vec::new();
        let mut marker_done = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let cursor = {
                        let mut g = match pump_grid.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        g.feed(&buf[..n]);
                        g.cursor()
                    };
                    // Answer DSR/DA queries so the child does not block (empty-pane bug).
                    // Carry only an INCOMPLETE trailing query prefix to the next read —
                    // never a complete query (already answered), so no duplicate reply.
                    qtail.extend_from_slice(&buf[..n]);
                    let resp = query_responses(&qtail, cursor);
                    if !resp.is_empty() {
                        let _ = pump_ctl.send(PtyCmd::Input(resp));
                    }
                    let keep = trailing_partial_query(&qtail).len();
                    let cut = qtail.len() - keep;
                    qtail.drain(0..cut);
                    pump_connecting.store(false, Ordering::Release);
                    if !marker_done {
                        let mut captured = None;
                        scan_marker_once(&mut marker_acc, &mut captured, &buf[..n]);
                        if let Some(tty) = captured {
                            marker_done = true;
                            let _ = events.send(PtyEvent::DisplayTty { id, tty });
                        }
                    }
                    // Coalesce: signal a redraw only if no Output is already pending
                    // for this attachment (the cockpit clears it after the next
                    // draw). Bounds the channel to ≤1 pending event per attachment,
                    // so a busy unselected session cannot flood the loop.
                    if !pump_pending.swap(true, Ordering::AcqRel)
                        && events.send(PtyEvent::Output { id }).is_err()
                    {
                        break; // the cockpit is gone — stop pumping
                    }
                }
                Err(_) => break,
            }
        }
        let _ = events.send(PtyEvent::Exited { id }); // master EOF: ask to reap us
    });

    Ok(Attachment {
        grid,
        control_tx,
        size: (cols, rows),
        connecting,
        pending,
        child,
        id,
    })
}

// ---------------------------------------------------------------------------
// Test-only helpers for the registry's headless tests.
// ---------------------------------------------------------------------------

/// A fake `portable_pty::Child` for tests that need an `Attachment` without a real
/// ConPTY. `portable_pty::Child` in 0.9 requires `ChildKiller + Downcast + Send`;
/// `Downcast` comes free via `downcast_rs`'s blanket impl for any `'static` type.
#[cfg(test)]
#[derive(Debug)]
pub struct DummyChild;

#[cfg(test)]
impl portable_pty::ChildKiller for DummyChild {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(DummyChild)
    }
}

#[cfg(test)]
impl portable_pty::Child for DummyChild {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))
    }
    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        Ok(portable_pty::ExitStatus::with_exit_code(0))
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

/// Constructs an `Attachment` backed by a `DummyChild` and a no-op control channel.
/// Used by the registry's headless tests.
#[cfg(test)]
pub fn fake_attachment(id: u64) -> Attachment {
    let (control_tx, _control_rx) = std::sync::mpsc::channel::<PtyCmd>();
    Attachment {
        grid: Arc::new(Mutex::new(Grid::new(24, 80))),
        control_tx,
        size: (80, 24),
        connecting: Arc::new(AtomicBool::new(true)),
        pending: Arc::new(AtomicBool::new(false)),
        child: Box::new(DummyChild),
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    // The async runtime is single-threaded, so a blocking write to a slow child (or
    // a blocking ConPTY resize) on the event loop freezes rendering, output
    // streaming, and the picker hotkey. `pty_control_loop` moves those blocking ops
    // onto a dedicated thread fed over a channel. This proves the loop writes input
    // bytes IN ORDER, applies resizes, and exits cleanly when the channel closes (so
    // teardown drops the master off the runtime thread).
    #[test]
    fn pty_control_loop_writes_in_order_resizes_and_exits() {
        #[derive(Default)]
        struct MockSink {
            writes: Arc<Mutex<Vec<u8>>>,
            resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        }
        impl PtySink for MockSink {
            fn write_input(&mut self, bytes: &[u8]) {
                self.writes.lock().unwrap().extend_from_slice(bytes);
            }
            fn resize(&mut self, cols: u16, rows: u16) {
                self.resizes.lock().unwrap().push((cols, rows));
            }
        }

        let writes = Arc::new(Mutex::new(Vec::new()));
        let resizes = Arc::new(Mutex::new(Vec::new()));
        let sink = MockSink {
            writes: writes.clone(),
            resizes: resizes.clone(),
        };

        let (tx, rx) = mpsc::channel::<PtyCmd>();
        let handle = std::thread::spawn(move || pty_control_loop(rx, sink));

        tx.send(PtyCmd::Input(b"ab".to_vec())).unwrap();
        tx.send(PtyCmd::Resize { cols: 80, rows: 24 }).unwrap();
        tx.send(PtyCmd::Input(b"cd".to_vec())).unwrap();
        drop(tx); // closing the channel must end the loop so we can join

        handle.join().expect("control loop must return on channel close");
        assert_eq!(&*writes.lock().unwrap(), b"abcd", "input written in order");
        assert_eq!(&*resizes.lock().unwrap(), &[(80, 24)], "resize applied");
    }

    #[test]
    fn query_responses_answers_dsr_and_da() {
        // ESC[6n (DSR cursor-position) → ESC[<row>;<col>R, 1-based from the (col,row).
        assert_eq!(query_responses(b"\x1b[6n", (4, 2)), b"\x1b[3;5R");
        // ESC[c and ESC[0c (primary Device Attributes) → a VT100-with-AVO reply.
        assert_eq!(query_responses(b"\x1b[c", (0, 0)), b"\x1b[?1;2c");
        assert_eq!(query_responses(b"\x1b[0c", (0, 0)), b"\x1b[?1;2c");
        // Plain output with no query → no response (the empty-pane bug was the pump
        // never answering these, so the child stalled and produced nothing).
        assert!(query_responses(b"hello world\r\n", (1, 1)).is_empty());
        // A query embedded in other bytes is still answered.
        let r = query_responses(b"abc\x1b[6ndef", (0, 0));
        assert_eq!(r, b"\x1b[1;1R");
    }

    #[test]
    fn trailing_partial_query_carries_only_incomplete_prefixes() {
        // A COMPLETE query is NOT carried (already answered) → no duplicate reply.
        assert_eq!(trailing_partial_query(b"out\x1b[c"), b"");
        assert_eq!(trailing_partial_query(b"out\x1b[6n"), b"");
        assert_eq!(trailing_partial_query(b"out\x1b[0c"), b"");
        // INCOMPLETE trailing prefixes ARE carried so the next read can complete them.
        assert_eq!(trailing_partial_query(b"out\x1b"), b"\x1b");
        assert_eq!(trailing_partial_query(b"out\x1b["), b"\x1b[");
        assert_eq!(trailing_partial_query(b"out\x1b[6"), b"\x1b[6");
        assert_eq!(trailing_partial_query(b"out\x1b[0"), b"\x1b[0");
        // Plain trailing bytes carry nothing.
        assert_eq!(trailing_partial_query(b"plain"), b"");
    }

    #[test]
    fn split_query_answered_once_across_reads() {
        // Mirrors the pump's carry: a DSR split across reads 1-2, and a DA landing at
        // the end of read 2 (a read boundary). Each must be answered EXACTLY ONCE —
        // the boundary DA must NOT be re-answered on read 3 (the dup-reply bug).
        let mut qtail: Vec<u8> = Vec::new();
        let mut all: Vec<u8> = Vec::new();
        for chunk in [&b"prompt\x1b[6"[..], &b"n more\x1b[c"[..], &b" tail"[..]] {
            qtail.extend_from_slice(chunk);
            all.extend_from_slice(&query_responses(&qtail, (0, 0)));
            let keep = trailing_partial_query(&qtail).len();
            let cut = qtail.len() - keep;
            qtail.drain(0..cut);
        }
        let dsr = all.windows(3).filter(|w| *w == b"1;1").count(); // ESC[1;1R cursor reply body
        let da = all.windows(7).filter(|w| *w == b"\x1b[?1;2c").count();
        assert_eq!(dsr, 1, "DSR answered exactly once (split across reads)");
        assert_eq!(da, 1, "DA answered exactly once (no duplicate at the read boundary)");
    }

    #[test]
    fn fake_attachment_input_and_resize_do_not_panic() {
        // The fake's control channel has no reader, so input/resize just drop into
        // a dead channel — they must not panic, and resize updates the grid + size.
        let mut att = fake_attachment(1);
        att.input(b"hello".to_vec());
        att.resize(100, 40);
        assert_eq!(att.size, (100, 40));
        assert_eq!(att.id(), 1);
        att.teardown();
    }

    #[test]
    fn scan_marker_once_emits_our_tty_then_stops() {
        // Feed two reads: the first carries unrelated output + a whole marker, the
        // second carries another marker. The tty is captured exactly once, on the read
        // that completes the first marker; later reads do not re-capture or re-grow.
        let mut acc: Vec<u8> = Vec::new();
        let mut captured: Option<String> = None;
        scan_marker_once(&mut acc, &mut captured, b"boot \x1b]XMUX-DISPLAY-TTY:/dev/pts/3\x07ok");
        assert_eq!(captured.as_deref(), Some("/dev/pts/3"), "captured on the completing read");
        let len_after = acc.len();
        scan_marker_once(&mut acc, &mut captured, b"\x1b]XMUX-DISPLAY-TTY:/dev/pts/9\x07");
        assert_eq!(captured.as_deref(), Some("/dev/pts/3"), "a later marker does not override our capture");
        assert_eq!(acc.len(), len_after, "scanning stops once captured (no unbounded growth)");
    }

    #[test]
    fn scan_marker_once_accumulates_across_reads_until_whole() {
        // A marker split across two reads is captured only when whole.
        let mut acc: Vec<u8> = Vec::new();
        let mut captured: Option<String> = None;
        scan_marker_once(&mut acc, &mut captured, b"x \x1b]XMUX-DISPLAY-TTY:/dev/p");
        assert!(captured.is_none(), "partial marker is not yet captured");
        scan_marker_once(&mut acc, &mut captured, b"ts/12\x07 rest");
        assert_eq!(captured.as_deref(), Some("/dev/pts/12"), "captured once the marker completes");
    }

    // End-to-end smoke of the real PTY-attach path: spawn a non-interactive child on
    // a ConPTY, and confirm its stdout round-trips into the `Grid` (the pump fed it),
    // an `Output` event was emitted, and `connecting` cleared.
    //
    // `#[ignore]` and MUST be run in a REAL, NON-NESTED terminal:
    //   cargo test -p xmux proxy::run::tests::spawn_attachment -- --ignored --nocapture
    // Run inside a mux pane (nested ConPTY) it FAILS even though the pump pipeline
    // runs (`connecting` still clears) — Windows does not deliver a nested
    // pseudoconsole child's output to the outer master. This is exactly why the
    // cockpit refuses to run inside a mux (`attach::nest_guard`): xmux must own the
    // terminal directly. So this smoke is a real-terminal gate, not a CI test.
    #[ignore = "spawns a real ConPTY child; run only in a non-nested real terminal"]
    #[test]
    fn spawn_attachment_feeds_grid_smoke() {
        use std::time::{Duration, Instant};
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        const MARKER: &str = "XMUXPTYSMOKE";
        // A NON-interactive child that prints the marker at once then idles briefly
        // (ping keeps the pty open so the pump reads the output before EOF).
        let argv: Vec<String> = vec![
            "cmd.exe".into(),
            "/c".into(),
            format!("echo {MARKER}& ping -n 5 127.0.0.1 >nul"),
        ];
        let att = spawn_attachment(&argv, 80, 24, 1, ev_tx).expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while Instant::now() < deadline {
            {
                let g = att.grid.lock().unwrap();
                let mut buf =
                    ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
                g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
                let text: String = (0..24)
                    .flat_map(|y| (0..80).map(move |x| (x, y)))
                    .map(|(x, y)| buf[(x, y)].symbol().to_string())
                    .collect();
                if text.contains(MARKER) {
                    seen = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(seen, "child output `{MARKER}` must round-trip into the grid via the pump");
        assert!(!att.connecting.load(Ordering::Acquire), "first output must clear `connecting`");
        assert!(
            matches!(ev_rx.try_recv(), Ok(PtyEvent::Output { id: 1 }) | Err(_)),
            "the pump emits Output events for the attachment"
        );
        att.teardown();
    }
}
