# xmux Host-abstraction & death-detection refactor — HANDOFF (2026-06-25)

Branch `feat/rust-rewrite`. NOT pushed. Working tree clean at handoff time; last verified build
**389 tests pass, clippy 0** (2026-06-24, no code changed since — re-verify before you start).
Build with the REAL rustup toolchain (the `~/.cargo/bin` shim is blocked on this box, os error 448):
prepend `C:\Users\hrlee\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin` to PATH and set
`RUSTC`/`RUSTDOC`, or call the toolchain's `cargo.exe` directly. `cargo test` does NOT rebuild the
bin (stale-binary trap). `XMUX_DEBUG=1` writes `~/.xmux/debug.log`.

Background in auto-memory: [[xmux-rust-rewrite]], [[xmux-pty-attach-display-rearch-2026-06-20]],
[[xmux-detach-refresh-lifecycle-2026-06-24]], [[pty-grid-must-answer-terminal-queries]],
[[psmux-session-discovery]], [[rust-toolchain-rustup-shim-blocked-windows]].

## Why this handoff — the decision to make

The display layer works, but the "host" concept is **denormalized across 4 types tied only by an
alias string**, and the two things we now want — (a) correct psmux/tmux model handling and (b)
low-cost detection of a dead display — have no home to live in. This handoff captures the AS-IS
abstraction, the gaps, and a proposed direction so the next session can decide:

> **Introduce a single `Host`/`MuxServer` object that owns {model, connection, display
> attachment(s), display tty, inventory} — or graft the minimal death-detection onto the current
> scattered structure?**

Both are viable; the trade is documented in Part 3. Nothing below is committed yet.

---

## Part 1 — Current abstraction (AS-IS)

### `Session` — a single domain type (good)

`src/session.rs:10` — `Session { source, name, windows, attached, last_attached }`. Reused at every
layer. Address key `Session::address()` = `"<source>/<name>"` (`session.rs:23`), the cross-server
target. Satellites: `WindowPanes { index, name, active, panes }` and `Pane { index, active, command }`
(`session.rs:29-44`). A window has **no standalone type** — it is `WindowPanes` detail under a session,
referenced in the tree by `(session, window_index)`.

### "Host" — no single class; 4 scattered representations keyed by `source: String`

| Representation | Location | Owns |
|---|---|---|
| `Source` | `src/source.rs:95` `{ alias, binary, remote }` | connection/transport — argv, ssh, quoting, attach commands |
| `Group` | `src/ui/tree.rs:11` `{ source, err, sessions }` | tree model — one source's sessions, recency-sorted/filtered |
| `HostInventory` | `src/host.rs:17` `{ sessions, panes }` | live metadata filled by the `-CC` control client |
| `RowRef::Host` | `src/ui/switcher.rs:266` `{ source, unreachable }` | the rendered tree row |

There is no object that holds a host's connection AND its display attachment AND its display tty AND
its liveness together. The common thread is the `source`/alias string.

### Tree-render layer

- `RowRef` (`switcher.rs:265`): `Host{source,unreachable} | Session(Session) | Window{sess,window:i64}
  | Pane | Loading`. Only Host/Session/Window are selectable.
- `Row` (`switcher.rs:402`): a rendered line (text + `RowRef`). `Switcher` (`switcher.rs:460`) owns
  rows + cursor + filter + menu/popup state.
- `Selection` (`src/cockpit.rs:174`): `{ source, session, window: Option<i64> }` — the committed
  cursor target; the bridge from tree cursor to which PTY to attach. `Selection::address()` =
  `source/session`.

### Data flow

```
Config → Source(connection) → [ -CC HostInventory | psmux registry enumerate ] → Vec<Session>
       → Group(per source)  → Vec<Row>/RowRef(flattened) → Selection(cursor target)
       → AttachRegistry(key: source/session; REMOTE keyed by host) → PTY + vt100 Grid
```

`display_key()` (cockpit.rs) picks the registry key: **remote → `source` (one PTY per host),
local → `source/session` (one PTY per session)**. `host_session: HashMap<source, session>` tracks
which session each remote host's single PTY is currently switched to.

---

## Part 2 — Gaps that motivate the refactor

### G1 — mux MODEL is discriminated by `remote`, not by mux kind (latent bug)

The model signal exists — `Source::is_local_psmux()` = `!remote && binary == "psmux"`
(`source.rs:289`) — and **enumeration uses it correctly** (`source.rs:314`: psmux → registry path,
else `list-sessions`).

But the **PTY-attach model** (per-session vs per-host) keys on `src.remote` ALONE:
- `sync_source_terminals` (`cockpit.rs:401` `if src.remote` → per-host warm; `cockpit.rs:420` else →
  per-session loop).
