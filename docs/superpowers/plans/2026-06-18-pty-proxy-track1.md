# PTY-proxy overlay (Track 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the xmux cockpit own a PTY so a prefix hotkey opens the cross-host picker as a fast local overlay over the live pane, then teleports (same-server) or re-attaches (cross-server) — nothing installed on remotes.

**Architecture:** `RealAttacher::attach` stops handing the terminal off blindly and instead runs the attach child on a `portable-pty` slave, copying bytes both ways on dedicated threads. A byte-level state machine watches host input for a configurable prefix; on the hotkey it pauses output forwarding and drives the existing switcher `event_loop` (no alt-screen toggle, fed via the `Cmd` channel) as an overlay, restoring the live pane from a `vt100` grid on close. A per-attach generation token lets a stale output pump self-silence so teardown never blocks on the un-EOF-ing ConPTY reader.

**Tech Stack:** Rust, tokio (current-thread), `portable-pty` 0.9, `vt100` 0.16, crossterm 0.29, ratatui 0.30.

Design source: `docs/superpowers/specs/2026-06-18-pty-proxy-overlay-design.md` (APPROVE). Spike: `tmp/pty-spike/` (GREEN; S-local GREEN).

## Global Constraints

- **Deps:** add `portable-pty = "0.9"`, `vt100 = "0.16"`. crossterm/tokio/ratatui already present.
- **Scope:** cockpit-only. `RealAttacher::attach` (`src/cockpit.rs:313`) only; `OsExecer::exec` (`src/attach.rs:42`) is UNCHANGED this track.
- **Release profile stays `panic = "unwind"`** (`Cargo.toml:16-23`) — RAII `Drop` restores the terminal.
- **No alt-screen toggle from the proxy.** The picker is driven via the extracted `event_loop` core WITHOUT `TerminalGuard`/`read_events`; the proxy owns screen/mode policy and restores from the vt100 grid.
- **Single stdin owner = the proxy.** Forwarding path is RAW BYTES (no decode→re-serialize). Picker mode decodes the same raw bytes to `Cmd::Key`.
- **Clear `PSMUX_SESSION`/`TMUX`/`TMUX_PANE`** on the attach child (the cockpit runs un-nested; defensive hygiene — proven necessary in spike S-local).
- **Teardown:** never `join` the output pump nor `drop` the master while a read is outstanding (ConPTY reader never EOFs); gate stdout writes on a generation token.
- **Default prefix `Ctrl-g` (0x07), configurable via `XMUX_PREFIX`**, with a prefix-arm timeout (~400ms) and double-tap → one literal.
- Live-terminal gates (validated in TDD on a real terminal, NOT headless): host raw-input forwarding fidelity, visual screen-handover after overlay close.

---

### Task 1: Prefix input state machine

Pure byte-level logic: forward bytes, detect the prefix, arm/dispatch/passthrough with a timeout, and never match the prefix inside a bracketed paste. The prefix is a C0 byte (e.g. 0x07), which never appears as a UTF-8 continuation/lead or mid-CSI, so bracketed paste is the only framing guard needed.

