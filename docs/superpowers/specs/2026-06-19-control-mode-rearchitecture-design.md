# xmux — Control-mode attach re-architecture (design)

- Date: 2026-06-19
- Branch: `feat/rust-rewrite`
- Status: design — proceeding to TDD plan
- Supersedes: `docs/superpowers/specs/2026-06-19-live-attach-preview-design.md`
  (the per-session PTY-proxy attach model). That model's `Attachment` /
  `AttachRegistry` / `LiveOwner` / raw-passthrough machinery is replaced wholesale
  by the control-mode model below. The vt100 `Grid` + `Grid::render_into` bridge,
  the switcher tree/nav/filter/rename/kill, the `xmux ctl` control socket,
  `attach::nest_guard`, and the config `[ui]` table are kept.

## Goal

xmux is a persistent terminal application that owns the user's terminal for the
whole session. It presents one cross-host tree of tmux/psmux sessions and lets the
user switch among them instantly. The right-hand region shows the **live** terminal
of the selected session.

The attach layer is re-architected onto tmux/psmux **control mode** (`-CC`). xmux
holds **one control-mode connection per host** (local + each remote). That single
connection both (a) enumerates and tracks that host's tree (`list-sessions` /
`list-windows` plus live `%sessions-changed` / `%window-*` notifications) and
(b) drives the live view: `switch-client -t <session>` re-points the connection,
its `%output` stream feeds a vt100 `Grid`, and the `Grid` is rendered to ratatui.
The number of child processes is bounded by the **number of hosts**, not the number
of sessions. Human keystrokes for the focused session are forwarded to its active
pane via `send-keys`; xmux stays programmatically controllable via the `xmux ctl`
socket.

Protocol facts are cited from `tmp/control-mode-research.md` (the authoritative
research) by section, e.g. `[research §4]`. Wire details are not re-derived here.

## Terminology

UI regions (canonical names — use these in code identifiers and docs):

- **Sidebar** — the left region in Overlay; its content is a one-line **title**
  (`xmux: cross-host MUX manager`), the **session tree** (hosts → sessions →
  windows → panes), and the **footer/help** line. Hidden in Passthrough.
