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
use futures::StreamExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::crossterm;
use tokio::sync::mpsc;

use crate::proxy::{
    input::{InAction, InputMachine},
    screen::Grid,
};
use crate::ui::switcher::{Ops, SwitchResult};

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
    /// The picker overlay owns the terminal; the output pump is suppressed via overlay_active.
    Picker,
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
///   the Task-6 picker overlay will be driven (stub for now).
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
        // Keep a local lock handle so drops don't release stdout between writes.
        let mut out = stdout.lock();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    // Tee to main thread for grid (non-blocking; unbounded so never drops).
                    let _ = pump_tx.send(chunk.clone());
                    // Write to real stdout only when the overlay is not active.
                    if !pump_overlay.load(Ordering::Acquire) {
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
    //
    // NOTE (S-input live-terminal gate): the proxy runs a raw stdin thread
    // (below) for byte-faithful forwarding AND a crossterm EventStream for
    // resize events. On Windows these two can compete for console input.
    // The correct single-reader design (one reader yielding both raw bytes
    // and resize via raw console input) is deferred to Task 6. Do NOT
    // rearchitect the input model here.
    // ------------------------------------------------------------------
    let mut grid = Grid::new(size.1, size.0);
    let mut machine = InputMachine::new(
        cfg.prefix,
        cfg.action_key,
        Duration::from_millis(400),
    );
    let mut mode = Mode::Forwarding;
    let pick_result: Option<SwitchResult> = None;

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
                                    // TODO(Task 6): drive the picker overlay here.
                                    // Steps:
                                    //   overlay_active.store(true, Ordering::Release);
                                    //   mode = Mode::Picker;
                                    //   run the picker fed by KeyDecoder over a Cmd channel;
                                    //   let restore = grid.restore_bytes();
                                    //   stdout.lock().write_all(&restore)?;
                                    //   overlay_active.store(false, Ordering::Release);
                                    //   mode = Mode::Forwarding; (or Quitting on pick)
                                    //
                                    // For now: swallow the hotkey, stay in Forwarding.
                                    // The prefix+action bytes are NOT forwarded to the child.
                                    let _ = &ops; // suppress unused-variable lint until Task 6
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
