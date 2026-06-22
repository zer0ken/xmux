# xmux PTY-attach re-architecture — HANDOFF (2026-06-20)

Branch `feat/rust-rewrite`. NOT committed (all changes in the working tree). Build with the
real rustup toolchain (the shim is blocked): use the helper
`C:\Users\hrlee\AppData\Local\Temp\claude\C--Projects-xmux\c2e3eef7-1a78-4b06-93b1-26d100d13d40\scratchpad\cg.sh`
(`bash cg.sh test` / `bash cg.sh clippy --all-targets`), or prepend
`C:\Users\hrlee\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin` to PATH + set RUSTC/RUSTDOC.
Current state: **255 tests pass, clippy 0 warnings, builds clean.** Full background in auto-memory
[[xmux-pty-attach-display-rearch-2026-06-20]], [[pty-grid-must-answer-terminal-queries]].

## What is DONE (verified)
- **Display = real per-session/per-host PTY-attached mux clients** (`src/proxy/run.rs` Attachment/
  spawn_attachment + `src/proxy/registry.rs` AttachRegistry), rendered into a vt100 `Grid` by
  ratatui. Control-mode `-CC` per remote host = metadata/events/select-window only (`refresh-client
  -f no-output`). Local psmux = poll (`spawn_local_enumeration`).
- **CRITICAL DSR fix**: the pump answers the child's terminal queries (`ESC[6n` DSR, `ESC[c`/`ESC[0c`
  DA) via `query_responses()`, else the child stalls and the pane is BLANK. VERIFIED: `xmux ctl dump`
  showed jupiter00's real attached tmux (output + status bar). ConPTY works here — do NOT re-blame
  the environment for a blank pane.
- **Per-HOST PTY for remote tmux** (one PTY per host, `switch-client` between its sessions);
  per-SESSION for local psmux (one-server-per-session). `display_key()` in cockpit.rs picks the key
  (remote→source, local→source/session). Switch uses `Source::switch_client_remote_cmd()` (a one-shot
  ssh that `switch-client -c <non-control-client-tty> -t <session>`, identifying the PTY client as the
  one whose `client_flags` lack `control-mode`). `host_session` map tracks each host PTY's session.
  Per-host KEYING + switch DISPATCH confirmed in debug.log (`select_attach key=jupiter00 ... sess=...`);
  the rendered content-switch was NOT live-verified (test navigated to the wrong host) — VERIFY IT.
- **UI/nav**: `(active)` text → BOLD active window/pane row (`Row::active`); `●` attached-marker
  removed (every session is attached in the PTY model → noise); `→`=descend to child / `←`=ascend to
  parent; `Ctrl+↑/↓`=move between SIBLINGS at the current level; prefix+arrow removed.
- Codex reviewed the design + 2 rounds + the DSR fix; all findings applied.

## REMAINING (user-reported 2026-06-20) — your tasks
1. **Local sessions don't show.** Root cause: psmux is one-server-per-session (Windows named pipes,
   socket name = session name); `psmux list-sessions` on the DEFAULT socket returns EMPTY (verified
   in native PowerShell + bash). xmux's local enumeration (`spawn_local_enumeration` →
   `Source::list_sessions` → `psmux list-sessions`) therefore finds nothing. Need a way to enumerate
   ALL local psmux per-session servers. NOTE: this Claude Code Bash/PowerShell context has a STALE
   `$TMUX`/`$PSMUX_SESSION` (server unreachable — `psmux display-message` says "no server"), so you
   CANNOT reproduce the user's live local state from here; investigate psmux's session-discovery
   mechanism (socket-dir scan? a psmux aggregate command? ask how the user lists sessions) and test
   carefully. SAFETY: never attach the user's LIVE local psmux during testing — use a temp
   `~/.config/xmux/config.toml` with `[local] mux = "cmd"` (inert) + only the throwaway remote
   `jupiter06`, and DELETE it after.
2. **Sidebar selection doesn't follow an external window change.** When the user moves windows
   INSIDE the mux (e.g. prefix-n) so the displayed tmux's active window changes, the sidebar cursor
   should follow to the new active window's row. This was REMOVED in the simplify (the
   `%session-window-changed` → probe → Focus → `Switcher::select_window` chain was deemed vestigial).
   `Switcher::select_window(source, session, window)` STILL EXISTS (kept). RE-WIRE: currently
   `%session-window-changed` → `HostEvent::Changed` (blanket refetch). Instead, carry the changed
   session + its new active window and call `switcher.select_window(host, session, window)`. CHALLENGE
   (documented): `%session-window-changed $id @win` gives a session ID, not a name → add
   `#{session_id}` to `mux::SESSION_FORMAT` and keep an id→name map in `HostInventory`, OR probe the
   changed session's active window via `display-message`. The per-host model complicates "which
   session is displayed" — only react when the changed session is the host's currently-displayed one
   (`host_session[host]`).
3. **Finish per-host live verification**: launch xmux (fresh console via `Start-Process -WindowStyle
   Hidden`, with `$env:TMUX=''` so nest_guard passes, `$env:XMUX_DEBUG='1'`), create ≥2 sessions on
   jupiter06 with distinct markers, navigate BETWEEN jupiter06's sessions (not jupiter00) via
   `xmux ctl --pid <pid> key ...` + `xmux ctl dump`, and confirm the ONE host PTY's right-pane content
   switches between the two sessions. `XMUX_DEBUG=1` writes `~/.xmux/debug.log`.

## Gotchas
- `dbg_log()` in cockpit.rs is gated by `XMUX_DEBUG` (no-op otherwise) — keep it; it's the diagnostic.
- A stray `xmux.exe` from a test run locks `target/debug/xmux.exe` → build fails "failed to remove";
  `powershell.exe -NoProfile -Command "Stop-Process -Name xmux -Force"` first.
- I sent `clear; echo …` keystrokes to the user's jupiter06 session "0" during testing (benign, just
  cleared their screen) — mention it; don't do that to live sessions.
- Codex review available: `codex exec --skip-git-repo-check` (gpt-5.5) works for design/review.