- **Terminal view** — the region showing the live terminal of the selected session
  (a vt100 `Grid` rendered to ratatui). Right of the sidebar in Overlay; the whole
  screen (minus the status bar) in Passthrough. Never called "pane" (avoids
  clashing with the mux's own panes) or "preview" (it is live, not a snapshot).
- **Status bar** — the bottom row in Passthrough (info only, drawn by ratatui).

Runtime concepts:

- **HostClient** — one per host (local + each remote). Owns the control-mode child
  process, the protocol reader thread, the command writer, a per-host session/window
  **inventory**, the **attached session id** (what `switch-client` last selected),
  the **active pane id** of that session, and one vt100 `Grid` for the
  currently-displayed session. Bound = number of hosts.
- **Control-mode child** — the piped child speaking the `-CC` text protocol over
  stdin (commands) / stdout (protocol). Local: `psmux -CC …`. Remote:
  `ssh <host> tmux -CC …` — **no `ssh -t`** (control mode is a text protocol over
  pipes, not a pty) `[research §1]`.
- **Selected session** — the session the cursor is on in the tree (Overlay) or the
  one being shown fullscreen (Passthrough). Selecting a session = attaching its
  host's connection to it (`switch-client`).
- **Connecting** — a HostClient whose child has been spawned but whose first
  `list-sessions` / first post-`switch-client` `%output` has not yet arrived. The
  session row shows a braille spinner while connecting.

## 1. State model & transitions

Two states only: **Overlay** (sidebar shown) and **Passthrough** (sidebar hidden,
selected session fullscreen). Rendering is **always Grid → ratatui** in both states
— ratatui owns stdout every frame; there is **no raw-byte stdout passthrough**.

- **Overlay** — ratatui draws: the one-line title, the session tree (with filter,
  rename, kill, spinner), the footer/help, and the terminal view (the live `Grid`
  of the selected session). Arrow / `j`/`k` navigation moves the cursor; moving the
  cursor to a session row **attaches** that session (select = attach, §5). The
  terminal view follows the cursor.
- **Passthrough** — ratatui draws the selected session's `Grid` fullscreen at
  `cols×(rows-1)`, plus a one-line status bar on the last row. The title, tree, and
  footer are hidden. Human keystrokes forward to the session's active pane (§6).

| From | Event | To |
|------|-------|-----|
| (start) | launch | Overlay; cursor preselected on the globally most-recent session; that session's host connects lazily and is attached |
| Overlay | cursor moves to a session/window row | (stays Overlay) ensure host connected → `switch-client` to it; terminal view follows |
| Overlay | xmux prefix (`C-g`) then `s` | Passthrough of the selected session (fullscreen) |
| Overlay | `Enter` | **no-op** |
| Overlay | `q` | quit xmux |
| Passthrough | xmux prefix (`C-g`) then `s` | Overlay (cursor on the session that was fullscreen) |
| Passthrough | xmux prefix (`C-g`) then `q` | quit xmux |
| Passthrough | the shown session is destroyed (`%sessions-changed` removes it, or `%exit`) | Overlay; cursor falls to the next session on that host (or the global most-recent) |

There is no "foreground attachment", no `Esc`-returns-to-previous, no dwell, and no
`Enter`-promote. The toggle between Overlay and Passthrough is purely a chrome
change over the **same** selected session; both render that session's `Grid`. The
selected session is shared state, so toggling never re-attaches or resizes.

## 2. Concurrency & ownership (core)

The single-threaded tokio runtime (`#[tokio::main(flavor = "current_thread")]`,
`main.rs`) is kept. As in the prior model, **all blocking work lives on dedicated OS
threads**, never on the event loop. Each HostClient owns its own threads; the event
loop only sends commands and consumes parsed events over channels.

### HostClient

One per host alias. Created lazily (§3). Owns:

```
HostClient {
  host:        String,              // source alias ("local" or ssh alias)
  child:       Box<dyn Child + Send + Sync>,   // the -CC child (kill on teardown)
  cmd_tx:      mpsc::Sender<HostCmd>,           // → writer thread (FIFO command queue)
  grid:        Arc<Mutex<Grid>>,    // the currently-displayed session's vt100 grid
  state:       Arc<Mutex<HostInventory>>,       // sessions/windows, attached id, active pane
  size:        (u16, u16),          // last refresh-client -C size pushed
  connecting:  Arc<AtomicBool>,     // true until first list-sessions / first %output
}
```

`HostInventory` holds the parsed session list (name, window count, last_attached,
attached flag), per-session window list, the `attached_session` id/name (set by the
last successful `switch-client`), and the `active_pane` id (`%N`) of the attached
session (tracked from `%window-pane-changed` / `%session-window-changed` and a
`display-message` query, §4).

### Per-HostClient threads

- **Reader thread** — reads the child's stdout, runs the line-framed protocol parser
  (§4), and dispatches: `%output`/`%extended-output` → decode into the reused buffer
  → feed `grid`; `%begin…%end|%error` → resolve the in-flight command at the FIFO
  front; notifications → update `HostInventory` and emit a `HostEvent` to the event
  loop (via an mpsc channel folded into the cockpit's `select!`). The reader owns the
  parser state machine and the decode buffer; nothing else touches them.
- **Writer thread** — drains a FIFO `mpsc<HostCmd>` and writes each command line
  (`cmd\n`) to the child's stdin, pushing the command's correlation entry onto the
  in-flight FIFO the reader pops. Blocking writes to a stalled child (e.g. a slow
  ssh remote) happen here, never on the loop. (Mirrors the prior `pty_control_loop`
  off-loop discipline.)

`HostCmd` variants: `Send(Vec<u8>)` (a raw command line, newline-terminated),
`SendKeys{ pane, bytes }` (built into a batched `send-keys -H` line, §6),
`SwitchClient{ target }`, `Resize{ cols, rows }`, `Shutdown`. The writer builds the
exact command bytes; the reader correlates the reply.

### No stdout owner, no live-owner gate

Because rendering is always Grid → ratatui, **there is no per-attachment raw stdout
writer and no `LiveOwner` token**. ratatui owns stdout in both states; the event
loop draws every frame from the selected HostClient's `grid`. The prior
`LiveOwner`, the per-pump `is_owner` gate, and the status-bar `scan_clear`
clear-detect/repaint are **removed** (§9).

### Teardown / reap

- **Per HostClient**: send `HostCmd::Shutdown` (writer writes nothing and returns),
  drop `cmd_tx`, `child.kill()`. The reader exits on child-stdout EOF. Join the
  threads with a bounded timeout off the loop.
- **On `%exit [reason]`** `[research §3]`: the reader emits `HostEvent::Exited`; the
  event loop reaps that HostClient (clears its inventory, marks the host
  unreachable / re-connectable). If the reaped host owned the selected session, fall
  to the global most-recent session.
- **On quit**: send `Shutdown` to all HostClients, then bounded-join all in parallel.
  Pre-24H2 Windows `ClosePseudoConsole` does **not** apply (no ConPTY here — the
  control children are piped processes, not pseudo-consoles), so the prior ConPTY
  teardown-stall risk is gone for the control path; a bounded join still guards a
  wedged ssh child.

## 3. HostClient lifecycle

- **Lazy connect**: a HostClient is created on first need — when the cursor first
  selects one of that host's sessions, or at launch for the host owning the
  preselected most-recent session. Until the first `list-sessions` returns (and,
  after a `switch-client`, until the first `%output`), the host/session shows
  "connecting…" with the braille spinner (§5).
- **Connect sequence** (writer thread, in order, immediately after spawn):
  1. `refresh-client -C <cols>x<rows>` — set the control client size (the `WxH`
     `x`-form is correct for 3.3.x; never the comma form) `[research §7]`.
  2. `refresh-client -f pause-after=2` — enable flow control so a firehose pane
     cannot make xmux buffer unbounded or get the client killed "too far behind"
     `[research §8]`.
  3. `list-sessions -F <SESSION_FORMAT>` — seed the inventory (reuses `mux.rs`
     `SESSION_FORMAT`; parsed by `mux::parse_sessions`). For each session,
     `list-windows`/`list-panes -s` as needed to populate the tree (reuses
     `mux::list_panes` / `mux::parse_panes`).
- **Attach a session**: `switch-client -t <session>` (`+ select-window -t
  <session>:<win>` for a window row) `[research §6, §9]`. The connection then streams
  `%output` for that session's panes; xmux observes `%session-changed <id> <name>`
  confirming the move. Clear `connecting` on the first `%output` after the switch.
- **Local vs remote child**:
  - Local: `psmux -CC attach -t <session>` (first attach) — or, for the initial
    handshake before any session is chosen, `psmux -CC new-session -A -s <bootstrap>`
    is **not** used; instead xmux spawns `psmux -CC attach` (attaches to the
    server's default/most-recent session) and immediately `switch-client`s to the
    intended one. If the server has no session, the host shows "(empty)" and no
    child is spawned until a session exists. Local `-S <socket>` is injected exactly
    as `source::exec_argv` does today when xmux runs inside a non-default mux server.
  - Remote: `ssh <host> tmux -CC attach` (or `tmux -CC new -A -s <name>` when the
    target must be created) — **no `-t`** `[research §1]`. xmux connects ssh's
    stdin/stdout to its own pipes.
- **One connection serves all the host's sessions** `[research §9]`: switching among
  a host's sessions is a `switch-client` on the existing connection — never a
  reconnect. Only the **attached** session streams `%output`; non-attached sessions
  of the same host produce no output until selected (acceptable — the terminal view
  only ever shows the selected session).
- **Reconnect on drop**: if the child exits (`%exit`) or its stdout EOFs while the
  host still has sessions, mark the host re-connectable; the next selection of one of
  its sessions re-spawns the HostClient (lazy connect again). A transient drop does
  not erase the last-known tree (the inventory is kept until a fresh `list-sessions`
  replaces it — stale-while-revalidate, as the prior scan cache did).

## 4. Control-protocol handling

All wire details per `[research §2–§9]`; this section states what xmux *does* with
them.

### Framing & command correlation `[research §2]`

- The reader is a line state machine over the child's stdout, line-buffered on `\n`,
  with states `IDLE` and `IN_BLOCK`.
- `%begin <t> <num> <flags>` → pop the front of the in-flight-command FIFO, begin
  collecting body lines, enter `IN_BLOCK` (record `num`). `%end`/`%error <t> <num>
  <flags>` → finalize the body, resolve the command (Ok body / Err body), return to
  `IDLE`. The `<num>` is the correlator; assert it matches the popped entry.
- Notifications never appear inside a block `[research §2]`, so body collection is
  unambiguous; `<flags>` is unused — do not branch on it.

### `%output` / `%extended-output` decode → Grid `[research §4, §8]`

- `%output %<pane> <data>`: decode `<data>` with the exact algorithm in `[research
  §4]` — every `\ooo` (3-digit octal) becomes one byte; all other bytes pass through.
  Decode into a **single reused `Vec<u8>` buffer** owned by the reader (cleared and
  refilled per line — no per-byte allocation). Feed the decoded bytes to `grid` for
  the pane that belongs to the attached session (filter by `<pane-id>` — output flows
  for *all* panes of the attached session `[research §6]`).
- `%extended-output %<pane> <age> … : <data>`: strip up to the single `:`, decode
  `<data>` the same way, note `age` for backpressure.
- The vt100 `Grid` already reassembles UTF-8 / escape sequences (it is a `vt100`
  parser); decoded bytes are fed raw. CR/LF inside output arrive as `\015`/`\012`
  and survive decode — do not strip them.

### Notifications xmux acts on `[research §3]`

| Notification | Action |
|---|---|
| `%session-changed <id> <name>` | confirm a `switch-client`; record `attached_session`; clear `connecting` once the first `%output` follows |
| `%sessions-changed` | re-`list-sessions` for that host → rebuild its tree rows (a session was created/destroyed) |
| `%window-add` / `%window-close` / `%window-renamed` | update that session's window list → rebuild tree rows |
| `%window-pane-changed <win> <pane>` / `%session-window-changed <sid> <win>` | update `active_pane` (where keystrokes go, §6) and the active window |
| `%layout-change …` | (optional) note layout; the `Grid` already reflects geometry via resize |
| `%pause <pane>` | mark the pane paused; when the renderer has caught up, send `refresh-client -A %<pane>:continue` `[research §8]` |
| `%continue <pane>` | clear the paused mark |
| `%client-detached` / `%exit [reason]` | tear down / reap the HostClient (§2); reconnect policy (§3) |
| `%extended-output` | see decode above; carries the buffering `age` |

`%unlinked-*` notifications are ignored (windows not in the attached session)
`[research §3]`. xmux does **not** rely on subscriptions (`refresh-client -B`); output
flows automatically for the attached session `[research §6]`.

### Active-pane tracking (for input)

After a `switch-client`, xmux issues `display-message -p -t <session> -F '#{pane_id}'`
(a `%begin…%end` block) to learn the attached session's active pane, then keeps it
current from `%window-pane-changed` / `%session-window-changed`. The verified spike
confirmed `display-message` returns `PANE=%N SIZE=WxH` over the control connection
(live-review issue log, architecture-decision section).

## 5. Selection, spinner, and tree

### Select = attach (#5/#6/#7)

- Moving the cursor onto a **session** row (Overlay): ensure that host's HostClient is
  connected (lazy connect, §3), then `switch-client -t <session>`. Moving onto a
  **window** row also `select-window -t <session>:<win>`. The terminal view follows
  the cursor and shows that session's `Grid`.
- **`Enter` is a no-op.** There is no dwell (no 500ms timer, no left→right fill bar).
- Host header / loading / pane rows are not attach targets (cursor skips panes/loading
  as today; a host header shows that host's most-recent session's view if connected).
- No coalesce / debounce on selection: cost is bounded by host count (one connection
  per host), and a `switch-client` on an existing connection is cheap. Rapid cursor
  movement across sessions of the **same** host issues back-to-back `switch-client`s
  on one connection (the writer FIFO serializes them); across **different** hosts it
  triggers at most one lazy connect per host, once.

### Spinner (#5)

A single-cell braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, U+2800 block) is drawn to the
**right of the session name** while:

- that session's host is **connecting** (child spawned, first `list-sessions` / first
  post-switch `%output` not yet arrived), OR
- a `switch-client` to that session is awaiting its first `%output`.

The spinner is **indeterminate** (a frame index advanced by the animation tick, §
"animation"); it is not a countdown. It stops when the session's `Grid` has received
output (live) — replaced by the normal row rendering.

### Tree (built from control-connection data)

- The tree is built from each HostClient's `HostInventory` (seeded by
  `list-sessions`/`list-windows`, kept live by `%sessions-changed`/`%window-*`),
  **not** from one-shot probe commands run through `EnvOps`. The streaming
  render-first scan (`Cmd::SourceResult` / `Cmd::Panes`, `spawn_probes` /
  `spawn_panes`) is **reconciled away** for the cockpit: the host skeleton is still
  painted instantly from `Ops::sources()` (sync, no probing), but the sessions/panes
  now stream from each host's control connection instead of detached `list-sessions`
  / `list-panes` probes. `EnvOps`/`Source::list_sessions` remain for the
  non-interactive `xmux ls` / `xmux doctor` paths (they do not open a control
  connection). See §9.
