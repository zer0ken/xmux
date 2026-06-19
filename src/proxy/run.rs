//! PTY attachment infrastructure: spawn a ConPTY-backed mux-client, tee its
//! output to a `Grid` for the cockpit's terminal view, route input from the
//! cockpit's control thread, and tear down without blocking the async runtime.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::proxy::screen::Grid;

/// RAII guard: enables raw mode on construction and restores it on drop, so the
/// host terminal is left cooked on normal return AND on a panic (release builds
/// unwind). The proxy needs raw mode for the WHOLE session — byte-faithful
/// forwarding can't tolerate the line discipline cooking input — not just while
/// the overlay is up.
pub struct RawGuard;

impl RawGuard {
    pub fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Proxy configuration: the C0 prefix byte and the action byte that, when
/// typed after the prefix, opens the session picker.
pub struct ProxyConfig {
    pub prefix: u8,
    pub action_key: u8,
}

/// Parse an `XMUX_PREFIX`-style spec into a C0 control byte.
///
/// Recognised forms:
/// - `C-<letter>` / `c-<letter>` → 0x01..0x1a
/// - `C-Space` / `c-space`       → 0x00
/// - anything else               → default 0x07 (`C-g`)
pub fn parse_prefix(spec: Option<&str>) -> u8 {
    let s = match spec {
        Some(s) => s.trim(),
        None => return 0x07,
    };
    let rest = s.strip_prefix("C-").or_else(|| s.strip_prefix("c-"));
    match rest {
        Some("Space") | Some("space") => 0x00,
        Some(c) if c.len() == 1 => {
            let ch = c.as_bytes()[0].to_ascii_lowercase();
            if ch.is_ascii_lowercase() {
                ch - b'a' + 1
            } else {
                0x07
            }
        }
        _ => 0x07,
    }
}


/// Write one chunk to stdout, acquiring and releasing the process-global stdout
/// lock within this call. Holding a `StdoutLock` across the pump's blocking read
/// (or while the picker overlay draws from another task) deadlocks the picker;
/// scoping the lock to the write is the contract that prevents C1.
fn pump_write(out: &std::io::Stdout, chunk: &[u8]) {
    let mut lock = out.lock();
    let _ = lock.write_all(chunk);
    let _ = lock.flush();
}

/// Detects an erase-display sequence `ESC [ <digits>? J` in `chunk`, carrying any
/// trailing unfinished CSI in `tail` so a sequence split across read chunks is
/// still caught. The owner pump re-emits the status bar whenever this returns
/// true (a child sized cols×(rows-1) only reaches the status row via a full
/// clear). The tail is bounded so a stream of bare ESCs cannot grow it.
fn scan_clear(tail: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let mut buf = std::mem::take(tail);
    buf.extend_from_slice(chunk);
    let mut found = false;
    let mut i = 0;
    let mut keep_from = buf.len();
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            keep_from = i; // lone trailing ESC: could begin a CSI next chunk
            break;
        }
        if buf[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < buf.len() && buf[j].is_ascii_digit() {
            j += 1;
        }
        if j >= buf.len() {
            keep_from = i; // unfinished params: resume from this ESC next chunk
            break;
        }
        if buf[j] == b'J' {
            found = true;
        }
        i = j + 1;
    }
    let mut rest = buf.split_off(keep_from);
    if rest.len() > 16 {
        let n = rest.len() - 16;
        rest.drain(0..n);
    }
    *tail = rest;
    found
}