- `select_attach` (`cockpit.rs:312` remote → switch-client; else → per-session attach).

This is only correct under the implicit assumption **local==psmux, remote==tmux**:
- **local tmux** (`binary=="tmux"`, `remote==false`) wrongly takes the per-session-PTY branch (tmux is
  one-server-many-sessions; one PTY + `switch-client` would suffice).
- **remote psmux** (`remote==true`) wrongly takes the per-host `switch-client` branch — psmux is
  one-server-per-session, so `switch-client` cannot cross its per-session servers → broken.

The real discriminator is the **server model** (one-server-per-session vs one-server-many-sessions),
which maps to psmux vs tmux, not to local vs remote.

### G2 — no unified Host object

Because the host is 4 scattered pieces, there is nowhere to attach per-host invariants (its model, its
display tty, its liveness, its single display PTY's current session). `host_session` is a side map;
the tty lives in a remote file (G3); liveness lives nowhere (G4).

### G3 — display client tty is not in memory

The attach command writes our display client's tty to the remote file `/tmp/.xmux-cli-<alias>` via
`tty >FILE` (`source.rs:184`, path at `source.rs:282`), and `switch_client_remote_cmd` reads it back
in a remote shell snippet (`source.rs:271`) to target `switch-client -c "$tty"`. **xmux never reads
that value into its own memory.** Any "is OUR display client alive/dead" logic needs that tty to
filter against — so capturing it once is the enabling primitive for G4.

### G4 — death detection is PTY-EOF only; the better push signal is discarded

Current death detection is **passive inference from the local PTY**: the pump (`src/proxy/run.rs`)
reads the master; `Ok(0)` (EOF) → `PtyEvent::Exited` → `registry.reap` (cockpit `pty_rx` arm
`cockpit.rs:1263-1308`). There is NO heartbeat.

- Catches: the local attach child exiting (session killed, detach where ssh exits, server down). For
  **psmux this is clean** — each session has its own attach, so a dead session EOFs its own PTY.
- Misses: the remote **no-EOF stuck case** — the display client is gone on the remote but the local
  `ssh -t` lingers → no EOF → xmux still believes the PTY is live → `switch-client` lands on a dead
  tty → blank pane. The manual workaround is the `r` re-attach (`cockpit.rs:1133`,
  `take_reattach_kick`), which tears down + rearms unconditionally.

The precise push signal for this gap already arrives and is **thrown away**: tmux pushes
`%client-detached <client>` to every control client the instant ANY client detaches — including our
display client. But:
- the parser drops the client argument (`src/proxy/control_proto.rs:144`:
  `"%client-detached" => Notif::ClientDetached`, payload-less),
- and `dispatch_notif` ignores it (`host.rs:266-272` `Notif::ClientDetached => {}`) because it is
  GLOBAL — blindly reaping on it killed whole hosts (the reason it was made inert).

### G5 — local keeps N warm PTYs

The per-session **count** is forced by psmux (can't `switch-client` across per-session servers), but
keeping all of them **warm** is a choice for instant switching (`cockpit.rs:420-431` attaches every
session). One local PTY + re-attach on switch is possible; trade = attach/render latency + a blank
flash per switch + no pre-warmed background grids.

---

## Part 3 — Proposed direction (decide, don't assume)

### D1 — model discrimination by server model, not `remote`

Replace the `src.remote` branch in `sync_source_terminals`/`select_attach` with a model predicate
(e.g. `Source::one_server_per_session()` ≈ `binary == "psmux"`). Per-session-server model →
PTY-per-session; one-server-many-sessions model → one PTY + `switch-client`. This fixes local tmux and
remote psmux and makes the asymmetry explicit instead of implicit. Smallest correct version: a single
predicate method + swap the two branch conditions; tests in `source.rs:698-703` already pin
`is_local_psmux` semantics.

### D2 — death as a PUSH, not a poll (cheaper AND faster)

"Alive" must be polled; "dead" can be pushed. Speed ranking of liveness signals:
1. **psmux**: `~/.psmux/<name>.port` file stat — existence = alive (`psmux-session-discovery`). Pure
   filesystem, microseconds, already the enumeration substrate. Plus the per-session PTY EOF already
   pushes death. psmux needs nothing new.
2. **tmux**: a one-line query on the already-open `-CC` connection (`HostClient::Query`, pattern at
   `host.rs:496` `probe_active_window`) — one round-trip, ms. No new ssh.
3. **(avoid)** a fresh `ssh … tmux has-session` — new TCP+auth handshake, 100ms–seconds.

The recommended fix for the tmux gap is event-driven, beating any poll:
- **Capture the display tty into memory** (G3) — read `/tmp/.xmux-cli-<alias>` once after a successful
  attach (one `run_raw`), store it on the host.
- **Parameterize + un-inert `%client-detached`**: carry the client name
  (`control_proto.rs:144` → `Notif::ClientDetached { client }`), and in `dispatch_notif` emit a reap
  event ONLY when `client` matches our stored display tty — never the blanket reap that caused
  69314b2. Match → reap the display attach + rearm attach (reuse the `r` path at `cockpit.rs:1133`).

This is zero added cost (already-streaming notification), fires at the instant of death, and stays
host-scoped because it filters on our own tty.

### D3 — (optional) the original ask: a pre-switch liveness gate

If a proactive gate is still wanted on top of D2: before painting the mux on a switch, run the
low-cost check (psmux → port-file stat; tmux → `-CC` `list-clients` matched to our tty) and, if dead,
re-attach instead of `switch-client`. With D2 in place this is largely redundant (the push already
reaps the dead display), so prefer D2 first and add D3 only if a residual race shows up live.

### D4 — (optional) collapse local to one PTY + re-attach

Only if N warm PTYs prove costly. Trades instant-switch for fewer processes (see G5). Not recommended
unless measured.

### Sequencing recommendation

D2 (death-push) is the highest value / lowest risk and is independent of the big refactor — it can
land on the current structure with just a stored tty + a parameterized notification. D1 is a small,
well-tested correctness fix. The full `Host` object (D1+D2+G2 unified) is the clean end state but is a
larger change; do it only if you're also touching `host_session`/`AttachRegistry` ownership anyway.

---

## Part 4 — File:line navigation index

- Domain types: `session.rs:10` Session, `:29` Pane, `:38` WindowPanes, `:23` address.
- Source/transport: `source.rs:95` Source, `:184` tty-capture write, `:271` switch_client_remote_cmd,
  `:282` client_tty_path, `:289` is_local_psmux, `:314` enumeration branch.
- Control client: `host.rs:17` HostInventory, `:38` HostCmd, `:50` HostEvent, `:83` PendingReply,
  `:188` resolve_block, `:228` dispatch_notif (`:266-272` ClientDetached inert), `:480` list_panes,
  `:496` probe_active_window (the Query→reply pattern to copy for a liveness query).
- Protocol: `proxy/control_proto.rs:31` Notif enum, `:110` parse_notif, `:144` `%client-detached`
  (drops arg).
- PTY/death: `proxy/run.rs` pump (EOF→Exited), `proxy/registry.rs` remove/reap/contains.
- Cockpit: `cockpit.rs:174` Selection, `:289` select_attach (`:312` remote branch), `:384`
  sync_source_terminals (`:401` remote / `:420` local), `:1133` `r` reattach consume,
  `:1263-1308` pty_rx Exited/detach handling, `:1157-1192` debounced attach apply.
- Tree/UI: `ui/tree.rs:11` Group, `ui/switcher.rs:265` RowRef, `:402` Row, `:460` Switcher,
  `:626` take_reattach_kick, `:1681` reattach_kick set.
- Env: `env.rs:25` Env.

---

## Part 5 — Constraints & gotchas

- **AS-IS principle**: this repo's files state current behavior as fact — no "was X, now Y" / history
  narration in code or docs. Git holds the deltas. This handoff is a design doc (forward-looking
  proposals are fine); do not leave delta-narration in the code you write.
- **Verification is a human live gate.** Attach/switch/detach behavior is NOT cleanly reproducible
  headless (the harness would nest the cockpit; local psmux is one shared server — a stray attach can
  hijack the user's live session). Tests and LSP have repeatedly reported green while the live path was
  broken — verify committed behavior from a real build + the user's eyes, not from a passing suite.
  Remote attach IS testable headless via `ssh -t`; drive throwaway-only `jupiter06` first, never
  picker-select near the user's live local/`if` session.
- **Do NOT disrupt the user's live work session** when probing (no attach/switch-client to `if` from
  test probes). Read-only `-CC` queries / `display-message` against a throwaway are safe.
- **The `r` manual re-attach already exists** (`cockpit.rs:1133`) — D2 should reuse its teardown+rearm,
  not duplicate it.
- **psmux lies about its version** (reports tmux 3.3.6) — discriminate by configured `binary` name,
  not by a runtime version probe. psmux config loads from `~/.tmux.conf` (see
  [[psmux-windows-config-loading]]).
- **Git policy**: commit autonomously; ask ONLY before pushing, then proceed
  ([[commit-autonomously-ask-only-push]]). Work is on `feat/rust-rewrite`, NOT pushed.
- **No memory writes from a worktree** — if you isolate this in a worktree, write auto-memory only to
  the main project's absolute memory path.
