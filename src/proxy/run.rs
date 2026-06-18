//! PTY proxy loop: own a ConPTY, spawn the attach child on its slave (with the
//! mux-nesting env cleared), pump child output (tee → Grid, generation-gated
//! stdout write), forward host input through the InputMachine, handle terminal
//! resize, and tear down without blocking.
//!
//! Generation tokens: each time the picker overlay takes over stdout, the
//! generation counter is bumped. The output pump captures the generation at
//! thread-spawn time and only writes to stdout when the live counter still
//! matches — so overlay writes never interleave with proxy output.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::proxy::{
    input::{InAction, InputMachine},
    screen::Grid,
};
use crate::ui::{
    run::{run_picker_fed, Cmd},
    switcher::{Ops, SwitchResult},
};

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

/// The proxy's internal mode.
#[derive(PartialEq, Clone, Copy)]
#[allow(dead_code)] // Picker used by Task-6 overlay path (not yet wired)
enum Mode {
    /// Forwarding host input to the child and streaming child output to stdout.
    Forwarding,
    /// The picker overlay owns the terminal; the output pump is generation-gated.
    Picker,
    /// Teardown in progress.
    Quitting,
}

/// Run `argv` under a PTY proxy.
///
/// - Owns the PTY master; spawns `argv[0]` on the slave with mux nesting-guard
///   env vars removed.
/// - Pumps child output to stdout (generation-gated while the picker overlay is
///   active) and tees every byte into a `Grid` for repaint on overlay close.
/// - Forwards host stdin bytes through `InputMachine`; on `InAction::OpenPicker`
///   opens the picker overlay (Task 6 TODO hook).
/// - On child exit, returns `None`. On picker pick/cancel, returns
///   `Some(result)`.
pub async fn proxy_attach(
    argv: &[String],
    ops: Arc<dyn Ops>,
    cfg: ProxyConfig,
) -> anyhow::Result<Option<SwitchResult>> {
    anyhow::ensure!(!argv.is_empty(), "proxy_attach: argv must not be empty");

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
    //    NOT read_to_end (deadlocks on ConPTY). The thread is never joined
    //    and the master is never dropped — both block on ConPTY.
    // ------------------------------------------------------------------
    let mut reader = pair.master.try_clone_reader()?;
    let mut pty_writer = pair.master.take_writer()?;

    // generation counter: bump when the picker overlay takes over stdout;
    // the pump only writes when its captured generation still matches.
    let generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let pump_gen = generation.clone();

    // Grid lives on the main thread (single-writer); pump sends raw chunks via a
    // channel so the main thread can feed them in.
    let (pump_tx, mut pump_rx) = mpsc::channel::<Vec<u8>>(256);
    // A second copy of pump_tx is kept for the grid so the channel stays open.
    let pump_tx2 = pump_tx.clone();

    // Stdout handle for the pump to write into (generation-gated).
    let stdout = std::io::stdout();
    std::thread::spawn(move || {
        // Keep a local lock handle so drops don't release stdout between writes.
        let mut out = stdout.lock();
        let mut buf = [0u8; 4096];
        let my_gen = pump_gen.load(Ordering::Acquire);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    // Tee to main thread for grid (best-effort; never block the pump).
                    let _ = pump_tx.try_send(chunk.clone());
                    // Write to real stdout only when in forwarding mode (generation
                    // matches the value at thread-spawn time).
                    if pump_gen.load(Ordering::Acquire) == my_gen {
                        let _ = out.write_all(&chunk);
                        let _ = out.flush();
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
    // ------------------------------------------------------------------
    let mut grid = Grid::new(size.1, size.0);
    let mut machine = InputMachine::new(
        cfg.prefix,
        cfg.action_key,
        Duration::from_millis(400),
    );
    let mut mode = Mode::Forwarding;
    let mut pick_result: Option<SwitchResult> = None;

    // Crossterm event stream for resize / raw key events.
    // We read stdin bytes directly for forwarding (crossterm input is
    // insufficient for proxying because it decodes and loses raw bytes).
    // However, we need crossterm for Resize events; raw bytes come from a
    // background stdin-reader thread that sends them over a channel.
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
        // Check if the child has exited (non-blocking).
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
                    for byte in bytes {
                        let actions = machine.feed(byte, now);
                        for action in actions {
                            match action {
                                InAction::Forward(b) => to_forward.extend_from_slice(&b),
                                InAction::OpenPicker => {
                                    // Flush pending bytes first.
                                    if !to_forward.is_empty() {
                                        let _ = pty_writer.write_all(&to_forward);
                                        let _ = pty_writer.flush();
                                        to_forward.clear();
                                    }
                                    // --- TODO (Task 6): open the picker overlay ---
                                    // Generation bump suppresses the output pump's
                                    // stdout writes while the overlay owns the terminal.
                                    // Restore with grid.restore_bytes() on close.

                                    // Bump generation so the pump stops writing.
                                    let new_gen = generation.fetch_add(1, Ordering::AcqRel) + 1;

                                    // Run the picker overlay using the pre-built
                                    // run_picker_fed path (caller owns the terminal).
                                    let result = run_overlay(ops.clone(), new_gen, &generation, &mut grid).await;

                                    mode = Mode::Forwarding;
                                    match result {
                                        OverlayOutcome::Picked(r) => {
                                            pick_result = Some(r);
                                            mode = Mode::Quitting;
                                        }
                                        OverlayOutcome::Cancelled => {
                                            // Restore child's screen from the grid.
                                            let restore = grid.restore_bytes();
                                            let _ = std::io::stdout().lock().write_all(&restore);
                                            let _ = std::io::stdout().lock().flush();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !to_forward.is_empty() {
                        let _ = pty_writer.write_all(&to_forward);
                        let _ = pty_writer.flush();
                    }
                    // Tick the machine to flush an armed-but-timed-out prefix.
                    let timed_out = machine.tick(Instant::now());
                    let mut flush_bytes: Vec<u8> = Vec::new();
                    for action in timed_out {
                        if let InAction::Forward(b) = action {
                            flush_bytes.extend_from_slice(&b);
                        }
                    }
                    if !flush_bytes.is_empty() {
                        let _ = pty_writer.write_all(&flush_bytes);
                        let _ = pty_writer.flush();
                    }
                }
            }

            // Crossterm events — only Resize is handled here (key bytes come
            // from the raw stdin thread above for byte-faithful proxying).
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Resize(cols, rows) = event {
                    grid.resize(rows, cols);
                    let _ = pair.master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }

            else => break,
        }

        if mode == Mode::Quitting {
            break;
        }
    }

    // ------------------------------------------------------------------
    // 4. Teardown: kill the child.  Do NOT join the pump thread and do NOT
    //    drop the master — both block on an outstanding ConPTY read that
    //    never returns after the child exits.
    // ------------------------------------------------------------------
    let _ = child.kill();
    // Keep master alive; process exit (or the caller's drop) reaps the pump.
    let _keep_master = pair.master;

    Ok(pick_result)
}

// ---------------------------------------------------------------------------
// Overlay driver — invoked from the input loop when InAction::OpenPicker fires.
// ---------------------------------------------------------------------------

enum OverlayOutcome {
    Picked(SwitchResult),
    Cancelled,
}

async fn run_overlay(
    ops: Arc<dyn Ops>,
    _new_gen: u64,
    _generation: &Arc<AtomicU64>,
    _grid: &mut Grid,
) -> OverlayOutcome {
    // Build a crossterm terminal that draws to the real stdout (the caller has
    // already suppressed the pump's stdout writes via the generation bump).
    let stdout = std::io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut term = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => return OverlayOutcome::Cancelled,
    };

    // Synthesise a channel pair; run_picker_fed reads from cmd_rx.
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(128);

    // Read crossterm events and feed them as Cmd::Key / Cmd::Resize so the
    // picker's event loop can drive without its own read_events task (which
    // would conflict with the proxy's own event stream).
    let feeder_tx = cmd_tx.clone();
    let feeder = tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(ev)) = stream.next().await {
            let cmd = match ev {
                Event::Key(k)
                    if k.kind
                        == crossterm::event::KeyEventKind::Press =>
                {
                    Cmd::Key(k)
                }
                Event::Resize(cols, rows) => Cmd::Resize(cols, rows),
                _ => continue,
            };
            if feeder_tx.send(cmd).await.is_err() {
                break;
            }
        }
    });

    let result = run_picker_fed(ops, cmd_tx, cmd_rx, &mut term).await;
    feeder.abort();

    match result {
        Ok(r) if r.chosen.is_some() => OverlayOutcome::Picked(r),
        _ => OverlayOutcome::Cancelled,
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

    // Smoke test: requires a console; run on demand with
    //   cargo test -p xmux proxy::run::tests::smoke -- --ignored --nocapture
    #[ignore]
    #[test]
    fn smoke_child_runs_under_proxy() {
        // mirrors tmp/pty-spike: spawn cmd.exe on a slave, write a command,
        // read the marker back. Asserts bytes round-trip through the proxy PTY.
        assert!(super::smoke_roundtrip());
    }
}