/// A command for the dedicated PTY control thread: bytes to write to the child,
/// or a resize to apply to the master. The async runtime is single-threaded, so
/// routing every blocking PTY write/resize through this channel keeps a slow
/// child (e.g. a stalled ssh remote that drains input slowly) from freezing
/// rendering, output streaming, and the picker hotkey on the event loop.
pub enum PtyCmd {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
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
/// which owns the master, so returning drops it here. Closing the ConPTY can
/// block on Windows older than 24H2 (`ClosePseudoConsole` waits for clients to
/// disconnect) — but only this control thread can stall on that, never the async
/// runtime or the caller's return. In the common teardown path the output pump
/// is still draining the read pipe, which lets the close complete.
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

// ---------------------------------------------------------------------------
// LiveOwner — shared "who owns raw stdout" token
// ---------------------------------------------------------------------------

/// The sentinel value meaning ratatui owns the terminal; no pump writes stdout.
const OVERLAY: u64 = u64::MAX;

/// The single shared "who owns raw stdout" token. A pump writes stdout iff it is
/// the current owner. The sentinel `OVERLAY` means ratatui owns stdout and NO
/// pump writes. This generalizes the old `overlay_active: AtomicBool` to a
/// per-Attachment owner check; exactly one writer touches real stdout at a time.
#[derive(Clone)]
pub struct LiveOwner(Arc<AtomicU64>);

impl LiveOwner {
    pub fn new() -> Self {
        LiveOwner(Arc::new(AtomicU64::new(OVERLAY)))
    }
    /// Make Attachment `id` the foreground stdout owner (Passthrough).
    pub fn set_owner(&self, id: u64) {
        self.0.store(id, Ordering::Release);
    }
    /// Enter Overlay: no pump writes stdout (ratatui owns it).
    pub fn set_overlay(&self) {
        self.0.store(OVERLAY, Ordering::Release);
    }
    /// Whether Attachment `id` may write raw stdout right now.
    pub fn is_owner(&self, id: u64) -> bool {
        self.0.load(Ordering::Acquire) == id
    }
}

impl Default for LiveOwner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Attachment — one kept live attach
// ---------------------------------------------------------------------------

/// One kept live attach: a ConPTY + attach child + dedicated control thread
/// (owns writer+master) + output pump + a shared `Grid`. The pump feeds the grid
/// always and writes raw stdout iff this attachment is the `LiveOwner`.
pub struct Attachment {
    pub grid: Arc<Mutex<Grid>>,
    pub control_tx: std::sync::mpsc::Sender<PtyCmd>,
    pub size: (u16, u16),
    pub last_used: Instant,
    /// The Passthrough status-bar bytes the owner pump re-emits after a child
    /// full-screen clear (empty until the cockpit sets it on Passthrough entry).
    pub status_bar: Arc<Mutex<Vec<u8>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    id: u64,
}

impl Attachment {
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Set the status-bar bytes the owner pump re-emits after a clear sequence.
    pub fn set_status_bar(&self, bytes: Vec<u8>) {
        if let Ok(mut g) = self.status_bar.lock() {
            *g = bytes;
        }
    }
    /// Queue input bytes to the child (FIFO, off the loop).
    pub fn input(&self, bytes: Vec<u8>) {
        let _ = self.control_tx.send(PtyCmd::Input(bytes));
    }
    /// Queue a resize to the child PTY (off the loop) and resize the grid.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.control_tx.send(PtyCmd::Resize { cols, rows });
        if let Ok(mut g) = self.grid.lock() {
            g.resize(rows, cols);
        }
    }
    /// Tear down: kill the child, drop the control sender so the control thread
    /// drops the master on ITS thread and exits. The pump exits on master EOF.
    pub fn teardown(mut self) {
        let _ = self.child.kill();
        drop(self.control_tx);
        // child + remaining fields drop here; the control thread owns the master.
    }
}

