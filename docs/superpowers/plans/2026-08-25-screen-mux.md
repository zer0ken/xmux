# GNU Screen Mux Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add GNU Screen as a first-class `Mux` family so xmux can enumerate, switch to, and create screen sessions on any host that has `screen` (issue #100).

**Architecture:** Add a `screen` Mux family mirroring the zellij family's shape — a poll-based metadata path (screen has no control-mode channel), a `PerSession` display fan-out (no in-place `switch-client`, so every session change reattaches), and death by attachment EOF. Screen shares no argv with tmux, so it overrides every command plan and names its own `-ls` / `-Q windows` parsers. A new `-v` identity probe distinguishes screen (which prints `Screen version … (GNU)`) from tmux (`-V`), and a screen-specific `enumerate` treats `screen -ls` exit code 1 (stdout `No Sockets found`) as an empty-but-reachable mux.

**Tech Stack:** Rust (existing `async_trait`, `tokio`), reusing the `Mux` trait and the `ServerModel`/`DeathSignal`/`EventSource` leaf types. No new dependencies.

**Live verification target:** `jupiter00` (ssh) — Ubuntu with `/usr/bin/screen` already installed. WSL verification skipped by user request.

---

### Task 1: screen vocab — argv builders + parsers

**Files:**
- Create: `src/mux/screen/vocab.rs`
- Test: `src/mux/screen/vocab.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Pane, Session, WindowPanes};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_sessions_is_dash_ls() {
        assert_eq!(list_sessions("screen"), argv(&["screen", "-ls"]));
    }

    #[test]
    fn windows_query_is_s_q_windows() {
        assert_eq!(
            windows("screen", "my sess"),
            argv(&["screen", "-S", "my sess", "-Q", "windows"])
        );
    }

    #[test]
    fn attach_is_multi_display_dash_x() {
        assert_eq!(attach("screen", "api"), argv(&["screen", "-x", "api"]));
    }

    #[test]
    fn new_session_is_detached_dmS() {
        assert_eq!(
            new_session("screen", "dev"),
            argv(&["screen", "-dmS", "dev"])
        );
    }

    #[test]
    fn select_window_sends_select_via_dash_x() {
        assert_eq!(
            select_window("screen", "dev", 2),
            argv(&["screen", "-S", "dev", "-X", "select", "2"])
        );
    }

    #[test]
    fn parse_sessions_reads_the_ls_listing() {
        let out = concat!(
            "There are screens on:\r\n",
            "\t2589.parsetest\t(08/25/2026 11:05:05 PM)\t(Detached)\r\n",
            "\t4190.alpha\t(08/25/2026 11:00:07 PM)\t(Attached)\r\n",
            "3 Sockets in /run/screen/S-hrlee.\r\n",
        );
        let got = parse_sessions("jup", "screen", out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "parsetest");
        assert!(!got[0].attached);
        assert_eq!(got[1].name, "alpha");
        assert!(got[1].attached);
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "screen"));
    }

    #[test]
    fn parse_sessions_skips_banners_and_footers() {
        // Header/footer lines carry no socket id; a tab-less or non-numeric-pid line is skipped.
        let out = concat!(
            "There is a screen on:\n",
            "\t1.work\t(08/25/2026 11:00:00 AM)\t(Detached)\n",
            "1 Socket in /run/screen/S-hrlee.\n",
        );
        let got = parse_sessions("local", "screen", out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "work");
    }

    #[test]
    fn parse_sessions_name_keeps_dots_and_spaces() {
        // Socket id is `pid.<name>`; the name is everything after the FIRST dot, so
        // both a dotted and a spaced name survive.
        let out = "There are screens on:\n\t1.foo.bar\t(08/25/2026 11:00:00 AM)\t(Detached)\n\t2.my sess\t(08/25/2026 11:00:00 AM)\t(Detached)\n";
        let got = parse_sessions("local", "screen", out);
        assert_eq!(got[0].name, "foo.bar");
        assert_eq!(got[1].name, "my sess");
    }

    #[test]
    fn parse_sessions_empty() {
        assert!(parse_sessions("local", "screen", "").is_empty());
    }

    #[test]
    fn parse_windows_reads_the_space_joined_list() {
        let out = "0 bash  1 bash  2 vim";
        let got = parse_windows(out);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].index, 0);
        assert_eq!(got[0].name, "bash");
        assert_eq!(got[2].index, 2);
        assert_eq!(got[2].name, "vim");
    }

    #[test]
    fn parse_windows_keeps_spaces_in_a_name() {
        // A title with spaces: `0 my title 1 bash`. The integer tokens delimit windows.
        let out = "0 my title 1 bash";
        let got = parse_windows(out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "my title");
        assert_eq!(got[1].name, "bash");
    }

    #[test]
    fn parse_windows_empty() {
        assert!(parse_windows("").is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib screen::vocab 2>&1 | tail -20`
