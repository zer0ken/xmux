//! PTY proxy loop: own a ConPTY, spawn the attach child on its slave (with the
//! mux-nesting env cleared), pump child output (tee → Grid, overlay-gated
//! stdout write), forward host input through the InputMachine, handle terminal
//! resize, and tear down without blocking.
//!
//! Overlay suppression: `overlay_active` is an `AtomicBool` shared with the
//! output pump. The pump writes to stdout only when `!overlay_active`. Task 6
//! sets it `true` before drawing the picker and `false` after restoring the
//! child's screen. A stale pump from a PRIOR cross-server attach is a separate
//! concern handled when the cockpit re-attaches (Task 7), not here.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::crossterm;
use tokio::sync::mpsc;

use crate::proxy::{
    decode::KeyDecoder,
    input::{InAction, InputMachine},
    screen::Grid,
};
use crate::ui::run::{run_picker_fed, Cmd, ScanCache};
use crate::ui::switcher::{Ops, SwitchResult};

/// RAII guard: enables raw mode on construction and restores it on drop, so the
/// host terminal is left cooked on normal return AND on a panic (release builds
/// unwind). The proxy needs raw mode for the WHOLE session — byte-faithful
/// forwarding can't tolerate the line discipline cooking input — not just while
/// the overlay is up.
struct RawGuard;