- Ordering and initial selection are unchanged behaviorally: local group(s) first,
  then sessions by `last_attached` descending (`tree::sort_by_recency` /
  `tree::order_groups`); the initial cursor is the globally most-recent session; the
  `user_moved` latch prevents the cursor jumping while inventory streams in.

## 6. Input — dual control

### Human keystrokes (Passthrough; and any byte typed while a session is selected)

- The xmux **prefix** (`C-g`, config `[ui] prefix`) is intercepted by the existing
  `InputMachine` (`proxy/input.rs`): `prefix s` → toggle Overlay/Passthrough, `prefix
  q` → quit, double-prefix → send one literal prefix byte to the pane, stale prefix /
  unknown follow-up handled as today. Bracketed paste is respected (never arm inside a
  paste).
- Every **other** byte (a "Forward" action from `InputMachine`) is forwarded to the
  attached session's **active pane** of the selected host via `send-keys`:
  - Build **one batched command per input burst** (latency/perf): the bytes from one
    stdin read that are not consumed by the prefix machine are emitted as a single
    `send-keys -t %<pane> -H <hex> <hex> …` line (each byte as a 2-digit hex arg).
    `-H` (hex) is the faithful raw-byte path in 3.3 (there is **no `-K`** in 3.3)
    `[research §5]`. Always send to the tracked `active_pane` (`%N`), updated from
    notifications (§4).
  - (Implementation note: `-H` covers all control/escape/arrow input — the only path
    xmux uses. `-l` (literal UTF-8) is an alternative for printable bursts but `-H`
    over the exact bytes is uniformly faithful, so v1 uses `-H` for everything.)