Expected: compile error — `screen::vocab` module does not exist.

- [ ] **Step 3: Write the minimal implementation**

```rust
//! GNU screen argv builders and parsers. Screen shares no argv with tmux, so these
//! are screen-native: `-ls` lists sessions, `-S <name> -Q windows` lists a session's
//! windows (the `-Q` reply comes back on stdout), `-x` attaches in multi-display mode,
//! and `-dmS` starts a detached session. Parsers are pure over the raw output.

use crate::session::{Pane, Session, WindowPanes};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `screen -ls` — lists this user's screen sessions. Exits 0 with sockets present,
/// 1 (stdout `No Sockets found`) when empty.
pub fn list_sessions(bin: &str) -> Vec<String> {
    argv(&[bin, "-ls"])
}

/// `screen -S <name> -Q windows` — prints the session's windows to stdout as a
/// space-joined `num name` list. The `-Q` flag makes screen write the reply to the
/// querying process's stdout, which is what makes headless window detail possible.
pub fn windows(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "-S", name, "-Q", "windows"])
}

/// `screen -x <name>` — attach in multi-display mode. Unlike `-r` (detached-only) it
/// attaches whether the session is detached or already attached elsewhere, which is
/// the attach a switcher needs: xmux adds its own display client without kicking one.
pub fn attach(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "-x", name])
}

/// `screen -dmS <name>` — start a DETACHED session. Prints nothing, so `manage::create`
/// keeps the requested name.
pub fn new_session(bin: &str, name: &str) -> Vec<String> {
    if name.is_empty() {
        argv(&[bin, "-dmS"])
    } else {
        argv(&[bin, "-dmS", name])
    }
}

/// `screen -S <name> -X select <index>` — makes window `index` active server-side
/// (all attached displays follow).
pub fn select_window(bin: &str, name: &str, index: i64) -> Vec<String> {
    argv(&[bin, "-S", name, "-X", "select", &index.to_string()])
}

/// Parses `screen -ls` output into sessions tagged with `source`/`mux`. Each socket
/// line is `\t<pid>.<name>\t(<date> <time> <ampm>)\t(<state>)`; the name is everything
/// after the first dot, and `attached` is read from the state column. Lines that carry
/// no socket id (header/footer) or a non-numeric pid are skipped so banners cannot
/// poison the list. `windows`/`last_attached` are unknown from `-ls`, so they are 0.
pub fn parse_sessions(source: &str, mux: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in out.split('\n') {
        let ln = ln.strip_suffix('\r').unwrap_or(ln);
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let id = fields[1];
        let Some((pid, name)) = id.split_once('.') else {
            continue;
        };
        if pid.is_empty() || name.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let state = fields[3].to_lowercase();
        let attached = state.contains("attached") && !state.contains("detached");
        sessions.push(Session {
            source: source.to_string(),
            name: name.to_string(),
            mux: mux.to_string(),
            windows: 0,
            attached,
            last_attached: 0,
        });
    }
    sessions
}

/// Parses `screen -S <name> -Q windows` output (`0 bash  1 bash  2 vim`) into
/// windows. Screen prints a space-joined `num name` list with no active marker, so
/// each window is one [`WindowPanes`] with a single placeholder pane, `active` false
/// (the nav falls back to the first window). Integer tokens delimit windows; any
/// other token appends to the current window's name, so a name with spaces survives.
pub fn parse_windows(out: &str) -> Vec<WindowPanes> {
    let mut windows: Vec<WindowPanes> = Vec::new();
    let mut cur: Option<(i64, Vec<String>)> = None;
    for tok in out.split_whitespace() {
        if let Ok(idx) = tok.parse::<i64>() {
            if let Some((index, name_tokens)) = cur.take() {
                windows.push(finish_window(index, name_tokens));
            }
            cur = Some((idx, Vec::new()));
        } else if let Some((_, name_tokens)) = cur.as_mut() {
            name_tokens.push(tok.to_string());
        }
    }
    if let Some((index, name_tokens)) = cur {
        windows.push(finish_window(index, name_tokens));
    }
    windows
}

fn finish_window(index: i64, name_tokens: Vec<String>) -> WindowPanes {
    let name = name_tokens.join(" ");
    WindowPanes {
        index,
        name: name.clone(),
        active: false,
        panes: vec![Pane {
            index: 0,
            active: false,
            command: name,
        }],
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib screen::vocab 2>&1 | tail -20`
Expected: PASS (all new tests green).