impl RawGuard {
    fn enter() -> anyhow::Result<Self> {
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

/// Read `XMUX_PREFIX` from the environment and parse it.
pub fn prefix_from_env() -> u8 {
    parse_prefix(std::env::var("XMUX_PREFIX").ok().as_deref())
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

/// A command for the dedicated PTY control thread: bytes to write to the child,
/// or a resize to apply to the master. The async runtime is single-threaded, so
/// routing every blocking PTY write/resize through this channel keeps a slow
/// child (e.g. a stalled ssh remote that drains input slowly) from freezing
/// rendering, output streaming, and the picker hotkey on the event loop.
enum PtyCmd {
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
/// On channel close it returns; in `proxy_attach` the sink owns the master, so
/// returning drops it here. Closing the ConPTY can block on Windows older than
/// 24H2 (`ClosePseudoConsole` waits for clients to disconnect) — but only this
/// control thread can stall on that, never the async runtime or `proxy_attach`'s
/// return. In the common teardown path the output pump is still draining the read
/// pipe, which lets the close complete.
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

/// The proxy's internal mode. The picker overlay is a self-contained inner loop
/// (gated by `overlay_active`, not a mode), so there is no separate Picker state.
#[derive(PartialEq, Clone, Copy)]
enum Mode {
    /// Forwarding host input to the child and streaming child output to stdout.
    Forwarding,
    /// Teardown in progress.
    Quitting,
}

/// Run `argv` under a PTY proxy.
///
/// - Owns the PTY master; spawns `argv[0]` on the slave with mux nesting-guard
///   env vars removed.
/// - Pumps child output to stdout (suppressed while `overlay_active`) and tees
///   every byte into a `Grid` for repaint on overlay close.
/// - Forwards host stdin bytes through `InputMachine`; on `InAction::OpenPicker`
///   it suspends forwarding, draws the picker overlay (feeding it the proxy's own
///   decoded stdin so there is never a second stdin reader), and on close repaints
///   the live pane via `Grid::restore_bytes()`.
/// - On child exit, returns `None`. On a picker pick (cross- or same-server),
///   returns `Some(result)`; on a picker cancel, resumes forwarding.
pub async fn proxy_attach(
    argv: &[String],
    ops: Arc<dyn Ops>,
    cfg: ProxyConfig,
    cache: Option<ScanCache>,
) -> anyhow::Result<Option<SwitchResult>> {
    anyhow::ensure!(!argv.is_empty(), "proxy_attach: argv must not be empty");

    // Raw mode for the whole session (RAII-restored on return or panic). Enabled
    // before any I/O so host keystrokes reach us byte-faithfully.
    let _raw = RawGuard::enter()?;

    // ------------------------------------------------------------------
    // 1. Open a PTY pair and spawn the child on the slave.
    // ------------------------------------------------------------------
    let pty = native_pty_system();
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let pair = pty.openpty(PtySize {
        rows: size.1,
        cols: size.0,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    // Clear the mux nesting-guard vars so the attach child is never refused.
    cmd.env_remove("PSMUX_SESSION");
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave); // parent must not hold the slave open

    // ------------------------------------------------------------------
    // 2. Spin up the output pump on a dedicated thread.
    //    Proven pattern from tmp/pty-spike: bounded read loop on a thread,
    //    NOT read_to_end (deadlocks on ConPTY). The pump thread is never joined;
    //    the master lives on the control thread below and is dropped there.
    // ------------------------------------------------------------------
    let mut reader = pair.master.try_clone_reader()?;
    let pty_writer = pair.master.take_writer()?;

    // The dedicated PTY control thread: it owns the writer AND the master, so
    // every blocking input write and ConPTY resize runs here, never on the
    // single-threaded async runtime — a slow child draining input can no longer
    // freeze rendering, streaming, or the picker hotkey. The event loop only
    // `send`s commands, which never blocks. Same off-loop pattern as the output
    // pump and the stdin reader.
    //
    // The channel is intentionally unbounded: a bounded `send` on a full queue
    // would block the runtime thread again (the freeze this removes), so it is
    // left to grow. Against a wedged child it grows only at human typing speed and
    // every byte is delivered in order once the child drains. Resize shares this
    // one FIFO with input so the child sees size changes in causal order with the
    // keystroke stream.
    let (pty_tx, pty_rx) = std::sync::mpsc::channel::<PtyCmd>();
    std::thread::spawn(move || {
        pty_control_loop(
            pty_rx,
            MasterSink {
                writer: pty_writer,
                master: pair.master,
            },
        )
    });

    // overlay_active: when true the pump does not write to stdout (the picker
    // overlay owns the terminal). Task 6 sets true before drawing, false after
    // restoring via grid.restore_bytes().
    let overlay_active: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let pump_overlay = overlay_active.clone();

    // Grid lives on the main thread (single-writer); pump sends raw chunks via
    // an unbounded channel so chunks are never dropped (Grid is the source of
    // truth for the restore repaint — dropping corrupts it).
    let (pump_tx, mut pump_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    // A second copy of pump_tx is kept so the channel stays open.
    let pump_tx2 = pump_tx.clone();

    // Stdout handle for the pump to write into (overlay-gated).
    let stdout = std::io::stdout();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    // Tee to main thread for grid (non-blocking; unbounded so never drops).
                    let _ = pump_tx.send(chunk.clone());
                    // Write to real stdout only when the overlay is not active.
                    // Acquire the process-global stdout lock PER WRITE so it is
                    // never held across the blocking ConPTY read (which never
                    // EOFs after child exit) and never held while the overlay is
                    // up. The picker draws to `std::io::stdout()` from a different
                    // (tokio) task; holding a long-lived `StdoutLock` here would
                    // block that draw forever and deadlock the first `Ctrl-g s`.
                    if !pump_overlay.load(Ordering::Acquire) {
                        pump_write(&stdout, &chunk);
                    }
                }
                Err(_) => break,
            }
        }
        drop(pump_tx); // signal channel closed
    });
    // Keep pump_tx2 alive so the channel stays open until proxy_attach returns.
    let _keep_pump_tx = pump_tx2;

    // ------------------------------------------------------------------
    // 3. Main async loop — interleaves:
    //    (a) Grid chunks arriving from the pump
    //    (b) Crossterm terminal resize events
    //    (c) Raw stdin bytes forwarded through InputMachine
    //
    // The proxy's raw stdin thread is the SINGLE reader of host stdin. The
    // EventStream is used only for Resize (not key bytes). When the overlay
    // opens, the same `stdin_rx` is decoded and fed into the picker — the proxy
    // never spawns a second stdin reader, which would race this thread.
    // ------------------------------------------------------------------
    let mut grid = Grid::new(size.1, size.0);
    let mut machine = InputMachine::new(
        cfg.prefix,
        cfg.action_key,
        Duration::from_millis(400),
    );
    let mut mode = Mode::Forwarding;
    let mut pick_result: Option<SwitchResult> = None;

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        use std::io::Read as _;
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut event_stream = EventStream::new();

    loop {
        // Check if the child has exited (non-blocking). NOTE (I2): child exit is
        // NOT observed while the overlay is open — the inner overlay loop does not
        // poll try_wait. It is detected here on the next outer iteration once the
        // overlay closes (the overlay is a short-lived modal), so the delay is
        // bounded by how long the picker stays up.
        if let Ok(Some(_status)) = child.try_wait() {
            mode = Mode::Quitting;
        }
        if mode == Mode::Quitting {
            break;
        }

        tokio::select! {
            // Grid chunks from the pump.
            Some(chunk) = pump_rx.recv() => {
                grid.feed(&chunk);
            }

            // Raw stdin bytes → InputMachine → forward or open picker.
            Some(bytes) = stdin_rx.recv() => {
                if mode == Mode::Forwarding {
                    let now = Instant::now();
                    let mut to_forward: Vec<u8> = Vec::new();
                    let mut open_picker = false;
                    for byte in bytes {
                        let actions = machine.feed(byte, now);
                        for action in actions {
                            match action {
                                InAction::Forward(b) => to_forward.extend_from_slice(&b),
                                InAction::OpenPicker => {
                                    // Flush pending bytes first; the prefix+action
                                    // bytes themselves are NOT forwarded to the child.
                                    if !to_forward.is_empty() {
                                        let _ = pty_tx
                                            .send(PtyCmd::Input(std::mem::take(&mut to_forward)));
                                    }
                                    open_picker = true;
                                }
                            }
                        }
                    }
                    if !to_forward.is_empty() {
                        let _ = pty_tx.send(PtyCmd::Input(std::mem::take(&mut to_forward)));
                    }

                    // The hotkey fired: run the picker overlay. The proxy stays
                    // the single stdin reader — it decodes its own `stdin_rx` and
                    // FEEDS the picker over a Cmd channel; no second reader.
                    if open_picker {
                        overlay_active.store(true, Ordering::Release);

                        let (pcmd_tx, pcmd_rx) = mpsc::channel::<Cmd>(256);
                        let picker = tokio::spawn(run_picker_fed(
                            ops.clone(),
                            pcmd_tx.clone(),
                            pcmd_rx,
                            cache.clone(),
                        ));
                        tokio::pin!(picker);

                        let mut decoder = KeyDecoder::new();
                        // Inner loop: keep the proxy as the SOLE reader of stdin,
                        // routing decoded keys + resize into the picker, feeding
                        // the grid, until the picker task completes.
                        let picker_out = loop {
                            tokio::select! {
                                // Picker finished — take its result.
                                joined = &mut picker => {
                                    break joined;
                                }
                                // Keep the grid current under the overlay so the
                                // restore repaint reflects the child's live screen.
                                Some(chunk) = pump_rx.recv() => {
                                    grid.feed(&chunk);
                                }
                                // The proxy's own stdin → decode → feed the picker.
                                Some(raw) = stdin_rx.recv() => {
                                    for key in decoder.feed(&raw) {
                                        if pcmd_tx.send(Cmd::Key(key)).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                                // Resize: re-size the child PTY + grid AND tell the
                                // picker so its layout tracks the live terminal.
                                Some(Ok(event)) = event_stream.next() => {
                                    if let Event::Resize(cols, rows) = event {
                                        grid.resize(rows, cols);
                                        let _ = pty_tx.send(PtyCmd::Resize { cols, rows });
                                        let _ = pcmd_tx.send(Cmd::Resize(cols, rows)).await;
                                    }
                                }
                            }
                        };

                        // Overlay closed: repaint the live pane from the grid, then
                        // re-enable the output pump's stdout writes.
                        let restore = grid.restore_bytes();
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(&restore);
                        let _ = out.flush();
                        drop(out);
                        overlay_active.store(false, Ordering::Release);

                        // A pick (cross- or same-server) ends the proxy so the
                        // cockpit acts on the switch. A cancel, a picker error, or a
                        // join failure all leave `mode` at Forwarding so the live
                        // session resumes rather than being torn down.
                        if let Ok(Ok(result)) = picker_out {
                            if result.chosen.is_some() {
                                pick_result = Some(result);
                                mode = Mode::Quitting;
                            }
                        }
                    }
                }
            }

            // Crossterm events — only Resize is handled here (key bytes come
            // from the raw stdin thread above for byte-faithful proxying).
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Resize(cols, rows) = event {
                    grid.resize(rows, cols);
                    let _ = pty_tx.send(PtyCmd::Resize { cols, rows });
                }
            }

            else => break,
        }

        if mode == Mode::Quitting {
            break;
        }
    }

    // ------------------------------------------------------------------
    // 4. Teardown: kill the child.  Do NOT join the pump thread — it blocks on
    //    an outstanding ConPTY read that never returns after the child exits.
    // ------------------------------------------------------------------
    let _ = child.kill();
    // Drop the control-thread sender so that thread's recv ends and it returns,
    // dropping the master there. The ConPTY close can block that control thread on
    // pre-24H2 Windows, but never this runtime thread or proxy_attach's return —
    // the cockpit re-attaches regardless.
    drop(pty_tx);

    Ok(pick_result)
}

// ---------------------------------------------------------------------------
// Headless smoke-test helper (used by the #[ignore] test below).
// ---------------------------------------------------------------------------

/// Spawns cmd.exe on a PTY slave, writes a command, waits for the marker to
/// round-trip through the master. Mirrors tmp/pty-spike/src/proxy.rs exactly.
#[cfg(test)]
pub fn smoke_roundtrip() -> bool {
    use std::sync::mpsc::{self, RecvTimeoutError};

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

    #[test]
    fn prefix_from_env_parses_and_defaults() {
        assert_eq!(parse_prefix(Some("C-g")), 0x07);
        assert_eq!(parse_prefix(Some("C-Space")), 0x00);
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

    // -- decode → picker wiring -------------------------------------------
    //
    // The full overlay (raw-mode stdin thread + live console) is a manual gate.
    // What IS headless-testable is the exact decode→feed path the overlay uses:
    // raw host bytes → `KeyDecoder` → `Cmd::Key` → the switcher's selection. This
    // proves a stream of host keystrokes (filter then Enter) drives the picker to
    // the intended `SwitchResult` deterministically.

    use crate::session::Session;
    use crate::ui::run::event_loop;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    struct NoopOps;

    #[async_trait::async_trait]
    impl Ops for NoopOps {
        fn sources(&self) -> Vec<String> {
            // No sources ⇒ the loop kicks no probes, leaving a `Switcher::new`
            // snapshot untouched (this test doesn't exercise streaming).
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
        async fn panes(&self, _s: &Session) -> anyhow::Result<Vec<crate::session::WindowPanes>> {
            Ok(Vec::new())
        }
        async fn capture(&self, _source: &str, _target: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    /// Two local sessions: `editor` (attached, most-recent) and `build`.
    fn sample_scan() -> Scan {
        Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![
                    Session {
                        source: "local".into(),
                        name: "editor".into(),
                        windows: 1,
                        attached: true,
                        last_attached: 999,
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
        }
    }

    #[tokio::test]
    async fn decoded_filter_then_enter_picks_visible_session() {
        // Host types `/build` then Enter (apply filter) then Enter (attach). The
        // SAME bytes the overlay would receive — decoded by `KeyDecoder` and sent
        // as `Cmd::Key` — must drive the picker to the visible (filtered) session,
        // not the attached/most-recent one that got filtered out.
        let raw = b"/build\r\r";
        let mut decoder = KeyDecoder::new();
        let keys = decoder.feed(raw);

        let (tx, rx) = mpsc::channel::<Cmd>(32);
        for k in keys {
            tx.send(Cmd::Key(k)).await.unwrap();
        }

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample_scan());
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();

        assert_eq!(
            sw.result().chosen.as_ref().map(|s| s.name.as_str()),
            Some("build"),
            "decoded filter+Enter must pick the visible (filtered) session"
        );
    }

    #[tokio::test]
    async fn decoded_esc_cancels_with_no_choice() {
        // A bare ESC byte from the host decodes to Esc, which cancels the picker —
        // leaving no chosen session so the proxy resumes forwarding untouched.
        let mut decoder = KeyDecoder::new();
        let keys = decoder.feed(b"\x1b");

        let (tx, rx) = mpsc::channel::<Cmd>(8);
        for k in keys {
            tx.send(Cmd::Key(k)).await.unwrap();
        }

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample_scan());
        event_loop(&mut term, &mut sw, Arc::new(NoopOps), tx.clone(), rx)
            .await
            .unwrap();

        assert!(
            sw.result().chosen.is_none(),
            "a decoded Esc must cancel the picker with no chosen session"
        );
    }
}