- In **Overlay**, typed bytes drive the tree (decoded via `KeyDecoder`, §"#2");
  they do **not** forward to the pane. Forwarding to the pane happens in
  **Passthrough** (the live fullscreen session). (Selecting in Overlay still attaches
  and streams output; it just does not type into the session.)

### Programmatic control (#10) — `xmux ctl` kept and strengthened

The `xmux ctl` control socket (`control.rs` + the accept/dispatch loop) is kept as a
first-class interface and extended to drive the new model:

- `key <name>` / `text <str>` → injected as `KeyEvent`s exactly as today; in Overlay
  they drive tree nav / filter / rename / kill and **select = attach** (so `key down`
  attaches the next session, headlessly). The cockpit consumes these through the same
  `handle_key` path real keys use.
- `dump` → renders the **current** ratatui frame to a `TestBackend` and flattens it.
  Because the terminal view is a `Grid` rendered to ratatui (not raw passthrough),
  `dump` now captures the **live session view** too — the control-mode view is
  headless-verifiable (this is the key testability win over the old raw passthrough).
- `ping` → `pong` (liveness).
- (Optional, additive) a `passthrough` / `overlay` verb to toggle state headlessly,
  and a `keys <hex…>` verb to inject raw bytes through the Passthrough input path so a
  test can prove `send-keys` forwarding. Additive only; the existing verbs are
  unchanged.

