# Task 3 Report: Display-lifecycle observability events

## Events added

### 1. `Grid::fingerprint` — `src/proxy/screen.rs`
Implemented as a `DefaultHasher` over `self.parser.screen().contents()`. Uses `std::hash::{Hash, Hasher}` and `std::collections::hash_map::DefaultHasher`. One lock-free read of the parser (no `Mutex` involved since the grid is `&self`); no allocation beyond the `contents()` string.

### 2. `display_show` (INFO) — `src/driver.rs`
Added to both `TmuxDriver::show` and `PsmuxDriver::show` at every decision branch:

**TmuxDriver (model=shared):**
- First-attach path (no live attachment, not in-flight): `decision=reattach, reason=no-live-client`
- Session switch path (live but different session): `decision=switch, reason=live+tty`
- Already-on path (new `else` branch added): `decision=warm, reason=already-on`

**PsmuxDriver (model=per-session):**
- Already-on with live+tty: `decision=warm, reason=already-on`
- In-place switch (live+tty, different session): `decision=switch, reason=live+tty`
- Reattach path: `decision=reattach, reason=no-live-client` (no live attachment) or `reason=no-tty` (live but tty not captured)

### 3. `attach_created` (INFO) — `src/driver.rs`
Emitted immediately after each `request_attach` call returns an `id`, in both drivers' reattach paths. Fields: `addr` (the display key), `id` (attachment id), `count` = `ctx.registry.len()` at that moment. Note: `request_attach` was changed from returning `u64` already (it did); the TmuxDriver first-attach path now binds the return value to emit this event.

### 4. `tty_probe` (DEBUG) — `src/driver.rs`, `spawn_local_psmux_tty_capture`
Added inside the `for attempt in 0..5` loop. Emits on every attempt, including failed `run()` calls. Fields: `addr` (derived as `local/<session>`), `attempt`, `result` (the parsed tty string, or `"none"`).

### 5. `display_inventory` (DEBUG) — `src/driver.rs`
Emitted from every exit path of both drivers' `show` methods after the decision executes. Fields: `count` = registry len, `attached` = comma-separated `addr=session` pairs from `ctx.registry.addresses()` joined with `host.display.shows(&addr)`, `displayed` = `sel.session`, `mismatch` = whether `host.display.shows(&key) != Some(&sel.session)`.

### 6. `display_grid_changed` (INFO) — `src/cockpit.rs`
A `HashMap<String, u64>` named `grid_fingerprints` is declared before the event loop. In the draw path (the `dirty && elapsed >= FRAME_MS` block), after acquiring the grid lock and before calling `term.draw`, the grid's `fingerprint()` is computed under the existing lock and compared to the stored value for the display key. If different (or absent), the event is emitted and the map is updated. The map comparison guarantees the event fires only on content change, never per-frame.

## Where the per-key fingerprint map lives
Declared as `let mut grid_fingerprints: HashMap<String, u64> = HashMap::new();` immediately before `let spinner_start` in `run_cockpit`, inside the function's local scope. It is loop-local state — not part of any struct — consistent with the cockpit's other per-run accumulations (`grid_fingerprints`, `connected`, `panes_requested`, etc.).

## Unit tests for `Grid::fingerprint`
Two tests added to `src/proxy/screen.rs`:
- `fingerprint_same_contents_same_hash`: two grids fed the same bytes produce the same fingerprint.
- `fingerprint_different_contents_different_hash`: a grid cleared and fed different bytes produces a different fingerprint.

## Commands and output
```
cargo build        → Finished `dev` profile [unoptimized + debuginfo] in 3.67s
cargo test         → 556 passed; 0 failed; 6 ignored (2 new fingerprint tests included)
cargo clippy --all-targets → Finished (0 warnings)
cargo fmt --check  → clean (one formatting fix applied after initial check)
```

## Self-review
- `display_inventory`'s `attached` field uses `host.display.shows(&addr)` where `addr` iterates over `ctx.registry.addresses()`. Since the registry keys are host-id strings (not source/session addresses for psmux), and `host.display.shows` takes a key that is also the host-id, the lookup is correct for both models.
- The TmuxDriver `show` originally had no `else` branch after the if/else if. Adding one to emit `warm/already-on` is a pure addition that changes no behavior.
- `tty_probe` uses `local/<session>` as the `addr` rather than the actual registry key, because the capture task does not have access to the registry key at that point (it receives only `bin`, `session`, `id`, and `pty_tx`). This is a deliberate approximation — the event is still unambiguous for a local psmux host.
- `display_grid_changed` is emitted under the grid lock, before `term.draw`. This is correct: the same lock guard is used for rendering immediately after, so no double-lock, and the fingerprint reflects the state that will be drawn.

## Concerns
None blocking. The `attached` field in `display_inventory` joins registry addresses with `host.display.shows`, which may show `"?"` for addresses that happen to not match a known key — cosmetic only, not incorrect.

---

## Review fix: I-1 + I-2 in `display_inventory`

### I-1: `mismatch` was vacuously `false` after `set_shows`

**Root cause.** Every exit path of both drivers' `show` computed `mismatch` AFTER calling `host.display.set_shows(&key, &sel.session)`, which had just written `sel.session` — making the comparison trivially false.

**Fix (`src/driver.rs`).** In `TmuxDriver::show`, one line was added immediately after `let key = host_selection_key(host)`:
```rust
let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());
```
In `PsmuxDriver::show`, line 308 already computed `already_on` (the inverse of the desired mismatch) before any mutations, so:
```rust
let pre_mismatch = !already_on;
```
All `display_inventory` log sites in both drivers now use `mismatch = pre_mismatch` instead of re-evaluating after bookkeeping.

**Borrow avoidance.** `host.display.shows(&key)` returns an `Option<&str>` (a borrow into `host`). Comparing it to `Some(sel.session.as_str())` yields a `bool`. The bool is captured as `pre_mismatch: bool` — fully owned, no lifetime dependency on `host` — before any `&mut host.display` mutation. No conflict.

### I-2: `attached` showed `?` for cross-host addresses

**Root cause.** The inventory block iterated `ctx.registry.addresses()` (global, all hosts) but resolved each address via `host.display.shows(&addr)` — the current host's bookkeeping only. Any address belonging to another host returned `None` → `?`.

**Fix (`src/driver.rs`).** The `attached` map in every `display_inventory` block now extracts the owning host id from the address:
```rust
let host_id = addr.split_once('/').map_or(addr.as_str(), |(h, _)| h);
let shown = ctx.hosts.get(host_id).and_then(|h| h.display.shows(&addr)).unwrap_or("?");
```
This is the same extraction used by `cockpit::host_of_key` (inlined to avoid making the private function pub).

**Borrow avoidance.** After the fix, no inventory block uses `host` (the `&mut Host` reborrow of `ctx.hosts`). In both `TmuxDriver` and `PsmuxDriver`'s switch/reattach arms the last use of `host` is already `lower_select_window(host, ...)` before the inventory block — NLL frees the reborrow and `ctx.hosts.get(host_id)` compiles. In `PsmuxDriver`'s warm arm, the `lower_select_window` call was moved to just before the inventory block (same semantics — select-window is independent of the log) so that `host`'s last use precedes the inventory block there too.

### Commands and output
```
cargo build        → Finished `dev` profile [unoptimized + debuginfo] in 3.05s
cargo test         → 556 passed; 0 failed; 6 ignored
cargo clippy --all-targets → Finished (0 warnings)
cargo fmt --check  → clean (no output)
```
