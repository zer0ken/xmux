# PTY-proxy overlay: a fast local cross-host picker from any session

## Goal

Let a global hotkey open the xmux cross-environment picker as a **fast local
overlay from ANY session — local OR remote — with nothing installed on the
remote host**. The picker draws over the live pane, and a pick either teleports
in place (same server) or re-attaches across hosts (the existing cockpit path).

This is Track 1 of three. Tracks 2 (picker/switch UX) and 3 (latency/render/scan
optimization) are deferred to their own specs; their evidenced target lists live
in `tmp/pty-brainstorm/02-repo-pain-points.md` and are summarized under
[Deferred work](#deferred-work).

## Status

**Strategy and PTY plumbing: proven (spike GREEN).** **Implementation: gated** on
two further spikes (S-local, S-input below) plus the design fixes in this doc.
The first design draft was revised after an adversarial review found the overlay
mechanism contradicted the picker it reused; that review's findings are resolved
here (`tmp/pty-brainstorm/04-adversarial-doc-review.md`).

## Why the cockpit must own the byte stream

Today the cockpit hands the terminal off blindly. `RealAttacher::attach`
(`src/cockpit.rs:313`) runs `tokio::process::Command::new(&argv[0]).args(..).status().await`,
and `OsExecer::exec` (`src/attach.rs:42`) does the same with `std::process`. The
cockpit is therefore NOT in the byte path: it cannot see a keystroke or draw
anything until the child exits.

The in-mux popup (`prefix g` → `display-popup -E "xmux popup"`, `docs/keybind.md`)
does not solve this: that binding executes on whichever mux server the client is
attached to, so from inside a REMOTE session it would run `xmux` ON THE REMOTE
HOST — which xmux deliberately does not install. The only no-remote-install
option today is binding the remote key to `detach-client`, a slow full-screen
bounce, not a fast overlay.

Fix direction: the cockpit owns a PTY, runs the attach child (ssh/tmux/psmux
attach) on the slave, copies bytes both ways, and watches the input stream for a
rare global hotkey. On the hotkey it pauses forwarding, draws the LOCAL picker
overlay (the local xmux already knows every host), then acts — relocating the
feature entirely local (nothing on remotes, fast over the live pane), exactly how
tmux/mosh own a prefix regardless of what runs inside.

## Scope (v1)

- **v1 is cockpit-only.** Only `RealAttacher::attach` (run inside `cockpit_loop`)
  gets the PTY proxy. The cockpit already has the control plane a cross-server
  pick needs (`pending` / `signal_cockpit_switch` / freshness / re-attach
  supervisor).
- **`OsExecer::exec` (out-of-mux direct attach) keeps its blind handover in v1.**
  A direct `xmux attach host/session` has no cockpit loop, so a cross-server pick
  there has nowhere to go (`decide_popup_action` → `NoCockpit`); giving `OsExecer`
  the overlay would import an unsolved cross-server-without-cockpit problem and
  effectively make it a second cockpit. Out-of-mux users keep the existing
  in-mux popup for same-server peek/teleport. This is a strictly smaller,
  coherent v1; the documented supported entry is already the cockpit
  (`keybind.md` "Cockpit precondition").

## Feasibility — spiked

A throwaway spike (`tmp/pty-spike/`, run under a 15s OS watchdog so it can never
hang the loop) validated the load-bearing plumbing on this Windows/ConPTY box:

- **portable-pty 0.9 bidirectional ConPTY proxy: OK.** Open a PTY, spawn a child
  on the slave, write a command to the master, read the child's banner + echoed
  command + output back (341 bytes round-tripped, marker present).
- **vt100 0.16 reconstruction: OK.** Feeding the real ConPTY output into a
  `vt100::Parser` reproduced the screen via `contents_formatted()` (274 bytes),
  and the parser tracks the alternate screen across `?1049h`/`?1049l`.

Three findings are baked into the design:

1. **ConPTY `read_to_end` deadlocks.** The cloned master reader does not return
   EOF after the child exits/kills. The output pump is a bounded read loop on a
   dedicated thread, and teardown must NOT `join` the pump nor `drop` the master
   (both block on the outstanding read) — see [Teardown](#teardown-ordering).
2. **ConPTY runs a terminal handshake.** On startup it emits win32-input-mode
   (`?9001h`), focus reporting (`?1004h`) and a DSR cursor-position query
   (`ESC[6n`), then waits for the terminal to answer before the child proceeds.
   The proxy must forward these queries to the real terminal AND relay its
   replies back to the master byte-for-byte — it must never swallow terminal
   queries or their replies, or the child wedges.
3. **The overlay must not toggle the alternate screen.** See
   [Overlay composition](#overlay-composition).

**Still unproven (two gating spikes required before implementation):**

- **S-local — local mux attach over the portable-pty slave.** The spike used
  `cmd.exe`; the load-bearing case is `psmux/tmux -S <sock> attach` on the slave,
  which is pickier about its controlling tty. This is the most-used path (the
  cockpit's own local session) and is historically hard to verify headless
  (`xmux-cockpit-local-attach-headless-untestable`). Spike it before building.
- **S-input — host raw input on Windows.** The spike proved PTY *child* I/O, not
  reading the *host* console as a forwardable raw byte stream with prefix
  detection (win32-input-mode / raw console read). Spike the host-input read +
  the minimal byte→key decode for the picker (below) before building.

The visual screen-handover (does the live pane look right after the overlay
closes) needs a real terminal / human eye and stays a gated manual check.

## Architecture

The cockpit's attach step changes from a blind `Command::status()` handover into
a **PTY proxy loop** with: a PTY pair, an output pump, a single host-input
reader, and a picker overlay, coordinated by a shared `Mode` + a per-attach
generation token.

### PTY ownership and threads

`portable-pty 0.9` (`native_pty_system()`) gives one API over unix
openpty/forkpty and Windows ConPTY. Its reader/writer are **blocking**, and
xmux's tokio runtime is current-thread, so PTY I/O lives on dedicated
`std::thread`s. The async `Attacher::attach` bridges to them via a blocking
join handle it awaits (e.g. `spawn_blocking` or a thread whose completion is
signalled back) — see [L: process-lib bridge](#low-severity-notes).

- `openpty(PtySize)` → `pair.master` + `pair.slave`.
- Build the child from `Source::attach_command(...)` (`src/source.rs:164`,
  unchanged) and `pair.slave.spawn_command(cmd)`.
- `pair.master.try_clone_reader()` → output pump; `pair.master.take_writer()`
  → master writes.

A shared `Mode` (`Forwarding` | `Picker` | `Quitting`) plus a `generation: u64`
(bumped each attach) are read by the pumps.

### Output pump (master → terminal, always tee'd into vt100)

A dedicated thread loops `reader.read(&buf)`. For every chunk:

- Always feed it to a `vt100::Parser` so the grid stays current.
- In `Forwarding`, also write it straight to real stdout (memcpy-cost latency).
- In `Picker`, withhold the stdout write but keep feeding the parser.
- **Before every stdout write, check the pump's captured `generation` against the
  current one; a stale pump (from a previous attach that never EOF'd) writes
  nothing and exits.** This is what makes "don't join the pump" safe (finding 1).
- On EOF/read-error, signal `Quitting`.

### Host input — single owner, raw bytes, mode-switched

There is exactly one stdin. The proxy is its single owner; the picker never
spawns its own reader (this removes the two-`EventStream`-on-one-fd race).

- **Forwarding:** read host input as **raw bytes** and forward them byte-for-byte
  to the master `writer` (lossless — this also carries the terminal's replies to
  ConPTY's startup queries, finding 2). Prefix detection is a byte-level state
  machine (below). Forwarding decoded crossterm events back to bytes is rejected:
  event→byte re-serialization is lossy for mouse/paste/function keys, so the
  forward path must be raw bytes.
- **Picker:** the proxy stops forwarding and decodes the same raw input with a
  **minimal byte→key decoder** (printable UTF-8, arrows/CSI, Enter, Esc, Backspace
  — the keys the picker uses) into `Cmd::Key`, fed to the reused picker core over
  the existing `Cmd` channel (`src/ui/run.rs:342` already injects `Cmd::Key` this
  way). Mouse is not decoded in the overlay (keyboard-only v1).

`S-input` must confirm the host raw-byte read + this decode on Windows ConPTY
before implementation.

### Hotkey state machine (byte-level)

Prefix model (a single rare leading key, then an action key) — not a bare chord:
the proxy intercepts before the inner mux and every inner app, so a bare chord
would be stolen forever with no escape; a prefix steals only the leading key and
gives the tmux escape hatch (prefix twice → one literal through).

States (byte stream, Forwarding mode):

```
Idle
  ── prefix byte, NOT in paste, at a sequence boundary ──▶ Armed
  ── any other byte ──▶ forward it, stay Idle
Armed
  ── action key (e.g. `s`) ──▶ enter Picker, clear Armed
  ── prefix byte again ──▶ forward ONE literal prefix byte, Idle   (send-prefix)
  ── timeout (no key within ~400ms) ──▶ forward ONE literal prefix byte, Idle
  ── any other byte ──▶ forward prefix byte THEN that byte, Idle
Paste sub-state
  ── on `ESC[200~` ──▶ in-paste: NEVER match the prefix until `ESC[201~`
```

The **timeout** matters: it makes a lone reflexive `Ctrl-g` (readline/emacs abort)
still reach the inner app without a deliberate double-tap. The machine must
respect UTF-8 boundaries and never match the prefix byte inside a multi-byte
escape sequence (arrows, mouse, the DSR reply) or a bracketed paste. It forwards
unmatched bytes allocation-free on the hot path.

### Hotkey default and config

- **Default prefix: `Ctrl-g` (0x07)** — distinct from the inner mux prefix
  (`Ctrl-b`), a single byte (trivial to detect). **Honest cost:** `Ctrl-g` is
  readline/emacs abort and is hit *frequently and reflexively*; the prefix-arm
  timeout (above) preserves it as a literal abort when not followed by an action
  key, which is the real mitigation (not the double-tap alone).
- **Action: prefix then `s`** opens the switcher.
- **Configurable** via one env/config key (`XMUX_PREFIX`, a `Ctrl-<x>` spec); only
  the prefix byte is configurable. `Ctrl-Space` (NUL) was the dissenting panel
  pick (rarely bound in CLI tools) but ConPTY/terminals deliver it inconsistently;
  hence neither default is clean and the knob exists. The prefix must never be the
  inner mux prefix.

### Overlay composition

**The proxy owns terminal screen + mode policy and NEVER toggles the alternate
screen.** It draws the picker on the buffer the child already established and
restores from its vt100 grid.

- The reused picker is the `event_loop` core extracted from `run_switcher`
  (`src/ui/run.rs:363`), invoked **without** `TerminalGuard` (which today emits
  `EnterAlternateScreen`/`EnableMouseCapture`, `run.rs:266-270`) and **without**
  `read_events` (the proxy is the sole input source). `run_switcher` keeps its
  current behavior for the standalone full-screen picker; the proxy calls the
  core directly. The earlier "reuse `run_switcher` unchanged" claim was wrong —
  this refactor is required.
- Why not a real alt-screen toggle from the proxy: the alternate screen is a
  single, non-stacking buffer cleared on entry (xterm `ctlseqs`, `?1049`). The
  child (tmux / ssh→remote tmux / vim) is usually already on it, so a proxy
  `?1049h` would clear the child's screen and `?1049l` would restore the *primary*
  buffer, losing the live pane. Toggling from the proxy also collides with the
  picker's own `TerminalGuard` toggle (double `?1049l`).

Overlay flow on the hotkey:

1. Input machine matches `prefix s` → `Mode::Picker` (swallow the keys).
2. Output pump pauses stdout forwarding (parser keeps consuming).
3. Draw the picker core on the current buffer (no alt-screen toggle).
4. On selection or `Esc`: act (below), set `Mode::Forwarding`, then **restore**:
   `stdout.write(parser.screen().contents_formatted())` AND re-emit the child's
   tracked terminal modes from the parser (cursor position + visibility,
   application-cursor / keypad, mouse-reporting, bracketed-paste). See losses.

#### What the grid restore preserves and loses

`contents_formatted()` repaints the visible cells + SGR. The proxy additionally
re-emits the parser-tracked private modes (cursor, app-cursor/keypad, mouse,
bracketed-paste) so the child's input handling is not silently broken after an
overlay close. This **assumes `vt100 0.16` exposes those tracked modes via public
API** — the spike proved the grid round-trips but not the private-mode surface;
the fidelity test (below) must verify it, and fall back to child-repaint if not.
Known losses, accepted and documented:

- **Scrollback above the visible grid is lost** for a child on the *primary*
  screen (a bare shell). Moot for an alt-screen child (tmux/vim) — the common
  case here, since the proxy wraps a mux attach.
- **Cursor position/visibility and wide-char (CJK) cells** must be asserted by a
  fidelity test (this box's own prompt contains wide Hangul); the spike proved
  contents round-trip but not these specifics. Part of S-input / a render test.

If the fidelity test fails for the mux case, the fallback is child-repaint (ask
the mux to `refresh-client`); it is mux-specific and flickers, so it is the
fallback, not the default.

### Act on a pick — reuse the existing cockpit semantics

The picker yields a `Target` (session + optional window):

- **Same server:** teleport in place (`switch-client` / key-inject), no detach.
- **Cross server:** signal the cockpit (`signal_cockpit_switch` / `pending` /
  freshness) and return so the cockpit re-attaches — the established model
  (`docs/solutions/architecture-patterns/cockpit-cross-host-switch.md`).
- **Esc / cancel:** restore and resume forwarding, pane untouched.

### Teardown ordering

Owning the byte stream changes when `attach()` returns, so this is specified, not
assumed:

1. On a cross-server pick, the proxy: stores `pending` + signals the cockpit
   (this happens at pick-confirm, right before teardown — so the freshness window
   still measures signal→re-attach, not human picker time), then kills the child,
   sets `Mode::Quitting`, **bumps the generation**, and returns from `attach()`
   **without** joining the output pump or dropping the master (finding 1).
2. The detached old pump may still be alive on its blocking read; because it
   checks `generation` before any stdout write, once the loop starts the next
   attach (new generation) the stale pump self-silences and never writes to the
   shared stdout. This prevents two pumps fighting over stdout.
3. The freshness window (`SWITCH_FRESH_WINDOW`, `cockpit.rs:159`) is unchanged:
   `pending` is stored at pick-confirm immediately before teardown.

### Terminal restore / panic path (single owner)

Release profile is `panic = "unwind"` (`Cargo.toml:16-23`) so RAII `Drop` restores
the terminal on panic. (`PROGRESS.md:113,167` still says `panic=abort`; **that is
stale and is corrected as part of this work item.**) Because the proxy-mode picker
does NOT install its own alt-screen `TerminalGuard` (overlay section), there is a
**single** terminal-restore owner: the proxy's `Drop` guard. On normal exit or
panic it does a plain `disable_raw_mode` + ensure-no-stray-alt-screen + kill child;
it does **not** repaint from the grid on the panic path (unsafe per finding 1) and
must not block on the pump/master. No double `?1049l`.

### Resize

One source of truth: crossterm `Event::Resize(cols, rows)` (portable across
Windows and unix), read by the proxy's input owner. On each event, unconditionally
and immediately: `pair.master.resize(..)`, then `parser.set_size(rows, cols)`, and
— **if the picker is open** — inject the resize into the picker over a **new
`Cmd::Resize(cols, rows)` variant** (none exists today; added to `src/ui/run.rs`
with an `event_loop` arm) so ratatui re-lays-out (the picker no longer has its
own `EventStream`). The picker also reads the live size at draw time. Repaint after a resize uses full
`contents_formatted()`, not a diff (stale baseline across a size change). A unix
`SIGWINCH` handler is an optional lower-latency add-on.

## Where it slots into the code

- `src/cockpit.rs` `RealAttacher::attach`: replace the
  `tokio::process::Command::...status().await` handover with the PTY proxy loop.
  Existing pre-checks stay. **`nest_guard` stays** (`cockpit.rs:322`): it guards
  the *cockpit process's* `$TMUX` env, which the PTY does not change — the slave's
  `$TMUX` belongs to the child, not the cockpit.
- `src/attach.rs` `OsExecer::exec`: **unchanged in v1** (out-of-mux is descoped).
- New module (e.g. `src/proxy.rs`): PTY pair, pumps, `Mode` + generation, the
  byte-level input state machine, the byte→key decoder, the vt100 grid, the
  overlay hook into the extracted picker core.
- `src/ui/run.rs`: extract the `event_loop` core so the proxy can drive it without
  `TerminalGuard`/`read_events`; `run_switcher` keeps wrapping it for standalone
  use. Add a `Cmd::Resize(cols, rows)` variant + an `event_loop` arm that
  re-lays-out (none exists today).
- `src/source.rs` `attach_command`: unchanged.

## Dependencies

Add `portable-pty = "0.9"` and `vt100 = "0.16"`. `crossterm`, `tokio`, `ratatui`
already present. Note: the project optimizes for size (`opt-level="z"`, LTO; a
~970 KB binary per `PROGRESS.md`); these deps add a measurable but accepted size
delta — measure it as part of the work, not a surprise.

## Risks

1. **Input mis-framing (top risk).** A byte scan that matches the prefix inside a
   bracketed paste, a UTF-8 rune, an escape sequence, or ConPTY's DSR reply
   corrupts input or pops the picker mid-paste. Mitigation: the byte-level state
   machine above; unit-test it against paste bursts, UTF-8, arrow/mouse/DSR
   sequences, prefix double-tap, and prefix-then-timeout.
2. **Host raw input on Windows (S-input).** Reading the host console as a
   forwardable raw byte stream is unproven; gate on the spike.
3. **Local mux attach over the slave (S-local).** Unproven for tmux/psmux; gate.
4. **vt100 restore fidelity (C3).** Scrollback/cursor/wide-char/private-mode gaps;
   mitigated by mode re-emit + documented scrollback loss + a fidelity test;
   child-repaint fallback.
5. **ConPTY teardown deadlock / handshake** — mitigated by findings 1–2 and the
   generation token.
6. **Mouse on ConPTY is lossy** — picker is keyboard-only in the overlay (mouse
   capture disabled in proxy-mode); document it.

## Testing strategy

- **Gating spikes:** S-local (psmux/tmux attach over the slave) and S-input (host
  raw read + picker byte→key decode) must run GREEN before implementation.
- **Unit tests:** the byte-level input state machine (paste/UTF-8/escape/DSR
  framing, prefix double-tap, prefix-timeout) and the vt100 restore (alt-screen
  reconstruct, cursor + one wide-char/CJK assertion).
- **Live verify:** same-server teleport and cross-server re-attach over real
  psmux + ssh, headless via a mux harness where possible.
- **Manual gate:** the visual screen-handover after an overlay close (human eye).

## Out of scope / accepted limitations

- Tracks 2 and 3 (UX, optimization) — separate specs.
- Out-of-mux (`OsExecer`) overlay hotkey — v1 cockpit-only.
- Picker mouse over ConPTY (keyboard-only overlay).
- Scrollback restore for a bare-shell (primary-screen) child.
- Per-terminal cockpit addressing and the Windows-no-ControlMaster handshake cost
  remain as previously documented.

## Deferred work

From `tmp/pty-brainstorm/02-repo-pain-points.md`: detail-pane actionability,
filter preservation across rescan, create auto-focus, default popup-binding
discoverability, per-host rescan, capture caching/throttling, time-to-full-data
under ~1s. Specced later.

## Low-severity notes

- **Process-lib bridge:** the proxy is blocking `std::thread`s awaited from a
  current-thread tokio runtime via a `spawn_blocking`/join-await bridge; call it
  out so it isn't discovered late. `OsExecer`'s sync `Execer` trait is untouched
  in v1.
- **Latency:** the "memcpy-cost, no round-trip" claim holds because forwarding is
  raw bytes (not decode→re-serialize) and the input machine is allocation-free.
- **Mouse capture:** the proxy-mode picker disables mouse capture (the standalone
  `run_switcher` still enables it).
- **PROGRESS.md** `panic=abort` lines are corrected by this work item.

## Decision record

- PTY crate: **portable-pty 0.9** (only mature ConPTY+unix-in-one-API crate).
- Overlay: **proxy owns screen/mode policy, no alt-screen toggle**; picker core
  extracted from `run_switcher` and driven by the proxy; restore via vt100
  `contents_formatted()` + re-emit tracked private modes.
- Input: **single owner, raw-byte forwarding** + byte-level prefix state machine
  with timeout; **minimal byte→key decode** feeds the picker via the `Cmd`
  channel; no second `EventStream`.
- Hotkey: **prefix**, default `Ctrl-g`, configurable, double-tap + arm-timeout
  passthrough.
- Scope: **cockpit-only v1**; `OsExecer` unchanged.
- Resize: **always forward** (master + parser + picker injection).
- I/O: dedicated threads + atomic `Mode` + per-attach generation token + a single
  `Drop` restore owner under `panic=unwind`.
- Gating spikes: **S-local** (mux attach over the slave), **S-input** (host raw
  read + picker decode) before implementation.