## 7. Sizing & performance

### Sizing

- Each HostClient sends `refresh-client -C <cols>x<rows>` on connect and on every
  terminal-resize event (the `WxH` `x`-form, 3.3.x) `[research §7]`. The control
  client's size drives the attached session's window geometry (`window-size latest`),
  so the `Grid` matches what xmux renders.
- The terminal view is sized to what xmux shows: `cols×(rows-1)` in Passthrough
  (status bar on the last row) and the right-region width in Overlay. On resize,
  recompute and push one `refresh-client -C` per HostClient (off the loop, via the
  writer). The vt100 `Grid` is resized to match (`Grid::resize`).

### Performance (explicit)

- **Flow control**: `pause-after=2` set at connect; only the **attached** session
  streams; `%pause`/`%extended-output age` are backpressure signals → resume with
  `refresh-client -A %pane:continue` after the renderer catches up `[research §8]`.
  Optionally mute off-screen panes of the attached session with `-A %pane:off` (v1
  keeps all panes of the attached session on — the `Grid` needs them).
- **Decode buffer reuse**: `%output` decoding writes into one reused `Vec<u8>` per
  reader thread; no per-byte allocation, no per-line `String`.
- **Input batching**: one `send-keys -H` command per input burst (§6), not per byte.
- **ratatui diffing**: ratatui only repaints changed cells; the per-frame draw from
  the selected `Grid` is diffed against the prior buffer. Draw only on an event
  (key, `HostEvent`, resize, spinner tick) — not in a busy loop.
- **Bound = host count**: at most one child + reader + writer thread per host (~10
  hosts worst case = 10 ssh children), independent of session count.

## 8. Rendering details

### #1 — alt-screen enter+clear on start / restore on exit

On `run_cockpit` start, enter the alternate screen and clear before the first draw
(crossterm `EnterAlternateScreen` + `terminal.clear()`), bundled RAII with `RawGuard`
so exit (normal or panic) runs `LeaveAlternateScreen` + `disable_raw_mode` and
restores the user's pre-launch screen. This fixes the pre-launch shell output bleeding
under the Overlay UI.

### #2 — Overlay arrow / `jk` navigation

Overlay stdin bytes are decoded xmux-side by `proxy::decode::KeyDecoder` (already
maps CSI `ESC[A/B/C/D` → Up/Down/Right/Left and printable bytes → `Char`) and fed to
`switcher.handle_key`. `handle_key` already handles both `KeyCode::Up`/`Down` and
`Char('j')`/`Char('k')`-class keys via `move_selection`. The fix is wiring: ensure the
cockpit's Overlay branch routes raw stdin through `KeyDecoder` → `handle_key` (it does
today via `KeyDecoder::feed`), and that `j`/`k` are added to the navigation arms if
not already present. Verified headless by `key up` / `key down` over the ctl socket
moving the cursor.

### Terminal view (both states)

A Grid → ratatui bridge (`screen.rs` `Grid::render_into`, kept) maps each vt100 cell
(fg/bg/attrs/true-color) to a ratatui `Style` and places the cursor. In Overlay the
right region renders a top-left clip of the selected `Grid`; in Passthrough the whole
`cols×(rows-1)` area renders the `Grid` and ratatui draws the status bar on the last
row.

### #8 — help / footer rewritten for the new model

The footer/help reflects the new model exactly (no stale "Enter attach" / "Esc"):

- Overlay footer/help: `↑/↓ · j/k move (select = attach)`, `/ filter`, `R rename`,
  `x kill`, `r refresh`, `? help`, `C-g s fullscreen`, `q quit`. State that **moving
  the cursor attaches and previews**, and **Enter does nothing**.
- Passthrough status bar: `host/session` + active window/pane (info only); help is
  hidden. The keys that work in Passthrough (`C-g s` overlay, `C-g q` quit) are listed
  in the Overlay help, not the status bar.

## 9. Removed / superseded (AS-IS: no dead code)

**Removed** (the per-session PTY-proxy attach model — fully replaced by control mode):

- `proxy/run.rs`: `Attachment`, `spawn_attachment`, the per-Attachment ConPTY +
  output-pump + `pty_control_loop`/`MasterSink` control thread, `LiveOwner` (+ the
  `is_owner`/`set_owner`/`set_overlay` gate), `scan_clear` and the owner-pump
  status-bar repaint, `pump_write`, the `smoke_roundtrip`/`fake_attachment`/
  `DummyChild` test scaffolding for ConPTY attachments. `RawGuard` is **kept**
  (now paired with alt-screen, §8). `parse_prefix` is **kept** (sources the prefix
  from config).
- `proxy/registry.rs`: the whole `AttachRegistry` (`ensure`/`lru_victim`/`reap`/
  `resize_all`/`teardown_all`, the keep-cap/LRU/protect machinery). Replaced by the
  set of HostClients keyed by host (bound = host count, no cap/eviction).