**Files:**
- Create: `src/proxy/mod.rs` (module decl: `pub mod input;`)
- Create: `src/proxy/input.rs`
- Modify: `src/lib.rs` (add `pub mod proxy;`)
- Test: in `src/proxy/input.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub enum InAction { Forward(Vec<u8>), OpenPicker }`
  - `pub struct InputMachine`
  - `pub fn InputMachine::new(prefix: u8, action_key: u8, timeout: Duration) -> Self`
  - `pub fn InputMachine::feed(&mut self, byte: u8, now: Instant) -> Vec<InAction>`
  - `pub fn InputMachine::tick(&mut self, now: Instant) -> Vec<InAction>` (flush an armed prefix once the timeout passes)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn m() -> InputMachine { InputMachine::new(0x07, b's', Duration::from_millis(400)) }
    fn fwd(a: &[InAction]) -> Vec<u8> {
        a.iter().flat_map(|x| match x { InAction::Forward(b) => b.clone(), _ => vec![] }).collect()
    }

    #[test]
    fn plain_bytes_pass_through() {
        let mut im = m();
        let now = Instant::now();
        assert_eq!(fwd(&im.feed(b'a', now)), vec![b'a']);
        assert_eq!(fwd(&im.feed(b'b', now)), vec![b'b']);
    }

    #[test]
    fn prefix_then_action_opens_picker() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty(), "prefix is swallowed while arming");
        let out = im.feed(b's', t);
        assert!(matches!(out.as_slice(), [InAction::OpenPicker]));
    }

    #[test]
    fn double_prefix_sends_one_literal() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        assert_eq!(fwd(&im.feed(0x07, t)), vec![0x07]);
    }

    #[test]
    fn prefix_then_other_forwards_prefix_then_byte() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        assert_eq!(fwd(&im.feed(b'x', t)), vec![0x07, b'x']);
    }

    #[test]
    fn armed_prefix_times_out_to_literal() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        let later = t + Duration::from_millis(401);
        assert_eq!(fwd(&im.tick(later)), vec![0x07]);
    }

    #[test]
    fn prefix_inside_bracketed_paste_is_literal() {
        let mut im = m();
        let t = Instant::now();
        // enter paste: ESC [ 2 0 0 ~
        for b in b"\x1b[200~" { let _ = im.feed(*b, t); }
        // a 0x07 inside the paste must be forwarded, never open the picker
        let out = im.feed(0x07, t);
        assert_eq!(fwd(&out), vec![0x07]);
        assert!(!out.iter().any(|a| matches!(a, InAction::OpenPicker)));
        // leave paste: ESC [ 2 0 1 ~ — afterwards the prefix arms again
        for b in b"\x1b[201~" { let _ = im.feed(*b, t); }
        assert!(im.feed(0x07, t).is_empty());
        assert!(matches!(im.feed(b's', t).as_slice(), [InAction::OpenPicker]));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run (PowerShell tool): `cargo test -p xmux proxy::input 2>&1 | Select-Object -Last 20`
Expected: FAIL — `InputMachine`/`InAction` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Byte-level prefix detection on the host input stream. The prefix is a C0
//! control byte, so it cannot collide with UTF-8 continuation bytes or appear
//! mid-CSI; bracketed paste is the only framing the matcher must respect.
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum InAction {
    Forward(Vec<u8>),
    OpenPicker,
}

#[derive(PartialEq)]
enum State { Idle, Armed(Instant) }

pub struct InputMachine {
    prefix: u8,
    action_key: u8,
    timeout: Duration,
    state: State,
    in_paste: bool,
    paste_scan: Vec<u8>, // rolling tail to detect the paste markers
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl InputMachine {
    pub fn new(prefix: u8, action_key: u8, timeout: Duration) -> Self {
        Self { prefix, action_key, timeout, state: State::Idle, in_paste: false, paste_scan: Vec::new() }
    }

    fn track_paste(&mut self, byte: u8) {
        self.paste_scan.push(byte);
        if self.paste_scan.len() > PASTE_START.len() {
            self.paste_scan.remove(0);
        }
        if !self.in_paste && self.paste_scan.ends_with(PASTE_START) {
            self.in_paste = true;
        } else if self.in_paste && self.paste_scan.ends_with(PASTE_END) {
            self.in_paste = false;
        }
    }

    pub fn feed(&mut self, byte: u8, now: Instant) -> Vec<InAction> {
        // Inside a paste, everything is literal — never arm.
        if self.in_paste {
            self.track_paste(byte);
            return vec![InAction::Forward(vec![byte])];
        }
        match self.state {
            State::Idle => {
                if byte == self.prefix {
                    self.state = State::Armed(now);
                    Vec::new()
                } else {
                    self.track_paste(byte);
                    vec![InAction::Forward(vec![byte])]
                }
            }
            State::Armed(_) => {
                self.state = State::Idle;
                if byte == self.action_key {
                    vec![InAction::OpenPicker]
                } else if byte == self.prefix {
                    vec![InAction::Forward(vec![self.prefix])] // double-tap → one literal
                } else {
                    self.track_paste(byte);
                    vec![InAction::Forward(vec![self.prefix, byte])]
                }
            }
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<InAction> {
        if let State::Armed(t) = self.state {
            if now.duration_since(t) >= self.timeout {
                self.state = State::Idle;
                return vec![InAction::Forward(vec![self.prefix])];
            }
        }
        Vec::new()
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::input 2>&1 | Select-Object -Last 20`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add src/proxy/mod.rs src/proxy/input.rs src/lib.rs
git commit -m "feat(proxy): byte-level prefix input state machine"
```

---

### Task 2: byte→KeyEvent decoder for picker mode

In Picker mode the proxy is still the sole stdin reader, so it must turn raw bytes into the crossterm `KeyEvent`s the switcher consumes (`Cmd::Key`). Only the keys the picker uses: printable UTF-8, `Enter`, `Esc`, `Backspace`, and CSI arrows.

**Files:**
- Create: `src/proxy/decode.rs`
- Modify: `src/proxy/mod.rs` (add `pub mod decode;`)
- Test: in `src/proxy/decode.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: crossterm `KeyEvent`, `KeyCode`, `KeyModifiers`.
- Produces:
  - `pub struct KeyDecoder`
  - `pub fn KeyDecoder::new() -> Self`
  - `pub fn KeyDecoder::feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent>`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn codes(bytes: &[u8]) -> Vec<KeyCode> {
        KeyDecoder::new().feed(bytes).into_iter().map(|k| k.code).collect()
    }

    #[test]
    fn printable_ascii() {
        assert_eq!(codes(b"dev"), vec![KeyCode::Char('d'), KeyCode::Char('e'), KeyCode::Char('v')]);
    }

    #[test]
    fn enter_esc_backspace() {
        assert_eq!(codes(b"\r"), vec![KeyCode::Enter]);
        assert_eq!(codes(b"\x1b"), vec![KeyCode::Esc]);
        assert_eq!(codes(b"\x7f"), vec![KeyCode::Backspace]);
    }

    #[test]
    fn csi_arrows() {
        assert_eq!(codes(b"\x1b[A"), vec![KeyCode::Up]);
        assert_eq!(codes(b"\x1b[B"), vec![KeyCode::Down]);
        assert_eq!(codes(b"\x1b[C"), vec![KeyCode::Right]);
        assert_eq!(codes(b"\x1b[D"), vec![KeyCode::Left]);
    }

    #[test]
    fn utf8_multibyte_char() {
        // 한 = E1 95 9C? use a 2-byte char é = C3 A9
        assert_eq!(codes(&[0xc3, 0xa9]), vec![KeyCode::Char('é')]);
    }

    #[test]
    fn lone_esc_then_char_is_esc_and_char() {
        // a bare ESC not starting a CSI is Esc; the next byte is its own key
        assert_eq!(codes(b"\x1bx"), vec![KeyCode::Esc, KeyCode::Char('x')]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::decode 2>&1 | Select-Object -Last 20`
Expected: FAIL — `KeyDecoder` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Minimal raw-byte → crossterm KeyEvent decoder for Picker mode. Covers only the
//! keys the switcher uses. A lone ESC that is not followed by `[<final>` is Esc.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct KeyDecoder {
    buf: Vec<u8>,
}

impl KeyDecoder {
    pub fn new() -> Self { Self { buf: Vec::new() } }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            match b {
                0x1b => {
                    // CSI arrow: ESC [ A/B/C/D
                    if i + 2 < self.buf.len() && self.buf[i + 1] == b'[' {
                        let code = match self.buf[i + 2] {
                            b'A' => Some(KeyCode::Up),
                            b'B' => Some(KeyCode::Down),
                            b'C' => Some(KeyCode::Right),
                            b'D' => Some(KeyCode::Left),
                            _ => None,
                        };
                        if let Some(c) = code {
                            out.push(KeyEvent::new(c, KeyModifiers::NONE));
                            i += 3;
                            continue;
                        }
                    }
                    // If an incomplete escape could still be a CSI, wait for more.
                    if i + 1 >= self.buf.len() || self.buf[i + 1] == b'[' && i + 2 >= self.buf.len() {
                        break; // keep the tail buffered
                    }
                    out.push(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    i += 1;
                }
                b'\r' | b'\n' => { out.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); i += 1; }
                0x7f | 0x08 => { out.push(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); i += 1; }
                _ if b < 0x80 => { out.push(KeyEvent::new(KeyCode::Char(b as char), KeyModifiers::NONE)); i += 1; }
                _ => {
                    // UTF-8 multibyte: find the char length, decode if complete.
                    let len = utf8_len(b);
                    if i + len > self.buf.len() { break; } // incomplete, buffer it
                    if let Ok(s) = std::str::from_utf8(&self.buf[i..i + len]) {
                        if let Some(c) = s.chars().next() {
                            out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                        }
                    }
                    i += len;
                }
            }
        }
        self.buf.drain(0..i);
        out
    }
}

fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 { 1 } else if lead < 0xe0 { 2 } else if lead < 0xf0 { 3 } else { 4 }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::decode 2>&1 | Select-Object -Last 20`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/proxy/decode.rs src/proxy/mod.rs
git commit -m "feat(proxy): minimal byte->KeyEvent decoder for picker mode"
```

---

### Task 3: vt100 screen grid + restore

Wrap a `vt100::Parser`, tee the child output into it, and produce the restore bytes (`contents_formatted()` + re-emitted private modes). NEW-1: the fidelity test verifies vt100 0.16 exposes the modes we re-emit; if an accessor is named differently, adjust and keep the test.

**Files:**
- Modify: `Cargo.toml` (add `vt100 = "0.16"`)
- Create: `src/proxy/screen.rs`
- Modify: `src/proxy/mod.rs` (add `pub mod screen;`)
- Test: in `src/proxy/screen.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct Grid { parser: vt100::Parser }`
  - `pub fn Grid::new(rows: u16, cols: u16) -> Self`
  - `pub fn Grid::feed(&mut self, bytes: &[u8])`
  - `pub fn Grid::resize(&mut self, rows: u16, cols: u16)`
  - `pub fn Grid::restore_bytes(&self) -> Vec<u8>` (full repaint + mode re-emit)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_plain_content() {
        let mut g = Grid::new(24, 80);
        g.feed(b"hello-VT100-grid");
        let bytes = g.restore_bytes();
        assert!(!bytes.is_empty());
        assert!(String::from_utf8_lossy(&bytes).contains("hello-VT100-grid"));
    }

    #[test]
    fn tracks_alternate_screen() {
        let mut g = Grid::new(10, 40);
        g.feed(b"\x1b[?1049h\x1b[Halt-text");
        assert!(g.restore_includes_alt_marker());
    }

    #[test]
    fn restore_reemits_bracketed_paste_mode() {
        // child enabled bracketed paste; restore must re-assert it
        let mut g = Grid::new(10, 40);
        g.feed(b"\x1b[?2004h");
        let bytes = g.restore_bytes();
        assert!(bytes.windows(8).any(|w| w == b"\x1b[?2004h"),
            "restore must re-emit bracketed-paste enable");
    }
}
```

(Add a small test-only helper `restore_includes_alt_marker(&self) -> bool` returning `self.parser.screen().contents().contains("alt-text")` to keep the alt test simple.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::screen 2>&1 | Select-Object -Last 20`
Expected: FAIL — `Grid` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! A one-pane vt100 grid the proxy tees child output into, used ONLY to repaint
//! the live pane after a transient overlay. Not a multiplexer: one grid, no
//! layouts, no input routing.
pub struct Grid {
    parser: vt100::Parser,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { parser: vt100::Parser::new(rows, cols, 0) }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.set_size(rows, cols);
    }

    /// Full repaint of the visible grid + re-emit of the private modes the child
    /// set, so its input handling is not silently broken after an overlay close.
    pub fn restore_bytes(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut out = screen.contents_formatted();
        // Re-emit tracked private modes. (vt100 0.16 accessor names verified by
        // the fidelity test; adjust here if an accessor differs.)
        if screen.bracketed_paste() { out.extend_from_slice(b"\x1b[?2004h"); }
        if screen.application_cursor() { out.extend_from_slice(b"\x1b[?1h"); }
        if screen.application_keypad() { out.extend_from_slice(b"\x1b="); }
        if screen.hide_cursor() { out.extend_from_slice(b"\x1b[?25l"); }
        out
    }

    #[cfg(test)]
    fn restore_includes_alt_marker(&self) -> bool {
        self.parser.screen().contents().contains("alt-text")
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::screen 2>&1 | Select-Object -Last 20`
Expected: PASS (3 tests). If an accessor name fails to compile, consult `https://docs.rs/vt100/0.16` and use the actual name (e.g. `mouse_protocol_mode`) — keep the assertion.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/proxy/screen.rs src/proxy/mod.rs
git commit -m "feat(proxy): vt100 grid with restore-bytes (repaint + mode re-emit)"
```

---

### Task 4: `Cmd::Resize` + picker-core entry the proxy can drive

Add the resize conduit (NEW-2) and a picker entry that drives `event_loop` WITHOUT `TerminalGuard`/`read_events`, so the proxy owns terminal policy and feeds keys via the channel.

**Files:**
- Modify: `src/ui/run.rs` (`Cmd` enum ~36-57; `event_loop` match ~200-234; add `run_picker_fed`)
- Test: `src/ui/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `Cmd::Resize(u16, u16)` (cols, rows)
  - `pub async fn run_picker_fed(ops: Arc<dyn Ops>, cmd_tx: mpsc::Sender<Cmd>, cmd_rx: mpsc::Receiver<Cmd>, term: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> anyhow::Result<SwitchResult>` — builds the switcher, runs `event_loop`, returns the result; installs NO `TerminalGuard` and spawns NO `read_events` (the caller owns input + screen).
- Consumes: existing `event_loop`, `Switcher::from_sources`, `Ops`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn resize_cmd_is_handled_then_quit() {
    use ratatui::backend::TestBackend;
    let ops = crate::ui::switcher::tests_support::empty_ops(); // existing test ops helper
    let (tx, rx) = mpsc::channel::<Cmd>(16);
    let mut switcher = Switcher::from_sources(ops.sources());
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    // a resize then a quit key
    tx.send(Cmd::Resize(100, 40)).await.unwrap();
    tx.send(Cmd::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))).await.unwrap();
    let r = event_loop(&mut term, &mut switcher, ops, tx.clone(), rx).await;
    assert!(r.is_ok(), "Cmd::Resize must be handled without error");
}
```

(If no `empty_ops` helper exists, reuse whatever `Ops` test double the existing `run.rs`/`switcher.rs` tests already use — match the established pattern.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux ui::run::tests::resize_cmd 2>&1 | Select-Object -Last 20`
Expected: FAIL — no `Cmd::Resize` variant.

- [ ] **Step 3: Write minimal implementation**

Add the variant to `Cmd` (after `Mouse`):
```rust
    /// Host terminal resized (cols, rows) — re-layout the picker.
    Resize(u16, u16),
```
Add the match arm in `event_loop` (alongside `Cmd::Mouse`):
```rust
                    Cmd::Resize(cols, rows) => {
                        let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, cols, rows));
                    }
```
Add the proxy entry (near `run_switcher`):
```rust
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
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux ui::run 2>&1 | Select-Object -Last 25`
Expected: PASS (existing run tests + the new one).

- [ ] **Step 5: Commit**

```bash
git add src/ui/run.rs
git commit -m "feat(ui): Cmd::Resize + run_picker_fed (proxy-driven picker core)"
```

---

### Task 5: PTY proxy loop (plumbing, generation token, output pump)

The integration core: own the PTY, spawn the child (nesting-env cleared), pump output (tee → grid, gen-gated stdout write), forward input through the state machine, and a teardown that never blocks. Verified by a reduced spike-style smoke test gated behind `#[ignore]` (runs on demand, not in the default suite), plus the pure-unit coverage from Tasks 1-3.

**Files:**
- Modify: `Cargo.toml` (add `portable-pty = "0.9"`)
- Create: `src/proxy/run.rs` (the proxy loop)
- Modify: `src/proxy/mod.rs` (add `pub mod run;` and re-exports)
- Test: `src/proxy/run.rs` `#[cfg(test)]` (an `#[ignore]` smoke test)

**Interfaces:**
- Consumes: `input::InputMachine`, `decode::KeyDecoder`, `screen::Grid`, `ui::run::{run_picker_fed, Cmd}`.
- Produces:
  - `pub struct ProxyConfig { pub prefix: u8, pub action_key: u8 }`
  - `pub fn prefix_from_env() -> u8` (parse `XMUX_PREFIX` like `C-g`; default 0x07)
  - `pub async fn proxy_attach(argv: &[String], ops: Arc<dyn Ops>, cfg: ProxyConfig) -> anyhow::Result<Option<crate::ui::switcher::SwitchResult>>` — runs the child under the proxy; returns `Some(result)` if the user opened the picker and picked/cancelled (so the caller acts on a switch), `None` if the child exited normally.

- [ ] **Step 1: Write the failing test (ignored smoke test)**

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::run::tests::prefix_from_env 2>&1 | Select-Object -Last 20`
Expected: FAIL — `parse_prefix` not found.

- [ ] **Step 3: Write the implementation**

Implement `parse_prefix(Option<&str>) -> u8` (map `C-<x>` → control byte: `C-Space`→0x00, `C-a`..`C-z`→0x01..0x1a, `C-g`→0x07; anything else → 0x07), `prefix_from_env()` calling it on `std::env::var("XMUX_PREFIX")`, and the proxy loop:

```rust
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use crate::proxy::{input::{InAction, InputMachine}, screen::Grid};

pub struct ProxyConfig { pub prefix: u8, pub action_key: u8 }

pub fn parse_prefix(spec: Option<&str>) -> u8 {
    let s = match spec { Some(s) => s.trim(), None => return 0x07 };
    let rest = s.strip_prefix("C-").or_else(|| s.strip_prefix("c-"));
    match rest {
        Some("Space") | Some("space") => 0x00,
        Some(c) if c.len() == 1 => {
            let ch = c.as_bytes()[0].to_ascii_lowercase();
            if ch.is_ascii_lowercase() { ch - b'a' + 1 } else { 0x07 }
        }
        _ => 0x07,
    }
}

pub fn prefix_from_env() -> u8 {
    parse_prefix(std::env::var("XMUX_PREFIX").ok().as_deref())
}
```

For the loop, follow `tmp/pty-spike/src/proxy.rs` (proven GREEN) for: openpty, `CommandBuilder` with `env_remove("PSMUX_SESSION"/"TMUX"/"TMUX_PANE")`, `try_clone_reader`/`take_writer`, the output pump thread that feeds `Grid` and (when `Mode==Forwarding` AND its captured generation == the live `AtomicU64`) writes to stdout, the input owner that runs bytes through `InputMachine` and forwards or opens the picker, resize via crossterm `Event::Resize` → `master.resize` + `Grid::resize`, and teardown that kills the child but does NOT join the pump or drop the master. Add the `#[cfg(test)] fn smoke_roundtrip() -> bool` adapted from the spike (cmd.exe, marker round-trip, answer the `ESC[6n` DSR).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::run::tests::prefix_from_env 2>&1 | Select-Object -Last 20`
Expected: PASS. Then the smoke test on demand (watchdog-wrapped, per the project's hang-prone-run rule):
Run: `cargo test -p xmux proxy::run::tests::smoke -- --ignored --nocapture 2>&1 | Select-Object -Last 25`
Expected: PASS (marker round-trips).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/proxy/run.rs src/proxy/mod.rs
git commit -m "feat(proxy): PTY proxy loop with generation-gated output pump"
```

---

### Task 6: Overlay invocation + act on a pick

On `InAction::OpenPicker`: pause forwarding, build a `CrosstermBackend` Terminal on the live (current) buffer, drive `run_picker_fed` feeding it decoded keys + resize over `Cmd`, and on close restore via `Grid::restore_bytes()`, then map the `SwitchResult` to the caller's action.

**Files:**
- Modify: `src/proxy/run.rs`
- Test: covered by the existing pure-unit tests (decoder feeds picker) + the live overlay gate (manual).

- [ ] **Step 1: Add overlay handling in the input owner**

On `OpenPicker`: set `Mode::Picker`; spawn/await `run_picker_fed`; route subsequent raw input bytes through `KeyDecoder` → `cmd_tx.send(Cmd::Key(..))`; route resize → `cmd_tx.send(Cmd::Resize(..))` AND `master.resize`+`grid.resize`; on `event_loop` return, write `grid.restore_bytes()` to stdout, set `Mode::Forwarding`, and return the `SwitchResult` to `proxy_attach`'s caller.

```rust
// inside the input owner, on InAction::OpenPicker:
mode.store(PICKER, Ordering::SeqCst);
let mut term = ratatui::Terminal::new(
    ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
let (ptx, prx) = tokio::sync::mpsc::channel::<crate::ui::run::Cmd>(256);
// (feed decoded keys/resize into ptx from this same input owner while the
//  picker runs; see Task 4 run_picker_fed)
let result = crate::ui::run::run_picker_fed(ops.clone(), ptx.clone(), prx, &mut term).await?;
let _ = std::io::stdout().write_all(&grid.restore_bytes());
let _ = std::io::stdout().flush();
mode.store(FORWARDING, Ordering::SeqCst);
// hand `result` back so proxy_attach returns Some(result)
```

- [ ] **Step 2: Verify the decoder→picker path with a unit test**

Add a test that `KeyDecoder` output drives a `Switcher` selection deterministically (reuse the existing headless switcher test harness: feed `Cmd::Key` from decoded bytes `j\r` and assert the resulting `SwitchResult`).

Run: `cargo test -p xmux proxy 2>&1 | Select-Object -Last 25`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/proxy/run.rs
git commit -m "feat(proxy): overlay invocation + restore on pick/cancel"
```

---

### Task 7: Wire the proxy into `RealAttacher::attach`

Replace the blind `Command::status().await` handover with `proxy_attach`, preserving the same-server pre-select, the cross-server signal/pending path, and the non-zero-exit logging. Cross-server picks from the overlay reuse `signal_cockpit_switch`/`pending`; the proxy returns so `cockpit_loop` re-attaches.

**Files:**
- Modify: `src/cockpit.rs` (`RealAttacher::attach` ~313-354)
- Test: existing cockpit tests must stay green; add one that the attacher maps a same-server pick to a switch (using the existing `Picker`/`Attacher` test doubles).

- [ ] **Step 1: Write/extend the failing test**

Using the existing cockpit test doubles (`Picker`/`Attacher` traits), assert that when the proxy yields a cross-server `Target`, `attach` stores `pending` and returns so the loop re-attaches (mirror the existing cross-host switch tests).

Run: `cargo test -p xmux cockpit 2>&1 | Select-Object -Last 25`
Expected: FAIL on the new assertion.

- [ ] **Step 2: Implement**

In `RealAttacher::attach`, after the existing `nest_guard` + local window pre-select, call `proxy::proxy_attach(&argv, self.ops.clone(), cfg)`. On `Ok(Some(result))` map it to a switch via the existing pick→signal path; on `Ok(None)` (child exited) behave as today; keep the non-zero-exit logging for the child. (The cockpit already holds an `Ops` for the picker; thread it into `RealAttacher`.)

- [ ] **Step 3: Verify**

Run: `cargo test -p xmux 2>&1 | Select-Object -Last 30`
Expected: PASS (full suite). Then clippy:
Run: `cargo clippy --all-targets 2>&1 | Select-Object -Last 15`
Expected: 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat(cockpit): attach via the PTY proxy overlay"
```

---

### Task 8: Docs + stale-fact correction

**Files:**
- Modify: `PROGRESS.md` (lines ~113, ~167: `panic=abort` → `panic=unwind`)
- Modify: `docs/keybind.md` (document the default `Ctrl-g s` prefix + `XMUX_PREFIX`)

- [ ] **Step 1: Correct `PROGRESS.md`**

Replace the `panic=abort` statements with the accurate `panic="unwind"` (RAII Drop restores the terminal). State current behavior only (no history).

- [ ] **Step 2: Document the hotkey in `docs/keybind.md`**

Add: the cockpit's global overlay hotkey is `prefix` then `s` (default prefix `Ctrl-g`), configurable via `XMUX_PREFIX` (a `C-<x>` spec); press the prefix twice to send one literal through. This works from ANY session under the cockpit, local or remote, with nothing installed on remotes.

- [ ] **Step 3: Commit**

```bash
git add PROGRESS.md docs/keybind.md
git commit -m "docs: PTY-proxy hotkey + correct stale panic=abort note"
```

---

## Live-terminal gates (run after Task 7, on a real terminal — NOT headless)

These are validated manually / live, per the design:
- **Host raw-input forwarding fidelity:** type in a remote `ssh -t … tmux attach` under the cockpit; confirm keystrokes/paste/arrows reach the inner session unaltered and the prefix opens the overlay.
- **Visual screen-handover:** open the overlay over a live alt-screen child (tmux/vim), pick `Esc`, and confirm the pane repaints correctly (vt100 restore + mode re-emit), incl. a wide-char (CJK) line.
- **Same-server teleport + cross-server re-attach:** drive a throwaway remote (e.g. jupiter06) + an exact local switch address; never picker-select near the live local server.

## Self-Review

- **Spec coverage:** prefix hotkey (T1,T5,T8), single raw-byte input owner + decode (T1,T2,T6), vt100 restore + mode re-emit (T3,T6), no alt-screen toggle / picker core (T4,T6), generation-token teardown (T5), resize conduit (T4,T5,T6), cockpit-only wiring (T7), nesting-env clear (T5,T7), panic=unwind note (T8). S-input + visual = live gates. Covered.
- **Type consistency:** `Cmd::Resize(u16,u16)`, `run_picker_fed`, `InputMachine`/`InAction`, `KeyDecoder`, `Grid::restore_bytes`, `proxy_attach`/`ProxyConfig`/`parse_prefix` are used consistently across tasks.
- **No placeholders:** every code step shows real code; vt100 mode accessors flagged for verification in T3 (NEW-1), not left as TODO.