/// Opens a PTY at `cols×rows`, spawns `argv` with mux nesting-guard env cleared,
/// starts the control thread (owns writer+master) and the output pump (always
/// feeds the grid; writes raw stdout iff `live.is_owner(id)`; sends `id` on
/// `eof_tx` at master EOF so the registry can reap it).
pub fn spawn_attachment(
    argv: &[String],
    cols: u16,
    rows: u16,
    id: u64,
    live: LiveOwner,
    stdout: std::io::Stdout,
    eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
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
    cmd.env_remove("PSMUX_SESSION");
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
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
    let pump_grid = grid.clone();
    let status_bar = Arc::new(Mutex::new(Vec::<u8>::new()));
    let pump_status = status_bar.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Rolling tail so a clear sequence split across reads is still caught.
        let mut clear_tail: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Ok(mut g) = pump_grid.lock() {
                        g.feed(chunk);
                    }
                    if live.is_owner(id) {
                        pump_write(&stdout, chunk);
                        // The child is sized cols×(rows-1); only a full-screen
                        // clear reaches the status row, so re-emit the bar after
                        // one. The bar wraps itself in cursor save/restore, so the
                        // re-emit is transparent to the child.
                        if scan_clear(&mut clear_tail, chunk) {
                            let sb = pump_status.lock().map(|g| g.clone()).unwrap_or_default();
                            if !sb.is_empty() {
                                pump_write(&stdout, &sb);
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let _ = eof_tx.send(id); // master EOF: ask the registry to reap us
    });

    Ok(Attachment {
        grid,
        control_tx,
        size: (cols, rows),
        last_used: Instant::now(),
        status_bar,
        child,
        id,
    })
}

// ---------------------------------------------------------------------------
// Test-only helpers for Task 5's headless registry tests.
// ---------------------------------------------------------------------------

/// A fake `portable_pty::Child` for use in tests that need an `Attachment`
/// without a real ConPTY.
///
/// `portable_pty::Child` in 0.9 requires `ChildKiller + Downcast + Send`.
/// `Downcast` is satisfied by `downcast_rs`'s blanket impl for any `'static` type.
/// `ChildKiller` requires `kill` + `clone_killer` (plus `Debug + Downcast + Send`).
/// `Child` requires `try_wait`, `wait`, `process_id`, and (Windows) `as_raw_handle`.
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

/// Constructs an `Attachment` backed by a `DummyChild` and a no-op control
/// channel. Used by Task 5's headless registry tests.
#[cfg(test)]
pub fn fake_attachment(id: u64, last_used: Instant) -> Attachment {
    let (control_tx, _control_rx) = std::sync::mpsc::channel::<PtyCmd>();
    Attachment {
        grid: Arc::new(Mutex::new(Grid::new(24, 80))),
        control_tx,
        size: (80, 24),
        last_used,
        status_bar: Arc::new(Mutex::new(Vec::new())),
        child: Box::new(DummyChild),
        id,
    }
}

// ---------------------------------------------------------------------------
// Headless smoke-test helper (used by the #[ignore] test below).
// ---------------------------------------------------------------------------

/// Spawns cmd.exe on a PTY slave, writes a command, waits for the marker to
/// round-trip through the master. Mirrors tmp/pty-spike/src/proxy.rs exactly.
#[cfg(test)]
pub fn smoke_roundtrip() -> bool {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    const MARKER: &str = "PROXY_SMOKE_54321";

    let pty = native_pty_system();
    let pair = match pty.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let cmd = CommandBuilder::new("cmd.exe");
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(_) => return false,
    };
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => return false,
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(_) => return false,
    };

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => { if tx.send(buf[..n].to_vec()).is_err() { break; } }
                Err(_) => break,
            }
        }
    });

    let _ = writer.write_all(format!("echo {MARKER}\r\n").as_bytes());
    let _ = writer.flush();

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut collected: Vec<u8> = Vec::new();
    let mut answered_dsr = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !answered_dsr && collected.windows(4).any(|w| w == b"\x1b[6n") {
            let _ = writer.write_all(b"\x1b[1;1R");
            let _ = writer.flush();
            answered_dsr = true;
        }
        if String::from_utf8_lossy(&collected).contains(MARKER) || Instant::now() > deadline {
            break;
        }
    }
    let _ = writer.write_all(b"exit\r\n");
    let _ = writer.flush();
    let _ = child.kill();
    // Do NOT join pump / drop master (ConPTY read won't return).
    let _keep_master = pair.master;
    let _ = pump;

    String::from_utf8_lossy(&collected).contains(MARKER)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn scan_clear_detects_ed_in_one_and_across_chunks() {
        // Whole sequence in one chunk.
        let mut tail = Vec::new();
        assert!(scan_clear(&mut tail, b"text\x1b[2Jmore"));

        // Split across two chunks: `ESC [ 2` then `J`.
        let mut tail = Vec::new();
        assert!(!scan_clear(&mut tail, b"abc\x1b[2"), "params unfinished: not yet");
        assert!(scan_clear(&mut tail, b"J xyz"), "completes on the next chunk");

        // `ESC [ J` (no param) is also an erase-display.
        let mut tail = Vec::new();
        assert!(scan_clear(&mut tail, b"\x1b[J"));

        // A non-ED CSI (colour) must NOT trigger a repaint.
        let mut tail = Vec::new();
        assert!(!scan_clear(&mut tail, b"\x1b[31m colored \x1b[0m"));
    }

    #[test]
    fn parse_prefix_recognises_specs_and_defaults() {
        assert_eq!(parse_prefix(Some("C-g")), 0x07);
        assert_eq!(parse_prefix(Some("C-Space")), 0x00);
        assert_eq!(parse_prefix(Some("c-a")), 0x01);
        assert_eq!(parse_prefix(None), 0x07);
        assert_eq!(parse_prefix(Some("garbage")), 0x07);
    }

    // -- B1: PTY control thread ------------------------------------------
    //
    // The async runtime is single-threaded, so a blocking write to a slow child
    // (or a blocking ConPTY resize) on the event loop freezes rendering, output
    // streaming, and the picker hotkey. `pty_control_loop` moves those blocking
    // ops onto a dedicated thread fed over a channel. This proves the loop writes
    // input bytes IN ORDER, applies resizes, and exits cleanly when the channel
    // closes (so teardown drops the master off the runtime thread).
    #[test]
    fn pty_control_loop_writes_in_order_resizes_and_exits() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};

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

    // -- C1 regression: the pump must not hold the stdout lock -------------
    //
    // Before the fix the pump bound a long-lived `StdoutLock`, so the picker's
    // draw (a `std::io::stdout()` write/lock from another thread) blocked
    // forever and the first `Ctrl-g s` hung the process. This proves the FIXED
    // per-write pattern (`pump_write`) does NOT block a concurrent stdout lock
    // from another thread.
    //
    // It can NEVER hang the suite: the writer self-terminates after N iterations,
    // the prober signals over a channel, the main thread waits with a 2s
    // recv_timeout, and a watchdog flips an AtomicBool that stops the writer
    // regardless — so every spawned thread returns and the test always finishes.
    #[test]
    fn pump_write_does_not_hold_stdout_lock_across_threads() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc::{self, RecvTimeoutError};

        let stop = Arc::new(AtomicBool::new(false));

        // Writer thread: emulate the pump's per-write step. Writes EMPTY bytes
        // (no test-output pollution) through the fixed acquire-write-release
        // helper, then exits after a bounded iteration count or when stopped.
        let writer_stop = stop.clone();
        let writer = std::thread::spawn(move || {
            let out = std::io::stdout();
            for _ in 0..50 {
                if writer_stop.load(Ordering::Acquire) {
                    break;
                }
                pump_write(&out, b"");
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        // Prober thread: a held pump lock (the C1 bug) would block this lock
        // acquisition forever; with the fix it returns promptly and signals.
        let (probe_tx, probe_rx) = mpsc::channel::<()>();
        let prober = std::thread::spawn(move || {
            let out = std::io::stdout();
            {
                let mut lock = out.lock();
                let _ = lock.write_all(b"");
            }
            let _ = probe_tx.send(());
        });

        // Watchdog: guarantees the writer stops even if anything stalls, so no
        // thread can outlive the test.
        let watchdog_stop = stop.clone();
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            watchdog_stop.store(true, Ordering::Release);
        });

        let probed = probe_rx.recv_timeout(Duration::from_secs(2));

        // Ensure everything winds down regardless of outcome.
        stop.store(true, Ordering::Release);
        let _ = writer.join();
        let _ = prober.join();
        let _ = watchdog.join();

        assert!(
            matches!(probed, Ok(())),
            "concurrent stdout lock timed out — the pump is holding the lock (C1 regression); err={:?}",
            probed.err().unwrap_or(RecvTimeoutError::Disconnected)
        );
    }

    // Smoke test: requires a console; run on demand with
    //   cargo test -p xmux proxy::run::tests::smoke -- --ignored --nocapture
    #[ignore]
    #[test]
    fn smoke_child_runs_under_proxy() {
        // mirrors tmp/pty-spike: spawn cmd.exe on a slave, write a command,
        // read the marker back. Asserts bytes round-trip through the proxy PTY.
        assert!(super::smoke_roundtrip());
    }

    // -- Task 4: LiveOwner gate ----------------------------------------------

    #[test]
    fn live_owner_gates_by_id_and_overlay() {
        let live = LiveOwner::new();
        live.set_owner(7);
        assert!(live.is_owner(7));
        assert!(!live.is_owner(3), "a non-owner id must not write stdout");
        live.set_overlay();
        assert!(!live.is_owner(7), "in Overlay NO id is the owner");
        assert!(!live.is_owner(3));
        live.set_owner(3);
        assert!(live.is_owner(3));
        assert!(!live.is_owner(7));
    }

    // The full spawn_attachment path needs a real ConPTY (a console), so it is an
    // #[ignore] smoke test driven on demand; the gate logic above is the headless
    // unit. spawn_attachment is exercised live in Task 14.
    #[ignore]
    #[test]
    fn spawn_attachment_feeds_grid_smoke() {
        let live = LiveOwner::new();
        live.set_overlay(); // do not write the test console's stdout
        let (eof_tx, _eof_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let argv: Vec<String> = vec!["cmd.exe".into()];
        let att = spawn_attachment(&argv, 80, 24, 1, live, std::io::stdout(), eof_tx)
            .expect("spawn");
        att.input(b"echo SMOKE\r\n".to_vec());
        std::thread::sleep(Duration::from_millis(800));
        let g = att.grid.lock().unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
        drop(g);
        att.teardown();
    }
}