- `proxy/app.rs`: `AppState::Passthrough { fg, fg_id }` + `Overlay` carrying the
  stdout-owner handoff, `App::enter_passthrough`/`enter_overlay`/`esc_target`/
  `prev_fg`, and the TOCTOU stdout-handoff logic. Replaced by a minimal two-variant
  state (`Overlay` / `Passthrough`) over the **shared selected session** with no
  stdout-owner concept.
- `cockpit.rs`: `attach_into_registry`, `handle_switcher_outcome` (Enter→promote,
  Esc→previous), `enter_passthrough`, `status_bar_bytes` (raw-byte status), the
  dwell-attach poll (`take_dwell_attach`), the `eof_tx`/`eof_rx` reap channel, and the
  `Cmd::SourceResult`/`Cmd::Panes` probe wiring in the cockpit loop. Replaced by the
  HostClient-driven loop.
- `switcher.rs`: the dwell mechanism (`DWELL`, `dwell_start`/`dwell_addr`,
  `rearm_dwell`/`dwell_candidate`/`dwell_pending`/`dwell_progress`/
  `take_dwell_attach`/`note_attached`, the left→right fill render block), the
  `result`/`chosen`/`on_enter`/`choose`/`take_esc`/`esc_requested` "Enter picks a
  session"/"Esc cancels" flow, and `attached: HashSet`. Replaced by: cursor-move →
  `select = attach` callback into the cockpit; spinner state per session.

**Kept** (load-bearing, behavior unchanged unless noted):

- `screen.rs` — the vt100 `Grid` + `Grid::render_into` + `restore_bytes` (restore is
  no longer used for raw handover but `render_into`/`resize`/`cursor` are core) +
  `vt_cell_style` color/attr bridge.
- `switcher.rs` — the tree model (`RowRef`, `rebuild`, ordering, filter), navigation
  (`move_selection`/`move_to`), rename/kill/create (`PendingOp`/`OpResult`/`run_op`),
  the streaming-apply methods (`apply_source_result`/`apply_panes`/`apply_op_result`)
  now fed from HostClient inventory deltas, `user_moved` latch, render of tree/input/
  footer/help (rewritten text, §8).
- `control.rs` + `ui/run.rs` `serve_control`/`dispatch`/`Cmd::Dump` — the `xmux ctl`
  socket, strengthened (§6).
- `attach::nest_guard` / `attach::in_mux` — xmux still refuses to run inside a mux.
- `config.rs` `[ui]` table (`prefix`, `keep_cap`) — `prefix` kept; `keep_cap` becomes
  **unused** (no per-session cap in the host-bound model). Decision: **remove
  `keep_cap`** from `UiConfig` (and its `default_keep_cap`/`keep_cap()`/clamp/tests),
  since AS-IS config must not advertise a knob with no effect. `[ui] prefix` stays.
- `mux.rs` builders/parsers (`SESSION_FORMAT`, `PANE_FORMAT`, `list_sessions`,
  `list_panes`, `parse_sessions`, `parse_panes`, `select_window`, `window_target`,
  `kill_session`, `rename_session`, `new_session`) — reused to build control-mode
  command lines and parse their `%begin…%end` bodies.
- `source.rs` — kept for `xmux ls`/`doctor`/`attach` (non-interactive) and for
  building host argv. The cockpit no longer calls `Source::attach_command` for an
  `ssh -t` attach; it builds the `-CC` child argv (a new small helper, §12). `source`
  is **not** used to open control connections via `EnvOps` probes in the cockpit.

**No self-mirror guard**: unreachable by construction — `nest_guard` refuses to start
xmux when inside a mux, and xmux only shows tmux/psmux sessions (whose panes export
`TMUX`), so a shown session's pane cannot be running xmux. (Carried over from the
prior spec; still valid since the control child is spawned with mux env cleared.)

## 10. Module impact map