- [ ] **Step 5: Commit**

```bash
git add src/mux/screen/vocab.rs
git commit -m "feat(screen): screen vocab — -ls/-Q windows parsers and argv builders"
```

---

### Task 2: Screen Mux impl + display driver

**Files:**
- Create: `src/mux/screen/mod.rs`
- Create: `src/mux/screen/display.rs`
- Create: `src/mux/screen/AGENTS.md`
- Modify: `src/mux/mod.rs` (register the family + `-v` detection probe + update tests)

- [ ] **Step 1: Write the failing test** — add `src/mux/screen/mod.rs` with the impl and inline tests (the `Mux` impl compiles only once the family is registered; the registration test in `mod.rs` fails first).

- [ ] **Step 2: Write the screen impl** — `src/mux/screen/mod.rs`:

```rust
//! screen: one per-user daemon holding every session, no control-mode channel, every
//! query a separate `screen` process. There is no `switch-client`, so the display
//! reattaches on every session change (the zellij shape).

use super::*;

pub mod display;
mod vocab;

pub use display::ScreenDriver;

/// The screen poll cadence. screen pushes no change events, so the session list is
/// discovered by re-enumeration; one sweep costs one `-ls` plus one `-Q windows` per
/// session, each a separate process (over ssh, a separate connection), so the cadence
/// mirrors psmux's polled local read.
const SCREEN_POLL_MS: u64 = 1500;

/// screen: one per-user daemon, enumerated from `-ls`, polled for change, each session
/// displayed through its own attachment.
pub struct Screen {
    pub bin: String,
}

#[async_trait]
impl Mux for Screen {
    /// screen has no tmux-style `-S <path>` server-socket flag; its `-S` is a session
    /// NAME, so a server socket must never be handed to it.
    fn takes_server_socket(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "screen"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        // One daemon holds every session, but there is no in-place `switch-client`, so
        // the display reattaches per session like a per-session mux.
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(ScreenDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &vocab::list_sessions(&self.bin));
        match runner.run(&name, &args).await {
            Ok(out) => Ok(vocab::parse_sessions(
                transport.host_id(),
                self.kind(),
                &String::from_utf8_lossy(&out),
            )),
            // screen exits 1 (stdout "No Sockets found") when it is reachable but empty —
            // the benign no-sessions case, distinct from a dead host.
            Err(RunError::Exit { code: 1, .. }) => Ok(Vec::new()),
            Err(e) if crate::mux::is_no_sessions(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vocab::attach(&self.bin, session)
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        // screen has no control-mode channel; its CLI is one process per query.
        None
    }

    fn death_signal(&self) -> DeathSignal {
        // The display reattaches per session, so the attachment dying IS the shown
        // session dying.
        DeathSignal::Eof
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: SCREEN_POLL_MS,
        }
    }

    fn list_panes_plan(&self, session: &str) -> Vec<String> {
        vocab::windows(&self.bin, session)
    }

    fn parse_panes(&self, out: &str) -> Vec<WindowPanes> {
        vocab::parse_windows(out)
    }

    fn select_window_plan(&self, target: &str) -> Vec<String> {
        // `select_window_plan` receives a `session:window` target; screen addresses a
        // session with `-S` and a window by `select <index>`.
        let (session, index) = match target.rsplit_once(':') {
            Some((s, i)) => (s, i.parse::<i64>().unwrap_or(0)),
            None => (target, 0),
        };
        vocab::select_window(&self.bin, session, index)
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        vocab::new_session(&self.bin, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CannedRunner(Mutex<Option<Result<Vec<u8>, RunError>>>);
    impl CannedRunner {
        fn ok(out: &str) -> Self {
            CannedRunner(Mutex::new(Some(Ok(out.as_bytes().to_vec()))))
        }
        fn err(e: RunError) -> Self {
            CannedRunner(Mutex::new(Some(Err(e))))
        }
    }
    #[async_trait]
    impl Runner for CannedRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    fn screen() -> Screen {
        Screen {
            bin: "screen".into(),
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }
    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn screen_is_polled_dies_by_eof_and_takes_no_socket() {
        let m = screen();
        assert_eq!(m.kind(), "screen");
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.death_signal(), DeathSignal::Eof);
        assert_eq!(
            m.event_source(),
            EventSource::Poll {
                interval_ms: SCREEN_POLL_MS
            }
        );
        assert!(!m.takes_server_socket(), "screen's -S is a session name, not a socket");
        assert!(m.control_argv().is_none() && m.control_protocol().is_none());
        assert!(m.switch_in_place("jup", "api", Some("/dev/pts/3")).is_none());
        let _object_safe: Box<dyn Mux> = Box::new(screen());
    }

    #[test]
    fn attach_is_multi_display_dash_x() {
        assert_eq!(
            screen().attach_plan("api"),
            argv(&["screen", "-x", "api"])
        );
    }

    #[test]
    fn new_session_is_a_silent_detached_dmS() {
        assert_eq!(
            screen().new_session_plan("dev"),
            argv(&["screen", "-dmS", "dev"])
        );
    }

    #[test]
    fn selecting_a_window_uses_s_select() {
        assert_eq!(
            screen().select_window_plan(&crate::mux::window_target("dev", 2)),
            argv(&["screen", "-S", "dev", "-X", "select", "2"])
        );
    }

    #[test]
    fn the_window_query_is_s_q_windows() {
        assert_eq!(
            screen().list_panes_plan("api"),
            argv(&["screen", "-S", "api", "-Q", "windows"])
        );
    }

    #[tokio::test]
    async fn enumerate_reads_the_ls_listing() {
        let m = screen();
        let out = "There are screens on:\n\t123.work\t(08/25/2026 10:00:00 PM)\t(Detached)\n\t124.dev\t(08/25/2026 10:00:01 PM)\t(Attached)\n";
        let runner = CannedRunner::ok(out);
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["work", "dev"]);
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "screen"));
    }

    #[tokio::test]
    async fn an_idle_screen_is_empty_and_an_unreachable_host_is_an_error() {
        // `screen -ls` exits 1 (stdout "No Sockets found") when reachable but empty.
        let idle = CannedRunner::err(RunError::Exit {
            stderr: String::new(),
            code: 1,
        });
        assert!(screen().enumerate(&ssh("jup"), &idle).await.unwrap().is_empty());
        let down = CannedRunner::err(RunError::Other(
            "ssh: connect to host jup port 22: Connection timed out".into(),
        ));
        assert!(screen().enumerate(&ssh("jup"), &down).await.is_err());
        let missing = CannedRunner::err(RunError::Exit {
            stderr: "screen: command not found".into(),
            code: 127,
        });
        assert!(screen().enumerate(&ssh("jup"), &missing).await.is_err());
    }

    // LIVE: enumerate over a real remote screen server. `#[ignore]` (needs ssh jupiter00).
    //   cargo test --lib screen::tests::screen_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs ssh jupiter00 with screen"]
    #[tokio::test]
    async fn screen_enumerate_live() {
        use crate::model::source::ExecRunner;
        let ssh = crate::transport::ssh("jupiter00".into(), String::new(), "linux".into());
        let got = screen().enumerate(&ssh, &ExecRunner).await;
        eprintln!("jupiter00/screen sessions: {got:?}");
    }
}
```

- [ ] **Step 3: Write the display driver** — `src/mux/screen/display.rs` (mirrors zellij's reattach-per-session driver):

```rust
//! The screen display driver: a per-session mux displayed through ONE per-host PTY that
//! is REATTACHED whenever the selected session changes. `Screen::driver` constructs it,
//! so mux selection lives in the screen family, not a central match.
//!
//! There is no in-place client switch to make: screen has no `switch-client` equivalent
//! and a client cannot be named from outside the session it is in, so every session
//! change is a fresh `screen -x <name>` attach. The stale attachment is kept until the
//! new one is ready so the view never blanks between the two.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{lower_select_window, DriverCtx, MuxDriver};
use crate::model::Selection;