- `src/cockpit.rs` — **modified (heavily)**. The event loop now: lazily creates
  HostClients; on cursor move (real or ctl) calls `select = attach` (ensure connect →
  `switch-client`); folds `HostEvent`s (inventory deltas, `%output`-driven redraw
  triggers, `%exit` reaps) into the `select!`; draws every frame from the selected
  HostClient's `grid`; toggles Overlay/Passthrough on `prefix s`; quits on `prefix q`
  / `q`. Removes `attach_into_registry`/`handle_switcher_outcome`/`enter_passthrough`/
  `status_bar_bytes`/dwell poll/eof channel/probe wiring (§9). Adds alt-screen
  enter/clear/restore (§8, #1).
- `src/host.rs` (or `src/proxy/host.rs`) — **created**. The `HostClient`,
  `HostInventory`, `HostCmd`, `HostEvent`, the reader-thread protocol parser
  (framing + notification dispatch + `%output` decode into a reused buffer), the
  writer-thread FIFO + command-correlation, and lazy spawn / teardown / reconnect.
  This is the heart of the re-architecture.
- `src/proxy/control_proto.rs` — **created** (or a submodule of `host.rs`). Pure,
  unit-testable functions: line classification (`%begin`/`%end`/`%error`/`%output`/
  `%extended-output`/notification), the `%output` octal-decode (`decode(&[u8]) ->
  &[u8]` into a reused buffer), `send-keys -H` line builder (bytes → batched hex
  command), `refresh-client -C` size formatter (`WxH`). No I/O — headless tests cover
  every wire detail from `[research §2–§8]`.
- `src/proxy/run.rs` — **modified (gutted)**. Remove `Attachment`/`spawn_attachment`/
  ConPTY pump/`pty_control_loop`/`MasterSink`/`LiveOwner`/`scan_clear`/`pump_write`
  and the ConPTY test scaffolding. Keep `RawGuard` (paired with alt-screen) and
  `parse_prefix`. (May shrink to just these two; consider relocating them to
  `cockpit.rs`/`config`-adjacent if `run.rs` would otherwise be near-empty.)
- `src/proxy/registry.rs` — **removed** (the file). `AttachRegistry` has no successor;
  the HostClient set lives in the cockpit / `host.rs`.
- `src/proxy/app.rs` — **modified**. Replace the stdout-owner `AppState` with a
  minimal `enum AppState { Overlay, Passthrough }` over the shared selected session
  (no `prev_fg`, no `LiveOwner`, no `enter_passthrough` byte-painting). May fold into
  `cockpit.rs` if trivial.
- `src/proxy/screen.rs` — **kept**. No change (the `Grid` bridge is reused as-is). The
  `restore_bytes` API may become test-only / unused — keep only if still called;
  remove if not (AS-IS).
- `src/proxy/input.rs` — **kept, lightly modified**. `InputMachine` prefix actions
  (`s` toggle, `q` quit, double-prefix literal, paste-aware) are reused for
  Passthrough. The `Forward(bytes)` output now feeds the `send-keys -H` builder
  (§6) instead of a PTY writer.
- `src/proxy/decode.rs` — **kept**. `KeyDecoder` drives Overlay navigation (#2);
  unchanged.
- `src/proxy/mod.rs` — **modified**. `pub mod registry;` removed; `pub mod host;` /
  `pub mod control_proto;` added.
- `src/ui/switcher.rs` — **modified**. Remove the dwell + Enter-picks/Esc-cancels flow
  and `attached` set (§9); add a `select = attach` hook the cockpit reads on cursor
  move; add per-session spinner state (frame index advanced by the tick) rendered
  right of the name; rewrite footer/help text (#8). Tree/nav/filter/rename/kill/
  ordering kept; `apply_*` methods fed from HostClient inventory deltas.
- `src/ui/run.rs` — **modified**. Keep `serve_control`/`dispatch`/`Cmd::Dump`/
  `dump_switcher` and the spinner animation tick (replaces the dwell-driven tick).
  Remove `Cmd::SourceResult`/`Cmd::Panes`/`spawn_probes`/`spawn_panes` from the
  *cockpit* path (the standalone `event_loop` used by `ls`-style tests may keep them,
  or they move there). The `event_loop` is no longer the cockpit's loop (the cockpit
  has its own loop in `cockpit.rs`); keep `event_loop` only if a non-cockpit caller
  still needs it, else remove (AS-IS).
- `src/source.rs` — **kept, lightly modified**. Add a `-CC` child-argv builder
  (local: `<bin> -CC attach …` with `-S <socket>` injection; remote: `ssh <host>
  <bin> -CC attach …`, **no `-t`**). Keep `list_sessions`/`run`/`exec_argv`/`quote`/
  `remote_command` for `ls`/`doctor`/`attach`. The interactive cockpit no longer uses
  `attach_command` (the `ssh -t` attach); `attach_command` stays for `xmux attach`.
- `src/env.rs` — **modified**. The cockpit builds HostClients from `env.srcs` /
  `env.by_alias` (host list + binaries). `EnvOps`/`ops()`/`spawn_probes` fan-out is no
  longer the cockpit's tree source; `EnvOps` stays for `ls`/`doctor`. Drop the
  `keep_cap` threading; keep `ui_prefix`.
- `src/main.rs` — **modified (small)**. Keep the `current_thread` runtime,
  `nest_guard`, and all subcommands (`ls`/`attach`/`doctor`/`ctl`/`version`). No
  `popup` subcommand exists to remove (already absent on this branch).
- `src/config.rs` — **modified**. Remove `keep_cap` from `UiConfig` (+ default +
  `keep_cap()` + clamp + its tests). Keep `prefix`.
- `src/control.rs` — **kept, lightly extended**. Add the optional `passthrough`/
  `overlay`/`keys` verbs if implemented (§6); existing wire protocol unchanged.

## 11. Risks & follow-ups

- **`send-keys` input fidelity limits** `[research §5]`:
  - *Mouse*: SGR mouse sequences from the user's terminal must be re-encoded and sent
    as bytes via `-H` (or `-M` inside a mouse binding). v1 may not forward mouse to
    the pane (Overlay mouse still drives the tree; Passthrough mouse forwarding is a
    follow-up).
  - *Bracketed paste*: there is no control-mode paste primitive; to preserve
    bracketed-paste semantics xmux must itself wrap pasted content in
    `ESC[200~ … ESC[201~` via `-H`. `InputMachine` already tracks paste framing, so the
    wrapper bytes can be forwarded faithfully; verify live.
  - *High/UTF-8 bytes*: `-H` is used for all bytes (the docs say "ASCII" for `-H`;
    empirically it emits the byte value `[research §5]`). If a multibyte burst
    misbehaves on psmux, fall back to `-l` for printable UTF-8 runs.
- **Remote ssh auth prompts**: `ssh <host> tmux -CC attach` with no `-t` runs over
  pipes; if the host needs interactive auth (password/2FA) the prompt has no tty and
  the connect will fail or hang. v1 assumes key-based / agent auth (as the existing
  remote probes do with `BatchMode`). A host needing a tty to even launch the login
  shell is the `ssh -tt` edge case `[research §1]` — out of scope for v1; surface the
  failure as "unreachable" with the stderr reason.
- **Reconnect on drop**: a dropped control child (network blip) must not wedge the UI;
  the lazy-reconnect-on-next-selection policy (§3) plus stale-while-revalidate
  inventory covers it. A flapping host could thrash connects — add a short reconnect
  backoff if observed (follow-up).
- **psmux/ConPTY `%output` byte fidelity** `[research §10, UNVERIFIED]`: the protocol
  is identical to upstream 3.3.6, but psmux's Windows pty backend's byte stream
  (CRLF, very high-throughput panes) should be smoke-tested live on jupiter06 — the
  decode is byte-exact, so any drift would be in what psmux emits, not in xmux's
  parser.
- **Watchdog discipline**: the `-CC` children and (especially) ssh children are
  non-self-bounding; teardown joins are bounded (§2), and any live verification that
  spawns them must wrap them in a hard OS-level watchdog (never idle-wait on a
  control child that may never EOF).

## 12. Testing strategy

- **Headless via `xmux ctl` + `dump`** on an **isolated psmux socket** (`-L <sock>`,
  `env -u PSMUX_SESSION -u TMUX` to dodge the nest guard), **jupiter06 only** — never
  the user's live local psmux server (Windows = one shared server). The terminal view
  is a `Grid` rendered to ratatui, so `dump` captures the **live** view: drive `key
  down` (select = attach), confirm the spinner appears then the session content
  renders in the dump, drive `key`/`text` to type into a session in Passthrough and
  confirm the pane echoes (via a follow-up `dump`).
- **Pure protocol unit tests** (`control_proto.rs`, no I/O, fully headless): `%begin/
  %end/%error` framing + command correlation; the `%output` octal decode against the
  exact cases in `[research §4]` (`\134`→`\`, `\012`→LF, `\015`→CR, `\000`→NUL,
  UTF-8 pass-through, split-across-reads); `%extended-output` `: ` stripping; the
  `send-keys -H` batched-line builder; the `refresh-client -C WxH` formatter; the full
  notification dispatch table.
- **HostClient lifecycle** with a fake child (a piped subprocess or an in-memory
  reader/writer pair replaying canned protocol bytes): connect handshake order,
  `switch-client` → `%session-changed` → first `%output` clears `connecting`,
  `%sessions-changed` re-list, `%exit` reap + reconnect, teardown bounded-join.
  Adversarially review the reader/writer thread concurrency.
- **Switcher** unit tests: tree build from inventory deltas, ordering/preselect,
  navigation (incl. `j`/`k` and arrows), filter, rename/kill, spinner state, footer/
  help text reflects the new model (no "Enter"/"Esc"/dwell strings).
- **Spike already verified** (live-review issue log): `-CC` framing,
  `%session-changed`, `%window-add`, `display-message` (`PANE=%2 SIZE=80x24`), `%exit`
  all behave per the research on Windows psmux over an isolated socket.
- **Human visual gate** (real terminal, not headless): the alt-screen
  enter/clear/restore (#1), the live fullscreen Passthrough render and status bar,
  and overall visual fidelity of the Grid → ratatui terminal view. Never select the
  live `local/xmux` session.

## 13. Open questions / decisions made (not pinned above)

- **`keep_cap` removal**: the pinned decisions keep config `[ui]` but the host-bound
  model has no per-session cap. Decision made here: **remove `keep_cap`** (unused knob
  must not persist, AS-IS). If a future need arises (e.g. a cap on concurrently-open
  *control connections*), reintroduce it with that meaning. Flag for review.
- **Local control child bootstrap**: when the local server has ≥1 session, spawn
  `psmux -CC attach` and `switch-client` to the target; when it has zero sessions,
  show "(empty)" and spawn nothing until a session exists. (Alternative: `-CC new -A
  -s <name>` to create-on-connect — rejected for the local case to avoid creating a
  stray session just to preview.) Flag for review.
- **`event_loop` in `ui/run.rs`**: it is the *standalone* picker loop today; the
  cockpit had its own loop. Decision: the cockpit keeps its own loop; `event_loop`
  is retained only if a non-cockpit caller (a test or a future standalone picker)
  needs it — otherwise removed with its probe fan-out (AS-IS). Confirm during the TDD
  plan which callers survive.
- **Overlay typing vs forwarding**: pinned that Overlay drives the tree and
  Passthrough forwards to the pane. An alternative (forward in Overlay too, with the
  tree reachable only via a sub-mode) was considered and rejected — Overlay is the
  navigator, Passthrough is the session. Flag if the user wants Overlay typing.