/// screen: one per-user daemon, displayed through ONE per-host PTY that is REATTACHED
/// whenever the selected session changes.
pub struct ScreenDriver;

impl MuxDriver for ScreenDriver {
    fn kind(&self) -> &str {
        "screen"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.nav);
        let control = ctx.mgr.get(&sel.source);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let live = ctx.registry.contains(&key);
        let already_on = host.display.shows(&key) == Some(sel.session.as_str());
        let pre_mismatch = !already_on;

        if live && already_on {
            // The live attachment already shows this session, so only a window row can
            // need moving: `-X select` on the session, no teardown.
            tracing::info!(
                host = %sel.source,
                model = "per-session",
                decision = "warm",
                reason = "already-on",
                session = %sel.session,
                "display_show"
            );
            if let Some(win) = sel.window {
                lower_select_window(host, control, &sel.session, win);
            }
            crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
            return true;
        }

        // REATTACH: the only way to move screen's display. The stale attachment is KEPT
        // in the registry so its grid stays on screen until DisplayReady swaps in the new
        // one (stale-while-revalidate).
        let reason = if live { "other-session" } else { "no-live-client" };
        tracing::info!(
            host = %sel.source,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
        host.display.clear(&key);
        let mux_argv = host.mux.attach_plan(&sel.session);
        let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        let id = request_attach(
            ctx.registry,
            ctx.worker,
            &mut host.display,
            ctx.attach_seq,
            &key,
            argv,
            (cols, rows),
        );
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        host.display.set_shows(&key, &sel.session);

        if let Some(win) = sel.window {
            lower_select_window(host, control, &sel.session, win);
        }
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::app::runtime::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::app::runtime::display_key(ctx.hosts, sel), bytes);
    }

    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        // Per-session attaches are selected on demand by `show`, not pre-warmed: sync
        // only tears down the host PTY when the host has no sessions left.
        if sessions.is_empty() {
            ctx.registry.remove(source);
            if let Some(host) = ctx.hosts.get_mut(source) {
                host.display.clear(source);
            }
        }
    }
}
```

- [ ] **Step 4: Register the family + add detection** — edit `src/mux/mod.rs`:

Add `mod screen;` and `pub use screen::Screen;` near the other families:

```rust
mod control;
mod psmux;
mod screen;
mod tmux;
pub mod vocab;
mod zellij;
```

Add screen to `known_muxes()`:

```rust
fn known_muxes() -> &'static [MuxKind] {
    &[
        MuxKind {
            name: "psmux",
            make: |bin| Box::new(Psmux { bin }),
        },
        MuxKind {
            name: "zellij",
            make: |bin| Box::new(Zellij { bin }),
        },
        MuxKind {
            name: "screen",
            make: |bin| Box::new(screen::Screen { bin }),
        },
    ]
}
```

Add the `-v` screen probe in `detect_backend`, between the help-marker loop and the `-V` tmux fallback:

```rust
    // screen identifies itself with `-v` ("Screen version 4.09.00 (GNU) ..."), a
    // positive signal distinct from tmux's `-V`. Checked before the `-V` tmux fallback
    // so a real screen is not handed to tmux's shared-server driver.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "-v".to_string()]);
    if let Ok(out) = runner.run(&name, &args).await {
        let low = String::from_utf8_lossy(&out).to_lowercase();
        if low.contains("screen version") && low.contains("gnu") {
            if let Some(make) = known_muxes().iter().find(|k| k.name == "screen").map(|k| k.make) {
                return Some(make(bin.to_string()));
            }
        }
    }
```

- [ ] **Step 5: Update the existing registration tests** in `src/mux/mod.rs`:

```rust
#[test]
fn discovery_only_looks_for_muxes_xmux_can_drive() {
    let names = supported_muxes();
    assert_eq!(names, vec!["tmux", "psmux", "zellij", "screen"]);
    for name in names {
        assert_eq!(for_binary(name).kind(), name, "{name} must be drivable");
    }
}

#[test]
fn is_recognized_covers_tmux_and_known_muxes() {
    assert!(is_recognized("tmux"));
    assert!(is_recognized("psmux"));
    assert!(is_recognized("zellij"));
    assert!(is_recognized("screen"));
    assert!(!is_recognized("byobu"));
    assert!(!is_recognized(""));
}

// new:
#[test]
fn screen_resolves_by_binary_name_and_by_kind() {
    assert_eq!(for_binary("screen").kind(), "screen");
    assert_eq!(for_kind("screen", "screen-custom").kind(), "screen");
    assert_eq!(for_kind("screen", "screen-custom").bin(), "screen-custom");
}

#[tokio::test]
async fn detect_backend_classifies_screen_via_dash_v() {
    // screen's positive signal is `-v` ("Screen version ... (GNU)"), NOT `-V` (which
    // errors), so the `-V` tmux fallback must never catch it.
    let t = crate::transport::local(None);
    let runner = ProbeRunner::new(
        Some("Must be connected to a terminal."), // `screen help`
        Some("Screen version 4.09.00 (GNU) 30-Jan-22"), // `screen -v`
    );
    let got = detect_backend(&t, "screen", &runner).await.unwrap();
    assert_eq!(got.kind(), "screen");
    assert_eq!(got.server_model(), ServerModel::PerSession);
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib 2>&1 | tail -25`
Expected: all PASS (existing + new).

- [ ] **Step 7: Add the AGENTS working notes** — `src/mux/screen/AGENTS.md`:

```markdown
# Working Notes: src/mux/screen/

screen: one per-user daemon, polled, display reattaches per session.

- `Mux` shape: `PerSession` model (no in-place `switch-client`, so every session change
  reattaches), `EventSource::Poll` (no control-mode channel), `DeathSignal::Eof`.
- `takes_server_socket` is FALSE: screen's `-S` names a session, not a server socket.
- `enumerate` runs `screen -ls`; exit code 1 (stdout `No Sockets found`) is an
  empty-but-reachable mux, never a dead host.
- `list_panes_plan` = `screen -S <name> -Q windows`; the reply is a space-joined
  `num name` list with no active marker, parsed by integer-token window delimiting.
- `attach_plan` = `screen -x <name>` (multi-display) so xmux adds its display client
  whether the session is detached or attached elsewhere.
- Detection: `screen -v` prints `Screen version … (GNU)`; `-V` errors, so screen is
  never caught by the tmux `-V` fallback.
```

- [ ] **Step 8: Commit**

```bash
git add src/mux/screen src/mux/mod.rs
git commit -m "feat(screen): add GNU screen mux family — enumerate/attach/windows + -v detection"
```

---

### Task 3: Live end-to-end verification on jupiter00

**Files:** none (verification only).

- [ ] **Step 1: Build**

Run: `cargo build 2>&1 | tail -3`
Expected: builds.

- [ ] **Step 2: Live enumerate via a scratch binary or test**

Run the ignored live test against the real server:

```bash
ssh jupiter00 'screen -dmS xmuxverify bash -c "sleep 300"; screen -dmS alpha top; sleep 1; screen -ls'
cargo test --lib screen::tests::screen_enumerate_live -- --ignored --nocapture 2>&1 | tail -10
```
Expected: the enumerate output names `xmuxverify` and `alpha`.

- [ ] **Step 3: Run the full suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all pass, 0 failed.

- [ ] **Step 4: Clean up the test sessions**

```bash
ssh jupiter00 'screen -S xmuxverify -X quit; screen -S alpha -X quit'
```

---

## Self-Review Notes

- Spec coverage: enumerate ✓ (Task 2), attach ✓ (Task 2 `-x`), windows ✓ (Task 1 parse +
  Task 2 `-Q windows`), create ✓ (`new_session_plan`), select-window ✓, detection ✓,
  register as a supported mux ✓, doc propagation in a later phase.
- Placeholder scan: every step has concrete code / commands; no TBDs.
- Type consistency: `Screen { bin: String }` mirrors `Zellij { bin: String }`;
  `ScreenDriver` mirrors `ZellijDriver`; `vocab::*` names match the calls in `mod.rs`.
