use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;
use std::sync::Mutex;
use unicode_width::UnicodeWidthStr;

use super::tests_support::auto_nav;

// --- mock ops -----------------------------------------------------------

#[derive(Default)]
struct RecordOps {
    created: Mutex<Vec<String>>,
    logged_in: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Ops for RecordOps {
    fn sources(&self) -> Vec<String> {
        Vec::new()
    }
    async fn list_sessions(&self, _source: &str) -> anyhow::Result<Vec<Session>> {
        Ok(Vec::new())
    }
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session> {
        self.created
            .lock()
            .unwrap()
            .push(format!("{source}/{name}"));
        Ok(Session {
            source: source.into(),
            name: name.into(),
            windows: 1,
            ..Default::default()
        })
    }
    async fn login_command(
        &self,
        source: &str,
        _login: &crate::transport::Login,
        _password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>> {
        self.logged_in.lock().unwrap().push(source.to_string());
        // A child that exits 0 at once: the conversation this stands in for is one that
        // needed nothing typed.
        Ok(Some(crate::transport::CommandSpec::from_argv(vec![
            "true".to_string()
        ])))
    }
    async fn login_follow_ups(
        &self,
        _source: &str,
        _login: &crate::transport::Login,
        _write_config: bool,
        _register: Option<crate::ui::ops::KeyRegistration>,
    ) -> (crate::ui::ops::RegistrationOutcome, Vec<String>) {
        (
            crate::ui::ops::RegistrationOutcome::NotRequested,
            Vec::new(),
        )
    }
}

// --- headless harness ---------------------------------------------------

struct Harness {
    sw: Switcher,
    plan: RenderPlan,
    state: crate::state::State,
    term: Terminal<TestBackend>,
    ops: RecordOps,
}

impl Harness {
    fn new(scan: Scan) -> Self {
        // Landscape ENOUGH: a row is two columns tall, so the side column survives only
        // while `w - nav - 1` beats twice the rows. 140x30 leaves a 91x30 terminal, which
        // is 91 over 60 in real proportions - the shape these list tests are about.
        Self::new_sized(scan, 140, 30)
    }

    /// A harness with a specific backend size - used to exercise the portrait band
    /// layout (height > width), whose navigation differs from the landscape column layout.
    fn new_sized(scan: Scan, w: u16, h_: u16) -> Self {
        let backend = TestBackend::new(w, h_);
        let term = Terminal::new(backend).unwrap();
        let mut state = crate::state::State::from_scan(scan);
        let mut h = Harness {
            sw: Switcher::new(&mut state),
            plan: RenderPlan::default(),
            state,
            term,
            ops: RecordOps::default(),
        };
        h.draw();
        h
    }

    fn from_sources(aliases: &[&str]) -> Self {
        let backend = TestBackend::new(140, 30);
        let term = Terminal::new(backend).unwrap();
        let aliases = aliases.iter().map(|s| s.to_string()).collect();
        let mut state = crate::state::State::from_sources(aliases);
        let mut h = Harness {
            sw: Switcher::from_sources(&mut state),
            plan: RenderPlan::default(),
            state,
            term,
            ops: RecordOps::default(),
        };
        h.draw();
        h
    }

    /// The hint bar's row, read at the width it actually paints: the nav column at
    /// rest, the whole window while the prefix is armed or an input is open (both
    /// float the bar over the view). Reading the nav width unconditionally would clip
    /// the armed cheatsheet or the input line.
    fn hint_bar_text(&self) -> String {
        let buf = self.buf();
        let y = buf.area.height - 1;
        let limit = if self.state.chrome.armed || self.state.is_inputting() {
            buf.area.width
        } else {
            NAV_WIDTH.min(buf.area.width)
        };
        let mut line = String::new();
        for x in 0..limit {
            line.push_str(buf[(x, y)].symbol());
        }
        line.trim_end().to_string()
    }

    /// Only the nav's CARD rows: the nav column minus the hint bar's bottom row, so a
    /// card assertion cannot be satisfied by the bar's own global scan indicator (both
    /// turn the same spinner).
    fn nav_cards_text(&self) -> String {
        let buf = self.buf();
        let limit = NAV_WIDTH.min(buf.area.width);
        let mut out = String::new();
        for y in 0..buf.area.height.saturating_sub(1) {
            for x in 0..limit {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Only the tree pane (first `NAV_WIDTH` columns) - so a hint assertion
    /// is not satisfied by the preview pane's own loading/reconnecting dialog.
    fn nav_text(&self) -> String {
        let buf = self.buf();
        let limit = NAV_WIDTH.min(buf.area.width);
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..limit {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Only the terminal-view region (past the nav column and its view border) - so a
    /// host-screen assertion is not satisfied by the nav card that says the same word.
    fn view_text(&self) -> String {
        let buf = self.buf();
        let first = (NAV_WIDTH + 1).min(buf.area.width);
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in first..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn draw(&mut self) {
        let sw = &self.sw;
        let state = &self.state;
        let previous = self.plan.clone();
        let mut next = None;
        self.term
            .draw(|f| {
                let plan = sw.layout(f.area(), auto_nav(NAV_WIDTH, f.area()), state, &previous);
                sw.render(f, None, false, state, &plan);
                next = Some(plan);
            })
            .unwrap();
        self.plan = next.expect("draw produced a render plan");
    }

    async fn key(&mut self, code: KeyCode) {
        let cmds = self
            .sw
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), &mut self.state);
        // Pump any RunOp inline so tests observe its effect, exactly as the real
        // event loop does (only off-loop there): apply turned the committing key
        // into a Command::RunOp, run_op executes it, apply_op_result folds it in.
        for cmd in cmds {
            if let Command::RunOp(op) = cmd {
                let r = run_op(&op, &self.ops).await;
                self.sw.apply_op_result(r, &mut self.state);
            }
        }
        self.draw();
    }

    async fn ch(&mut self, c: char) {
        self.key(KeyCode::Char(c)).await;
    }

    fn buf(&self) -> &Buffer {
        self.term.backend().buffer()
    }

    fn text(&self) -> String {
        buffer_text(self.buf())
    }

    /// What the open input popup holds, or `""` when none is open - the jump's buffer
    /// is the number under edit, so a test can assert a refused digit never landed.
    fn input_buffer(&self) -> String {
        match &self.state.modal {
            Some(crate::ui::modal::Modal::Input(i)) => i.buffer.clone(),
            _ => String::new(),
        }
    }

    fn nav_row_of(&self, text: &str) -> Option<u16> {
        row_of(self.buf(), text, NAV_WIDTH)
    }

    fn nav_fg_of(&self, text: &str) -> Option<Color> {
        fg_of(self.buf(), text, NAV_WIDTH)
    }

    fn nav_mod_of(&self, text: &str) -> Option<Modifier> {
        mod_of(self.buf(), text, NAV_WIDTH)
    }
}

fn buffer_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Finds the first screen row where `text` appears within the first `limit`
/// columns (the tree pane), returning that row and starting column.
fn locate(buf: &Buffer, text: &str, limit: u16) -> Option<(u16, u16)> {
    let limit = limit.min(buf.area.width);
    let needle: Vec<char> = text.chars().collect();
    for y in 0..buf.area.height {
        let mut x = 0u16;
        while (x as usize) + needle.len() <= limit as usize {
            let matched = needle
                .iter()
                .enumerate()
                .all(|(i, &c)| buf[(x + i as u16, y)].symbol() == c.to_string());
            if matched {
                return Some((x, y));
            }
            x += 1;
        }
    }
    None
}

fn row_of(buf: &Buffer, text: &str, limit: u16) -> Option<u16> {
    locate(buf, text, limit).map(|(_, y)| y)
}

fn fg_of(buf: &Buffer, text: &str, limit: u16) -> Option<Color> {
    locate(buf, text, limit).map(|(x, y)| buf[(x, y)].fg)
}

fn mod_of(buf: &Buffer, text: &str, limit: u16) -> Option<Modifier> {
    locate(buf, text, limit).map(|(x, y)| buf[(x, y)].modifier)
}

// --- sample data --------------------------------------------------------

fn sess(source: &str, name: &str, windows: i64, attached: bool) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        mux: String::new(),
        windows,
        attached,
    }
}

/// Adds a decoy session on a host whose name sorts first, so the card under test is
/// NOT the selected one. The selected card is painted in reverse video, which flattens
/// every level colour on it by design, so a colour assertion has to read an unselected
/// card. The decoy's own words (`aaa`, `parked`, `psmux`) collide with no needle in
/// these tests.
fn selection_parked_elsewhere(mut scan: Scan) -> Scan {
    scan.groups.push(Group {
        source: "aaa".into(),
        err: None,
        sessions: vec![sess_mux("aaa", "parked", "psmux")],
    });
    scan
}

fn sample() -> Scan {
    let groups = vec![
        Group {
            source: "local".into(),
            err: None,
            sessions: vec![
                sess("local", "editor", 2, true),
                sess("local", "build", 1, false),
            ],
        },
        Group {
            source: "jupiter00".into(),
            err: None,
            sessions: vec![sess("jupiter00", "inference", 1, false)],
        },
        Group {
            source: "db-2".into(),
            err: Some("connection timed out".into()),
            sessions: vec![],
        },
    ];
    Scan { groups }
}

/// Two sources with a session each and TWO with none, so the host band holds more than
/// one card. Used where the band's own SIZE is the point: to ←/→ it is one category
/// however many cards it holds.
fn scan_with_a_host_band() -> Scan {
    Scan {
        groups: vec![
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "editor", 1, false)],
            },
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![sess("jupiter00", "inference", 1, false)],
            },
            Group {
                source: "db-2".into(),
                err: Some("connection timed out".into()),
                sessions: vec![],
            },
            Group {
                source: "db-3".into(),
                err: Some("connection timed out".into()),
                sessions: vec![],
            },
        ],
    }
}

/// One reachable source carrying `n` sessions, so the nav holds exactly `n` cards
/// numbered `1..=n`. Used where the card COUNT is the point (a two-digit jump needs
/// more cards than [`sample`] has).
fn scan_with_sessions(n: usize) -> Scan {
    let sessions = (0..n)
        .map(|i| sess("local", &format!("s{i}"), 1, false))
        .collect();
    Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions,
        }],
    }
}

fn cur_session_name(h: &Harness) -> Option<String> {
    match h.sw.current_ref()? {
        RowRef::Session { sess } => Some(sess.name.clone()),
        _ => None,
    }
}

/// The session names of one source's group, in `state.groups` (display) order.
fn group_session_names(h: &Harness, source: &str) -> Vec<String> {
    h.state
        .groups
        .iter()
        .find(|g| g.source == source)
        .map(|g| g.sessions.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

/// The host-group sources in `state.groups` (display) order.
fn group_order(h: &Harness) -> Vec<String> {
    h.state.groups.iter().map(|g| g.source.clone()).collect()
}

/// The single [`MuxOp`](crate::model::MuxOp) a committing key resolved to, pulled
/// out of the [`Command`]s `handle_key` returned - the off-loop op the run loop
/// would spawn. `None` when no op was queued (validation refused / cancelled).
fn only_run_op(cmds: Vec<Command>) -> Option<crate::model::MuxOp> {
    cmds.into_iter().find_map(|c| match c {
        Command::RunOp(op) => Some(op),
        _ => None,
    })
}

fn two_window_scan() -> Scan {
    Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 2, false)],
        }],
    }
}

#[test]
fn arrows_and_hjkl_navigate() {
    // The card list has no levels: ↑/↓ (and k/j) step ONE card along it, and ←/→
    // (and h/l) step one category, the same way on every placement.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let start = sw.selected;
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    let next = sw.selected;
    assert_eq!(next, start + 1, "↓ steps to the next card");
    sw.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut state);
    assert_eq!(sw.selected, start, "↑ steps back");
    // j/k mirror ↓/↑ exactly.
    sw.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(sw.selected, next, "j == ↓");
    sw.handle_key(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(sw.selected, start, "k == ↑");
    // ←/→ are the OTHER step (one category at a time), so neither is a second way to
    // step a card: from the first card of the first source, → leaves that source.
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_ne!(sw.selected, next, "→ is not ↓");
    let next_category = sw.selected;
    sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state);
    assert_eq!(
        sw.selected, start,
        "← returns to the first source's first card"
    );
    // h/l mirror ←/→: the OTHER step, one category at a time, not a card step.
    sw.handle_key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(sw.selected, next_category, "l == →: a category step");
    sw.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        sw.selected, start,
        "h == ←: back to the first source's first card"
    );
}

// --- tests --------------------------------------------------------------

#[tokio::test]
async fn renders_a_session_card_per_session() {
    // One card per session: a `{host}/{mux}` context line over the session name on
    // the detail line. No per-window rows (the focused window a card used to name is
    // gone from the card).
    let h = Harness::new(sample());
    let out = h.text();
    for want in [
        "local",
        "editor",
        "build",
        "jupiter00",
        "inference",
        "db-2",
        "▲", // unreachable host marker (the reason lives on the host screen)
    ] {
        assert!(out.contains(want), "nav missing {want:?}\n{out}");
    }
    assert!(
        !out.contains("shell") && !out.contains("logs"),
        "no window name on any card:\n{out}"
    );
}

#[tokio::test]
async fn launch_preselects_top_row() {
    // #G: on launch the highlight sits on the very top card (index 0) - the first
    // local session (frozen there before any remote streams in); no persisted
    // last_session is consulted and a remote must not steal the top.
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    // A remote streams in and must NOT pull the cursor down.
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        h.sw.selected, 1,
        "the launch cursor is the top SESSION card"
    );
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "local" && sess.name == "editor"
        ),
        "the top card is the local session, not the remote"
    );
}

#[tokio::test]
async fn panes_are_not_selectable() {
    let mut h = Harness::new(sample());
    // The flat card list has no pane rows; the launch card is a session card.
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { .. })),
        "launch lands on a session card"
    );
    // There is nothing under a session card to descend onto: a step lands on another
    // card, never on a pane of the one it left.
    h.key(KeyCode::Down).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { .. }) | Some(RowRef::Host { .. })
        ),
        "a step lands on a card, never on a pane"
    );
    // ↓ steps the flat list; the selection always lands on a real card node.
    let mut saw_session = false;
    for _ in 0..8 {
        let r = h.sw.current_ref();
        assert!(r.is_some(), "selection landed on a node");
        if matches!(r, Some(RowRef::Session { .. })) {
            saw_session = true;
        }
        h.key(KeyCode::Down).await;
    }
    assert!(saw_session, "navigation reaches session cards");
}

/// Whether `s` holds a spinner frame: the marker of a level that has not resolved.
fn spins(s: &str) -> bool {
    s.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
}

#[tokio::test]
async fn rescan_resets_to_scanning_skeleton() {
    // `r` resets every host to its scanning state and signals the loop to
    // re-kick the probes - the tree returns to skeletons until results land.
    let mut h = Harness::new(sample());
    assert!(h.text().contains("inference"), "sessions before rescan");
    h.sw.request_rescan(&mut h.state);
    h.draw();
    assert!(
        h.sw.take_rescan_kick(),
        "rescan must signal the loop to re-probe"
    );
    let tree = h.nav_cards_text();
    assert!(
        spins(&tree),
        "hosts return to a spinning skeleton after rescan:\n{tree}"
    );
    assert!(
        !tree.contains("inference"),
        "stale sessions clear until the re-probe lands:\n{tree}"
    );
}

#[test]
fn initial_source_seed_does_not_arm_a_rescan() {
    let mut h = Harness::from_sources(&["local", "jupiter00"]);

    assert!(
        !h.sw.take_rescan_kick(),
        "launch discovery is started directly, not by the first user input"
    );
}

// --- streaming model (render-first, per-element) ------------------------

#[tokio::test]
async fn from_sources_renders_scanning_skeletons() {
    // The first frame: one host-skeleton row per source, each in a scanning
    // state, before ANY probe result lands. Structure first, data later.
    let h = Harness::from_sources(&["local", "jupiter00"]);
    let out = h.nav_cards_text();
    assert!(out.contains("local"), "host skeleton present:\n{out}");
    assert!(out.contains("jupiter00"), "host skeleton present:\n{out}");
    assert!(
        spins(&out),
        "each host card spins in the level it is waiting on:\n{out}"
    );
    assert_eq!(
        out.matches("scanning").count(),
        1,
        "only the selected card carries the scanning word:\n{out}"
    );
    assert!(
        !out.contains("window"),
        "no pane detail before any probe:\n{out}"
    );
}

#[tokio::test]
async fn a_scanning_host_card_is_one_line_with_a_trailing_spinner() {
    // Every navigation row is one line now, a scanning host included: the host name,
    // the confirmed mux, and ONE spinner trailing the line - in the same trailing
    // place whether or not the mux is already known, so all scanning cards read alike
    // and none leaves a blank second row.
    use super::render::SELECTED_MARK;
    let sp = crate::ui::spinner_glyph(0);
    let non_empty = |h: &Harness| {
        h.nav_cards_text()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
    };

    // A bare source id has no confirmed mux yet: the card is host + trailing spinner.
    let h = Harness::from_sources(&["local"]);
    let rows = non_empty(&h);
    assert_eq!(rows.len(), 1, "one row, no blank second line:\n{rows:?}");
    assert_eq!(rows[0], format!("{SELECTED_MARK} local {sp} scanning"));

    // A qualified id already confirms its mux: same shape, the mux in the middle.
    let h = Harness::from_sources(&["local:zellij"]);
    let rows = non_empty(&h);
    assert_eq!(rows.len(), 1, "one row, no blank second line:\n{rows:?}");
    assert_eq!(
        rows[0],
        format!("{SELECTED_MARK} local/zellij {sp} scanning")
    );
}

#[tokio::test]
async fn remove_source_drops_the_card_and_everything_keyed_to_it() {
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "api", 2, false)],
        None,
        &mut h.state,
    );

    h.sw.remove_source("jupiter00", &mut h.state);
    h.draw();
    let out = h.nav_text();
    assert!(
        !out.contains("jupiter00"),
        "the card is gone:
{out}"
    );
    assert!(
        !out.contains("api"),
        "its sessions went with it:
{out}"
    );
    assert!(!h.state.scanning.contains("jupiter00"));
}

#[tokio::test]
async fn remove_source_ignores_a_source_the_nav_does_not_show() {
    let mut h = Harness::from_sources(&["local"]);
    let before = h.state.groups.len();
    h.sw.remove_source("jupiter00", &mut h.state);
    assert_eq!(h.state.groups.len(), before, "idempotent");
}

#[tokio::test]
async fn apply_source_result_turns_scanning_into_sessions() {
    let mut h = Harness::from_sources(&["local"]);
    assert!(
        spins(&h.nav_cards_text()),
        "the host card spins before the result"
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 2, false)],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.nav_text();
    assert!(
        out.contains("editor"),
        "session appears after result:\n{out}"
    );
    assert!(
        !h.hint_bar_text().contains("scanning"),
        "the scan indicator clears once the only host resolves"
    );
    assert!(
        !spins(&out),
        "a resolved session card is settled, no loading spinner:\n{out}"
    );
}

#[tokio::test]
async fn poll_preserves_session_order_after_scan() {
    // Scan establishes name order db, web. A later poll reports the sessions in a
    // different arrival order - the deterministic name order holds.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false),
            sess("local", "db", 1, false),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "the scan applies name order"
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false),
            sess("local", "web", 1, false),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "a routine poll reproduces the same name order"
    );
}

#[tokio::test]
async fn poll_sorts_a_new_session_into_place() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false),
            sess("local", "db", 1, false),
        ],
        None,
        &mut h.state,
    ); // → db, web
       // A poll surfaces a brand-new session `api`. It sorts into its name position,
       // never appending at the end.
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false),
            sess("local", "web", 1, false),
            sess("local", "api", 1, false),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["api", "db", "web"],
        "a session new since the scan sorts into name position"
    );
}

#[tokio::test]
async fn poll_preserves_host_group_order_after_scan() {
    // Scan settles the host order: local first, then remotes by name (jupiter00 below
    // jupiter06).
    let mut h = Harness::from_sources(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "w", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "b", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "a", 1, false)],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_order(&h),
        vec!["local", "jupiter00", "jupiter06"],
        "the scan orders hosts local-first then by name"
    );
    // A poll reports jupiter06's session again - the deterministic name order holds.
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "b", 1, false)],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_order(&h),
        vec!["local", "jupiter00", "jupiter06"],
        "a routine poll reproduces the same host order"
    );
}

#[tokio::test]
async fn rescan_reapplies_name_order() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false),
            sess("local", "db", 1, false),
        ],
        None,
        &mut h.state,
    ); // → db, web
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false),
            sess("local", "web", 1, false),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "the poll held the order"
    );
    // The `r` re-scan clears sessions + re-seeds scanning; the next result re-applies
    // the deterministic name order, identical to the poll's.
    h.sw.request_rescan(&mut h.state);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false),
            sess("local", "web", 1, false),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "a re-scan re-applies name order"
    );
}

/// Streams the sample three-host tree (local/jupiter00/jupiter06), each with one
/// session, and leaves the selection on the MIDDLE host's session.
async fn three_hosts_cursor_on_middle() -> Harness {
    let mut h = Harness::from_sources(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "build", 1, false)],
        None,
        &mut h.state,
    );
    // infer is the launch preselect - the top card - so a select_address
    // to it is a no-op; pin it as a deliberate user selection so a rebuild won't drift it.
    h.sw.select_address(
        &crate::session::Address::new("jupiter00", "infer"),
        &h.state,
    );
    h.sw.user_moved = true;
    assert_eq!(cur_session_name(&h).as_deref(), Some("infer"));
    h
}

#[tokio::test]
async fn rescan_parks_on_parent_host_not_bottom() {
    let mut h = three_hosts_cursor_on_middle().await;
    h.sw.request_rescan(&mut h.state);
    // Skeleton phase: every session vanished, so the selection parks on infer's parent
    // host (jupiter00), NOT the last host a removal-fallback would jump to.
    match h.sw.current_ref() {
        Some(RowRef::Host { source, .. }) => assert_eq!(
            source, "jupiter00",
            "the re-scan skeleton parks on the parent host, not the bottom"
        ),
        _ => panic!("expected the parent host row after a re-scan"),
    }
}

#[tokio::test]
async fn rescan_returns_cursor_to_the_same_session() {
    let mut h = three_hosts_cursor_on_middle().await;
    h.sw.request_rescan(&mut h.state);
    // Sessions re-stream in a different arrival order; infer's host arrives last.
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "build", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    assert_eq!(
        cur_session_name(&h).as_deref(),
        Some("infer"),
        "a re-scan returns the selection to the session it was on, not the bottom host"
    );
}

#[tokio::test]
async fn rescan_reselect_dropped_when_user_navigates_away() {
    let mut h = three_hosts_cursor_on_middle().await;
    h.sw.request_rescan(&mut h.state);
    // The user navigates to the last host during the skeleton phase.
    h.key(KeyCode::End).await;
    // Sessions re-stream - the selection must NOT get yanked back to infer.
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    assert_ne!(
        cur_session_name(&h).as_deref(),
        Some("infer"),
        "a user move during the skeleton cancels the pending auto-reselect"
    );
}

#[tokio::test]
async fn apply_source_result_empty_shows_empty_status() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result("local".into(), vec![], None, &mut h.state);
    h.draw();
    // The selected card names its state, and the host screen repeats the state with its
    // available actions.
    let cards = h.nav_cards_text();
    assert!(
        cards.contains("no sessions"),
        "the selected card carries its status word:\n{cards}"
    );
    assert!(
        !spins(&cards),
        "and the card stops spinning once it has its answer:\n{cards}"
    );
    let view = h.view_text();
    assert!(
        view.contains("no sessions"),
        "the host screen reads (no sessions):\n{view}"
    );
    assert!(!h.text().contains("scanning"), "no longer scanning");
}

#[tokio::test]
async fn a_reachable_empty_host_card_is_a_single_row() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result("local".into(), vec![], None, &mut h.state);
    h.draw();
    let cards = h.nav_cards_text();
    assert!(
        cards.contains("local"),
        "the card still names the host:\n{cards}"
    );
}

#[tokio::test]
async fn apply_source_result_marks_the_card_and_states_the_reason_on_the_screen() {
    let mut h = Harness::from_sources(&["prod"]);
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("command failed (exit 255): ssh: connect to prod port 22: connection refused".into()),
        &mut h.state,
    );
    h.draw();
    // Nav: the one-cell marker and nothing more. No part of the message reaches the card -
    // the screen is where it is stated, and a card is too narrow to hold it whole.
    let tree = h.nav_text();
    assert!(tree.contains('▲'), "the host row is marked with ▲:\n{tree}");
    for absent in ["connection refused", "command failed"] {
        assert!(
            !tree.contains(absent),
            "the card states no reason, found {absent:?}:\n{tree}"
        );
    }
    // The lone unreachable host is auto-selected → its host screen states it is
    // unreachable and shows why.
    let out = h.text();
    assert!(
        out.contains("unreachable"),
        "the host screen states unreachable:\n{out}"
    );
    assert!(
        out.contains("connection refused"),
        "the host screen shows the failure reason:\n{out}"
    );
}

#[tokio::test]
async fn host_failures_use_distinct_one_cell_glyphs_and_selected_state_words() {
    let scan = Scan {
        groups: vec![
            Group {
                source: "login-box".into(),
                err: Some("alice@login-box: Permission denied (publickey,password).".into()),
                sessions: vec![],
            },
            Group {
                source: "list-box".into(),
                err: Some("invalid tuios session listing: expected value".into()),
                sessions: vec![],
            },
            Group {
                source: "dead-box".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
        ],
    };
    let mut h = Harness::new(scan);
    let cards = h.nav_cards_text();
    assert!(cards.contains('?'), "login-needed glyph:\n{cards}");
    assert!(cards.contains('✗'), "list-failure glyph:\n{cards}");
    assert!(cards.contains('▲'), "unreachable glyph:\n{cards}");
    assert_eq!(
        h.nav_fg_of("?"),
        Some(crate::ui::palette::Palette::default().warning)
    );
    assert_eq!(
        h.nav_fg_of("✗"),
        Some(crate::ui::palette::Palette::default().primary)
    );
    assert_eq!(
        cards.matches("unreachable").count(),
        1,
        "only the selected card has a word:\n{cards}"
    );
    h.key(KeyCode::Down).await;
    let cards = h.nav_cards_text();
    assert_eq!(
        h.nav_fg_of("▲"),
        Some(crate::ui::palette::Palette::default().error)
    );
    assert!(!cards
        .lines()
        .any(|line| line.contains("dead-box") && line.contains("unreachable")));
    assert!(cards
        .lines()
        .any(|line| line.contains("list-box") && line.contains("list failed")));
    assert!(
        h.view_text().contains("expected value"),
        "the listing reason stays on the host screen:\n{}",
        h.view_text()
    );
}

#[tokio::test]
async fn scanning_and_settled_host_glyphs_share_a_fixed_column() {
    let mut h = Harness::from_sources(&["scanbox", "deadbox"]);
    h.sw.apply_source_result(
        "deadbox".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let spinner = crate::ui::spinner_glyph(h.state.chrome.spinner_frame).to_string();
    let x_of = |needle: &str| {
        let buffer = h.buf();
        (0..buffer.area.height)
            .find_map(|y| {
                (0..NAV_WIDTH.min(buffer.area.width)).find(|x| buffer[(*x, y)].symbol() == needle)
            })
            .expect("glyph in nav")
    };
    assert_eq!(
        x_of(&spinner),
        x_of("▲"),
        "state glyphs occupy one fixed slot"
    );
}

#[tokio::test]
async fn long_card_names_are_middle_ellipsized() {
    let mut h = Harness::new_sized(
        Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![Session {
                    source: "local".into(),
                    name: "my-important-production-session-with-a-very-long-tail-session".into(),
                    mux: "tmux".into(),
                    ..Default::default()
                }],
            }],
        },
        48,
        12,
    );
    h.draw();
    let cards = h.nav_cards_text();
    assert!(
        cards.contains('…'),
        "a long card uses a middle ellipsis:\n{cards}"
    );
    assert!(
        cards.contains("my-"),
        "the beginning remains visible:\n{cards}"
    );
    assert!(
        cards.contains("session"),
        "the end remains visible:\n{cards}"
    );
}

#[tokio::test]
async fn open_filter_reports_matches_hidden_hosts_and_bolds_matching_cells() {
    let mut h = Harness::new(Scan {
        groups: vec![
            Group {
                source: "host".into(),
                err: None,
                sessions: vec![Session {
                    source: "host".into(),
                    name: "alpha".into(),
                    ..Default::default()
                }],
            },
            Group {
                source: "alpine".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
        ],
    });
    h.sw.hide_unreachable = true;
    h.sw.rebuild(&mut h.state);
    h.key(KeyCode::Char('/')).await;
    h.ch('l').await;
    h.ch('p').await;
    let hint = h.hint_bar_text();
    assert!(hint.contains("2 matches"), "match count:\n{hint}");
    assert!(hint.contains("1 hidden host"), "hidden-host count:\n{hint}");
    assert!(
        h.nav_mod_of("l")
            .is_some_and(|m| m.contains(Modifier::BOLD)),
        "a matching session-name cell is bold:\n{}",
        h.nav_cards_text()
    );
    assert!(
        h.nav_mod_of("a")
            .is_some_and(|m| !m.contains(Modifier::BOLD)),
        "a non-matching session-name cell is not bold:\n{}",
        h.nav_cards_text()
    );
}

#[tokio::test]
async fn filter_highlights_the_session_part_of_the_matched_address_not_its_title() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            source: "host".into(),
            err: None,
            sessions: vec![Session {
                source: "host".into(),
                name: "alpha".into(),
                ..Default::default()
            }],
        }],
    });
    h.key(KeyCode::Char('/')).await;
    h.ch('h').await;
    h.ch('a').await;
    assert!(
        h.nav_mod_of("h")
            .is_some_and(|m| !m.contains(Modifier::BOLD)),
        "section titles do not carry match emphasis:\n{}",
        h.nav_cards_text()
    );
    assert!(
        h.nav_mod_of("a")
            .is_some_and(|m| m.contains(Modifier::BOLD)),
        "the session character that completes the address match is bold:\n{}",
        h.nav_cards_text()
    );
}

#[tokio::test]
async fn filter_input_keeps_typed_text_visible_at_supported_widths() {
    for width in [24, 50] {
        let mut h = Harness::new_sized(sample(), width, 20);
        h.key(KeyCode::Char('/')).await;
        for ch in "needle".chars() {
            h.ch(ch).await;
        }
        assert!(
            h.text().contains("needle"),
            "counts give way before the edit buffer at width {width}:\n{}",
            h.text()
        );
    }
}

#[tokio::test]
async fn empty_filter_counts_only_visible_cards_and_no_logged_in_host_as_hidden() {
    let mut h = Harness::new(Scan {
        groups: vec![
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![Session {
                    source: "local".into(),
                    name: "alpha".into(),
                    ..Default::default()
                }],
            },
            Group {
                source: "hidden".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            Group {
                source: "kept".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
        ],
    });
    h.state.logged_in.insert("kept".into());
    h.sw.hide_unreachable = true;
    h.sw.rebuild(&mut h.state);
    h.key(KeyCode::Char('/')).await;
    let hint = h.hint_bar_text();
    assert!(hint.contains("2 matches"), "visible-card count: {hint}");
    assert!(hint.contains("0 hidden hosts"), "hidden-host count: {hint}");
}

#[tokio::test]
async fn list_failure_allows_a_new_session_on_the_answering_host() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            source: "list-box:tmux".into(),
            err: Some("invalid tmux session listing: expected value".into()),
            sessions: vec![],
        }],
    });
    h.ch('n').await;
    assert!(h.state.is_inputting(), "new-session input opens");
    assert!(
        h.state.chrome.flash.is_empty(),
        "the host is not unreachable"
    );
}

#[tokio::test]
async fn open_filter_reports_zero_for_the_no_match_fallback() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Char('/')).await;
    for ch in "zzzz".chars() {
        h.ch(ch).await;
    }
    assert!(
        h.hint_bar_text().contains("0 matches"),
        "fallback host cards are not matches: {}",
        h.hint_bar_text()
    );
}

#[tokio::test]
async fn terminal_below_the_minimum_renders_the_required_size() {
    let h = Harness::new_sized(Scan::default(), 20, 3);
    let out = h.text();
    assert!(
        out.contains("need 24x4"),
        "required dimensions are visible:\n{out}"
    );
    assert!(
        out.contains("20x3"),
        "current dimensions are visible:\n{out}"
    );
}

#[tokio::test]
async fn interaction_screens_render_key_tokens_in_one_shape() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![],
        }],
    });
    let key_shape = |h: &Harness, key: &str| {
        mod_of(h.buf(), key, h.buf().area.width).expect("key token on screen")
    };
    assert!(key_shape(&h, "C-g n").contains(Modifier::BOLD));
    h.sw.show_help(&mut h.state);
    h.draw();
    assert!(key_shape(&h, "C-g n").contains(Modifier::BOLD));
}

#[tokio::test]
async fn an_unselected_unreachable_card_keeps_the_warning_mark() {
    // The ▲ mark keeps the error colour. The SELECTED card is painted in
    // reverse video, which flattens every level colour on it by design, so the colour
    // assertion reads an UNSELECTED unreachable card.
    let scan = selection_parked_elsewhere(Scan {
        groups: vec![Group {
            source: "dead".into(),
            err: Some("connection refused".into()),
            sessions: vec![],
        }],
    });
    let h = Harness::new(scan);
    assert_ne!(
        h.sw.selected,
        h.sw.band_boundary().expect("the unreachable card"),
        "the decoy holds the selection, so the mark's colour reads"
    );
    assert_eq!(
        h.nav_fg_of("▲"),
        Some(crate::ui::palette::Palette::default().error),
        "the mark keeps the error colour on an unselected card"
    );
}

#[tokio::test]
async fn a_locked_host_card_reads_locked_with_the_lock_mark() {
    let mut h = Harness::from_sources(&["prod"]);
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    // The card carries the lock mark on its host row (the screen state and reason are
    // the panel's own assertions).
    let tree = h.nav_text();
    assert!(
        tree.lines()
            .any(|l| l.contains("prod") && l.contains(crate::ui::chrome::BLOCK_MARK)),
        "the locked host row carries the lock mark:\n{tree}"
    );
}

#[tokio::test]
async fn login_pane_draws_its_fields_with_the_password_masked() {
    // The login pane is a feature of the terminal view, not a modal: the connection
    // values sit in the panel, driven from `State::login`. It renders them in the clear
    // and the password as bullets, and no plaintext reaches the frame.
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        source: "pwbox".into(),
        address: "100.88.0.0".into(),
        port: "22".into(),
        username: "alice".into(),
        password: "hunter2".into(),
        focus: crate::state::LoginFocus::Password,
        ..Default::default()
    });
    h.draw();
    let screen = h.text();
    assert!(
        h.state.modal.is_none(),
        "the login pane is not a modal:\n{screen}"
    );
    assert!(
        screen.contains("alice") && screen.contains("100.88.0.0"),
        "the pane shows the entered values:\n{screen}"
    );
    assert!(screen.contains('•'), "the password draws masked:\n{screen}");
    assert!(
        !screen.contains("hunter2"),
        "no plaintext reaches the rendered frame:\n{screen}"
    );
}

#[tokio::test]
async fn login_pane_marks_required_fields_and_hints_the_optional_one() {
    // A field carries its own emptiness: a required one is marked in its label, and the
    // optional one says so in the space its value would occupy.
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    let screen = h.text();
    for required in ["address*", "port*", "username*"] {
        assert!(screen.contains(required), "{required} is marked:\n{screen}");
    }
    assert!(
        screen.contains("password") && !screen.contains("password*"),
        "the optional field carries no mark:\n{screen}"
    );
    assert!(
        screen.contains("optional"),
        "an empty optional field says so:\n{screen}"
    );
}

#[tokio::test]
async fn login_pane_offers_the_remember_choice_only_after_a_value_changes() {
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    assert!(
        !h.text().contains("write address, port, username"),
        "nothing to record yet:\n{}",
        h.text()
    );
    h.state.feed_login("pwbox", b"x");
    h.draw();
    assert!(
        h.text()
            .contains("write address, port, username to ssh config"),
        "an edited value is worth recording:\n{}",
        h.text()
    );
}

#[tokio::test]
async fn login_pane_prefills_all_values_from_ssh_config() {
    let mut h = Harness::from_sources(&["e2e-box"]);
    h.state.chrome.set_login_defaults(
        std::collections::HashMap::from([(
            "e2e-box".into(),
            crate::provision::env::LoginDefaults {
                address: crate::provision::env::LoginValue {
                    value: "127.0.0.1".into(),
                    provenance: "from ssh config",
                },
                port: crate::provision::env::LoginValue {
                    value: "2222".into(),
                    provenance: "from ssh config",
                },
                username: crate::provision::env::LoginValue {
                    value: "dev".into(),
                    provenance: "from ssh config",
                },
            },
        )]),
        Default::default(),
    );
    h.sw.apply_source_result(
        "e2e-box".into(),
        vec![],
        Some("dev@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.feed_login("e2e-box", b"");
    let draft = h.state.login.as_ref().unwrap();
    assert_eq!(draft.address, "127.0.0.1");
    assert_eq!(draft.port, "2222");
    assert_eq!(draft.username, "dev");
    assert_eq!(draft.address, draft.default_address);
    assert_eq!(draft.port, draft.default_port);
    assert_eq!(draft.username, draft.default_username);
    h.draw();
    assert!(
        h.view_text().contains("from ssh config"),
        "resolved values show their provenance:\n{}",
        h.view_text()
    );
    assert!(!h.text().contains("write address, port, username"));
}

#[tokio::test]
async fn login_and_key_registration_results_are_one_toast_kept_for_the_host() {
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: LoginOutcome {
                connect: UnlockOutcome::Ok,
                registration: RegistrationOutcome::Registered,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    h.draw();
    let toast = &h.state.notify.toasts[0];
    assert_eq!(
        toast.title, "pwbox",
        "the toast names the host it reports on"
    );
    let lines: Vec<&str> = toast.notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(lines, ["logged in", "public key registered"]);
    assert!(toast.until.is_some(), "a success leaves by itself");
    assert!(
        h.text().contains("public key registered"),
        "the toast is on screen:\n{}",
        h.text()
    );
    assert_eq!(
        h.state.notify.history.len(),
        2,
        "both results are in the history"
    );
    assert_eq!(
        h.state.registration_reports.get("pwbox"),
        Some(&RegistrationOutcome::Registered)
    );
}

#[tokio::test]
async fn a_failed_login_and_a_skipped_key_are_a_toast_that_stays() {
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::state::notify::Level;
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: LoginOutcome {
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied".into(),
                },
                registration: RegistrationOutcome::NotRequested,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    let toast = &h.state.notify.toasts[0];
    assert_eq!(toast.notes[0].level, Level::Error);
    assert_eq!(
        toast.notes[0].text, "login failed: the password was refused",
        "the toast carries the verdict; the pane keeps ssh's own words"
    );
    assert!(toast.until.is_none(), "a failure waits to be dismissed");

    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: LoginOutcome {
                connect: UnlockOutcome::Ok,
                registration: RegistrationOutcome::Skipped("no key to send".into()),
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    let levels: Vec<Level> = h.state.notify.toasts[0]
        .notes
        .iter()
        .map(|n| n.level)
        .collect();
    assert_eq!(levels, [Level::Success, Level::Warning]);
    assert!(h.state.notify.toasts[0].until.is_none());

    // A cancelled login says nothing about the connection the user ended.
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: LoginOutcome {
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::Cancelled,
                    reason: "cancelled".into(),
                },
                registration: RegistrationOutcome::NotRequested,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    assert!(h.state.notify.toasts.is_empty());
}

#[tokio::test]
async fn a_running_login_says_so_in_place_of_the_submit_button() {
    // Submitting the pane hands the values to ssh and waits. The form stays on screen
    // with what it collected, and the row the user would press says the login is running
    // and how to stop it, so the pane is never a screen where nothing happens.
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        source: "pwbox".into(),
        address: "100.88.0.0".into(),
        port: "22".into(),
        username: "alice".into(),
        ..Default::default()
    });
    h.draw();
    assert!(
        h.text().contains("[ login ]"),
        "the pane offers the login before one runs:\n{}",
        h.text()
    );

    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("pwbox"));
    h.draw();
    let screen = h.text();
    assert!(
        screen.contains("logging in") && screen.contains("esc to stop"),
        "the running login says so and says how to stop it:\n{screen}"
    );
    assert!(
        screen.contains("username") && screen.contains("100.88.0.0"),
        "the values the login is using stay on screen:\n{screen}"
    );
    assert!(
        !screen.contains("[ login ]"),
        "there is nothing left to submit:\n{screen}"
    );

    // A login running for a DIFFERENT host is not this pane's: the button stays.
    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("elsewhere"));
    h.draw();
    assert!(
        h.text().contains("[ login ]"),
        "another host's login leaves this pane alone:\n{}",
        h.text()
    );
}

#[tokio::test]
async fn the_verdict_takes_the_login_screen_down() {
    // However the conversation ended, it is over: the PTY goes with it and the pane comes
    // back holding what was typed, so a failure is retried rather than retyped.
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::OpResult;
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        source: "pwbox".into(),
        username: "alice".into(),
        ..Default::default()
    });
    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("pwbox"));
    h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: crate::ui::ops::LoginOutcome {
                connect: UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied (publickey,password).".into(),
                },
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("ssh: connect to host pwbox: Connection refused".into()),
        &mut h.state,
    );
    assert!(
        h.state.login_run.is_none(),
        "the running login is gone once the verdict is in"
    );
    // The verdict's toast is the subject of its own test; this one reads the pane.
    h.state.notify.dismiss_all();
    h.draw();
    assert!(
        h.text().contains("alice"),
        "the pane comes back holding what was typed:\n{}",
        h.text()
    );
    assert!(
        h.text().contains("the password was refused"),
        "a later probe does not replace the login's own reason:\n{}",
        h.text()
    );
}

#[tokio::test]
async fn login_success_reprobes_only_that_machine_and_a_failure_keeps_it_blocked() {
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::OpResult;
    let mut h = Harness::from_sources(&["pwbox"]);
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    // A successful unlock returns the unlocked source so the app re-probes ONLY that
    // machine (its reach changed locked→connected), and it does NOT arm a whole-roster
    // re-scan - that would re-probe every host for one that changed.
    let reprobe = h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login {
                user: Some("alice".into()),
                ..Default::default()
            },
            outcome: crate::ui::ops::LoginOutcome {
                connect: UnlockOutcome::Ok,
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    // The values that authenticated come back WITH the source: the app records them on
    // the machine, so the re-probe below reaches it as the account that just worked
    // rather than as whoever runs xmux.
    assert_eq!(
        reprobe,
        Some((
            "pwbox".to_string(),
            crate::transport::Login {
                user: Some("alice".into()),
                ..Default::default()
            }
        )),
        "success re-probes the unlocked machine with the login that worked"
    );
    assert!(
        !h.sw.take_rescan_kick(),
        "success does not re-scan the whole roster"
    );
    // A failed unlock stays locked and re-probes nothing.
    h.sw.apply_source_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    let reprobe = h.sw.apply_op_result(
        OpResult::Login {
            source: "pwbox".into(),
            login: crate::transport::Login::default(),
            outcome: crate::ui::ops::LoginOutcome {
                connect: UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied (publickey,password).".into(),
                },
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                notes: Vec::new(),
            },
        },
        &mut h.state,
    );
    assert_eq!(reprobe, None, "a failure re-probes nothing");
    assert!(
        h.sw.current_host_blocked(),
        "auth failure keeps the card locked"
    );
}

#[tokio::test]
async fn a_card_claims_a_mux_only_when_it_is_confirmed() {
    // A host-state card claims no mux it cannot back with an answer. A bare-id host
    // (its mux is a config assumption, never probed until the enumeration answers)
    // reads the host alone when unreachable or scanning; a QUALIFIED id names a mux
    // the machine was resolved to serve, which is a confirmed fact even while the host
    // is unreachable; a settled reachable host shows the mux its enumeration answered
    // through.
    let h = Harness::new(Scan {
        groups: vec![
            // bare id: mux only assumed, unreachable - no claim.
            Group {
                source: "dead".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            // qualified id: the mux was resolved on the machine, so it is a fact.
            Group {
                source: "srv:zellij".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            // settled reachable empty host: the enumeration answered through its mux.
            Group {
                source: "fresh:psmux".into(),
                err: None,
                sessions: vec![],
            },
        ],
    });
    let out = h.nav_text();
    assert!(
        !out.contains("dead/") && !out.contains("/tmux"),
        "a bare unreachable card claims no mux:\n{out}"
    );
    assert!(
        out.contains("srv/zellij ▲"),
        "a qualified unreachable card keeps its resolved mux:\n{out}"
    );
    assert!(
        out.contains("fresh/psmux"),
        "a settled reachable host shows the mux its enumeration answered through:\n{out}"
    );
}

#[tokio::test]
async fn hide_unreachable_leaves_no_card_for_the_unreachable_host() {
    // With `[ui] hide-unreachable` on, the settled unreachable host takes no card;
    // the reachable hosts keep theirs.
    let mut h = Harness::new(sample());
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.draw();
    let out = h.nav_text();
    assert!(
        !out.contains("db-2"),
        "the unreachable host takes no card:\n{out}"
    );
    assert!(
        out.contains("jupiter00"),
        "the reachable hosts keep their cards:\n{out}"
    );
}

#[tokio::test]
async fn the_filter_names_a_hidden_unreachable_host_and_its_card_returns() {
    // The named card is the one entry to the unreachable screen: the filter naming
    // the hidden host brings its card back, and Enter lands the selection on it.
    let mut h = Harness::new(sample());
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.draw();
    h.ch('/').await;
    h.ch('d').await;
    h.ch('b').await;
    assert!(
        h.nav_cards_text().contains("db-2"),
        "the filter naming the host brings its card back:\n{}",
        h.nav_cards_text()
    );
    h.key(KeyCode::Enter).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Host { source, unreachable: true, .. }) if source == "db-2"
        ),
        "the selection lands on the named host's card"
    );
    let out = h.view_text();
    assert!(
        out.contains("unreachable"),
        "the unreachable screen is reachable:\n{out}"
    );
}

#[tokio::test]
async fn hide_unreachable_off_brings_the_card_back() {
    // The setter rebuilds the rows whichever way it flips: off undoes the hiding.
    let mut h = Harness::new(sample());
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.draw();
    assert!(!h.nav_text().contains("db-2"));
    h.sw.set_hide_unreachable(false, &mut h.state);
    h.draw();
    assert!(
        h.nav_text().contains("db-2"),
        "setting it back to false brings the card back:\n{}",
        h.nav_text()
    );
}

#[tokio::test]
async fn a_selected_host_going_unreachable_hides_and_the_selection_lands_on_a_remaining_card() {
    // A host going unreachable mid-run hides from that result on; the selection
    // sitting on its card falls to a remaining card instead of vanishing.
    let mut h = Harness::from_sources(&["local", "db-2"]);
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "editor", "tmux")],
        None,
        &mut h.state,
    );
    h.sw.move_to(-1, &h.state); // the user parked the selection on db-2's card
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"),
        "the selection starts on the db-2 card"
    );
    h.sw.apply_source_result(
        "db-2".into(),
        Vec::new(),
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.nav_text();
    assert!(!out.contains("db-2"), "hidden the moment it fails:\n{out}");
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "local" && sess.name == "editor"
        ),
        "the selection lands on a remaining card"
    );
}

#[tokio::test]
async fn a_selected_host_own_failure_leaving_no_cards_does_not_panic_the_fallback() {
    // A poll failure for the one host still holding cards empties the nav in one
    // result (hiding on prunes the settled unreachable group whole). The selection
    // sat on that host's session card, so the removal fallback runs with a previous
    // index past the now-empty rows: it lands nowhere and nothing panics.
    let mut h = Harness::from_sources(&["local", "db-2"]);
    h.sw.set_hide_unreachable(true, &mut h.state);
    // db-2 settles unreachable first and hides, leaving local the only cards.
    h.sw.apply_source_result(
        "db-2".into(),
        Vec::new(),
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "editor", "tmux")],
        None,
        &mut h.state,
    );
    h.sw.move_to(0, &h.state); // the user parked the selection on local's session card
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "local" && sess.name == "editor"
        ),
        "the selection starts on local's session card"
    );
    // local's poll fails: every card vanishes and the fallback runs on an empty list.
    h.sw.apply_source_result(
        "local".into(),
        Vec::new(),
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.draw();
    assert!(
        h.nav_cards_text().trim().is_empty(),
        "the nav renders empty:\n{:?}",
        h.nav_cards_text()
    );
    assert!(
        h.sw.current_ref().is_none(),
        "with no card left, the fallback lands nowhere"
    );
}

#[tokio::test]
async fn a_hidden_unreachable_host_returns_when_its_scan_answers() {
    // Hiding is a view state, not a removal: a successful scan revives the card.
    let mut h = Harness::new(sample());
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.draw();
    assert!(!h.nav_text().contains("db-2"));
    h.sw.apply_source_result(
        "db-2".into(),
        vec![sess_mux("db-2", "reports", "tmux")],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        h.nav_text().contains("db-2"),
        "a successful scan revives the host:\n{}",
        h.nav_text()
    );
}

#[tokio::test]
async fn hiding_every_host_leaves_a_tidy_empty_nav() {
    // Every host unreachable and hiding on: the nav holds no card, and the hint bar
    // keeps its prefix so the session is still operable.
    let mut h = Harness::from_sources(&["db-2"]);
    h.sw.set_hide_unreachable(true, &mut h.state);
    h.sw.apply_source_result(
        "db-2".into(),
        Vec::new(),
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.draw();
    assert!(
        h.nav_cards_text().trim().is_empty(),
        "only empty lines in the nav:\n{:?}",
        h.nav_cards_text()
    );
    assert!(
        h.hint_bar_text().contains("C-g"),
        "the hint bar still shows the prefix:\n{}",
        h.hint_bar_text()
    );
}

#[tokio::test]
async fn unreachable_host_screen_keeps_a_long_reason_whole() {
    // ssh wraps the failure in its own context and names it LAST, past the width of the
    // screen: a reason cut off at the edge drops the only words that say what went wrong.
    let reason =
        "command failed (exit 255): ssh: connect to host kyla.tail1cbccc.ts.net port 22: Connection timed out";
    let mut h = Harness::from_sources(&["kyla"]);
    h.sw.apply_source_result("kyla".into(), vec![], Some(reason.into()), &mut h.state);
    h.draw();
    let out = h.view_text();
    for word in reason.split_whitespace() {
        assert!(
            out.contains(word),
            "the screen keeps `{word}` of the reason:
{out}"
        );
    }
}

#[tokio::test]
async fn the_session_xmux_runs_in_is_never_a_terminal_view_target() {
    // Attaching to it would put a second client on the session holding xmux: that moves
    // the user's own client and paints xmux inside itself. The refusal is on the TARGET,
    // which is the one value the display reconcile, the attach and the mux-side switch
    // all read, so none of them can reach the session by another path.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "xmus", "psmux")],
        None,
        &mut h.state,
    );
    h.draw();

    assert_eq!(
        h.sw.terminal_view_target().target,
        "",
        "no target, so nothing downstream attaches"
    );
    assert!(
        h.sw.current_attach_target(&h.state).is_none(),
        "and the mux-side switch has nothing to switch to"
    );
}

#[tokio::test]
async fn the_session_xmux_runs_in_shows_a_screen_instead_of_its_grid() {
    // Refusing silently would leave the last session's grid standing under the wrong
    // card. The screen says whose session it is and why it is not shown.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "xmus", "psmux")],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    assert!(out.contains("local/xmus"), "headlined by address:\n{out}");
    assert!(out.contains("running xmux"), "and by its state:\n{out}");
    assert!(out.contains("refused"), "and says it is refused:\n{out}");
}

#[tokio::test]
async fn the_self_session_screen_headline_carries_the_mux_too() {
    // The screen is reached by an ADDRESS, and an address names three levels: the machine,
    // its mux, the session. The headline states all three, in the cards' own grammar.
    let mut h = Harness::from_sources(&["local"]);
    h.state.chrome.set_source_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "xmus", "psmux")],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    assert!(
        out.contains("local/psmux/xmus"),
        "machine, mux, session:\n{out}"
    );
}

#[tokio::test]
async fn another_instances_session_is_shown_like_any_other() {
    // Only xmux's OWN session is refused. A session running a DIFFERENT xmux mirrors
    // like anything else - that is a real screen a user may want to look at.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "other", "psmux")],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(h.sw.terminal_view_target().target, "other");
    assert!(h.sw.current_attach_target(&h.state).is_some());
}

#[tokio::test]
async fn unreachable_host_screen_names_the_provider_that_offered_the_host() {
    // A host that fails is only half an answer while the user cannot tell why it is on
    // the list at all: a tailnet peer nobody wrote down reads as a mystery. The screen
    // names the provider that put it there, which is also the one they would turn off.
    let mut h = Harness::from_sources(&["kyla"]);
    h.state.chrome.set_roster_providers(
        [("kyla".to_string(), "tailscale".to_string())]
            .into_iter()
            .collect(),
    );
    h.sw.apply_source_result(
        "kyla".into(),
        vec![],
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    assert!(out.contains("provider"), "the row is named:\n{out}");
    assert!(out.contains("tailscale"), "and carries the answer:\n{out}");
}

#[tokio::test]
async fn a_host_nothing_recorded_gets_no_provider_row() {
    // An empty map is not "offered by nothing": it is nothing recorded. The row is
    // absent rather than blank, so the screen never states an answer it does not have.
    let mut h = Harness::from_sources(&["kyla"]);
    h.sw.apply_source_result(
        "kyla".into(),
        vec![],
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    assert!(
        out.contains("connection timed out"),
        "the screen is up:\n{out}"
    );
    assert!(!out.contains("provider"), "and says nothing of one:\n{out}");
}

#[tokio::test]
async fn unreachable_host_screen_shows_ssh_config_stanza() {
    let mut h = Harness::from_sources(&["jupiter00"]);
    h.state.chrome.set_login_defaults(
        Default::default(),
        std::collections::HashMap::from([(
            "jupiter00".into(),
            "Host jupiter00\n    HostName 143.248.140.120\n    User hrlee\n".into(),
        )]),
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![],
        Some("no route".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.text();
    assert!(
        out.contains("HostName 143.248.140.120"),
        "shows the host's ssh config:\n{out}"
    );
    assert!(out.contains("hrlee"), "shows the configured user:\n{out}");
    assert!(
        !out.contains("1.2.3.4"),
        "does NOT leak an unrelated host's config:\n{out}"
    );
}

#[tokio::test]
async fn streaming_keeps_local_preselect_when_untouched() {
    // An untouched selection sits on the top SESSION card (the local host's first
    // session, row 1 - row 0 is its section title), and a later REMOTE
    // session streaming in must NOT steal it: the selection must not leap to a remote
    // on first launch (#1).
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        h.sw.selected, 1,
        "the selection stays on the local session card, under its section title"
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        h.sw.selected, 1,
        "an untouched selection stays on the top session card; a remote must not steal it"
    );
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "local"),
        "the untouched selection is the local session card"
    );
}

#[tokio::test]
async fn streaming_holds_the_first_session_that_answered() {
    // The hosts answer the scan in whatever order they happen to, and every answer
    // rebuilds the rows. The selection lands on the first session to appear and STAYS
    // on it: a host answering later does not take the cursor, not even one the display
    // order puts above it. A cursor that walked from host to host through the scan would
    // attach a session per step, leaving the screen on whichever step is still in flight
    // while the cursor names another.
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    // The remote answers first.
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "jupiter00"
        ),
        "the first session to answer takes the cursor"
    );
    // The local host answers second, and the order puts it ABOVE the remote - the case a
    // top-card preselect would move the cursor for.
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "jupiter00" && sess.name == "infer"
        ),
        "a host answering later does not take the cursor off the session already on screen"
    );
}

#[tokio::test]
async fn request_rescan_arms_a_display_reattach() {
    // The `r` re-scan also arms an explicit re-attach of the current display, so a
    // detached / dead display client is re-created on demand (the loop consumes it).
    let mut state = crate::state::State::from_sources(vec!["h".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    assert!(
        !sw.take_reattach_kick(),
        "no re-attach armed before a re-scan"
    );
    sw.request_rescan(&mut state);
    assert!(
        sw.take_reattach_kick(),
        "an r re-scan arms a display re-attach"
    );
    assert!(!sw.take_reattach_kick(), "the kick is consumed once");
}

#[tokio::test]
async fn rebuild_holds_a_user_moved_session_against_the_preselect() {
    // The selection thrash: once the user has moved the selection onto a session, a bare
    // rebuild (a frequent poll / %-event that does not route through restore_focus)
    // must keep it there, not snap it back to the preferred preselect.
    let mut state = crate::state::State::from_sources(vec!["h".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    sw.apply_source_result(
        "h".into(),
        vec![sess("h", "a", 1, false), sess("h", "b", 1, false)],
        None,
        &mut state,
    );
    let names: Vec<String> = sw
        .rows
        .iter()
        .filter_map(|r| match &r.reference {
            RowRef::Session { sess } => Some(sess.name.clone()),
            _ => None,
        })
        .collect();
    // Pick the session that is NOT the preselect target, so a
    // bare rebuild's preselect would move the selection here if the fix were absent.
    let other = names[1].clone();
    let idx = sw
        .rows
        .iter()
        .position(|r| matches!(&r.reference, RowRef::Session { sess } if sess.name == other))
        .expect("other session card");
    sw.set_selected(idx, &state);
    sw.user_moved = true;
    sw.rebuild(&mut state);
    let got = match sw.current_ref() {
        Some(RowRef::Session { sess }) => sess.name.clone(),
        _ => "<not a session>".to_string(),
    };
    assert_eq!(
        got, other,
        "a user-selected session must survive a bare rebuild (no snap to preselect)"
    );
}

#[tokio::test]
async fn streaming_preserves_cursor_once_user_moves() {
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "editor", 1, false),
            sess("local", "build", 1, false),
        ],
        None,
        &mut h.state,
    );
    h.draw();
    // build's card preselected (index 0, name order); step down to editor's card.
    h.key(KeyCode::Down).await;
    assert_eq!(cur_session_name(&h).as_deref(), Some("editor"));
    // A remote session streams in; the selection must NOT jump.
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        cur_session_name(&h).as_deref(),
        Some("editor"),
        "once the user has moved, streaming updates keep the selection put"
    );
}

#[tokio::test]
async fn hint_bar_shows_scanning_progress_then_clears() {
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    let hint_bar = h.hint_bar_text();
    assert!(
        hint_bar.contains("scanning"),
        "hint_bar shows a global scanning indicator:\n{hint_bar:?}"
    );
    assert!(
        hint_bar.contains("/2"),
        "hint_bar shows the host progress fraction:\n{hint_bar:?}"
    );
    h.sw.apply_source_result("local".into(), vec![], None, &mut h.state);
    h.sw.apply_source_result("jupiter00".into(), vec![], None, &mut h.state);
    h.draw();
    let hint_bar = h.hint_bar_text();
    assert!(
        !hint_bar.contains("scanning"),
        "the scanning indicator clears once all hosts settle:\n{hint_bar:?}"
    );
    assert_eq!(
        hint_bar.trim(),
        "C-g",
        "the resting hint bar is the prefix alone:\n{hint_bar:?}"
    );
}

#[tokio::test]
async fn armed_hint_bar_fits_a_narrow_nav() {
    // The armed cheatsheet is the widest thing the bar ever shows, and it now has only
    // the nav column to fit in, so it must degrade to a shorter candidate, never clip.
    let mut state = crate::state::State::from_scan(sample());
    state.chrome.set_armed(true);
    let sw = Switcher::new(&mut state);
    let nav_w = 24u16;
    // Landscape enough for the side column: a row counts as two columns, so the terminal
    // beside a 24-wide nav must beat twice the rows (90 - 25 = 65 against 60).
    let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(nav_w), &state))
        .unwrap();
    let buf = term.backend().buffer();
    let y = buf.area.height - 1;
    let mut hint_bar = String::new();
    for x in 0..nav_w {
        hint_bar.push_str(buf[(x, y)].symbol());
    }
    let hint_bar = hint_bar.trim_end().to_string();
    assert!(
        UnicodeWidthStr::width(hint_bar.as_str()) <= nav_w as usize,
        "the armed cheatsheet fits the nav column:\n{hint_bar:?}"
    );
    assert!(
        hint_bar.contains("C-g"),
        "it still names the armed prefix:\n{hint_bar:?}"
    );
}

#[test]
fn the_nav_renders_at_the_minimum_width() {
    // The side nav may be shrunk to its minimum width. At that width the prefix stays
    // visible and the cards clip.
    let min = crate::app::model::nav_width_min("C-g");
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(min), &state))
        .unwrap();
    let buf = term.backend().buffer();
    let y = buf.area.height - 1;
    let text: String = (0..min).map(|x| buf[(x, y)].symbol()).collect();
    assert_eq!(text.trim_end(), " C-g", "resting bar at min width");

    state.scanning.insert("local".into());
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(min), &state))
        .unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..min).map(|x| buf[(x, y)].symbol()).collect();
    let total = state.groups.len();
    let done = total.saturating_sub(state.scanning.len());
    assert!(
        text.contains(&format!("{done}/{total}")),
        "scan progress stays intact: {text:?}"
    );
}

#[test]
fn hint_bar_has_status_bar_background() {
    // The hint bar is a solid dark status bar fit to what it has to say: at rest the
    // prefix sits on the nav's last row. The cells it owns carry the
    // dark bar background, while columns outside the controls remain with the view below.
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    // Wide enough that the terminal view stays landscape, so the layout is a column and the
    // nav column runs the full height (its last row IS the hint bar).
    let mut term = Terminal::new(TestBackend::new(140, 20)).unwrap();
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer();
    let y = buf.area.height - 1; // the one-line hint bar sits on the nav's last row
    let bg = crate::ui::palette::Palette::default().bar_bg;
    assert_eq!(buf[(1, y)].bg, bg, "a text cell has the dark bar bg");
    assert_eq!(
        buf[(1, y)].fg,
        crate::ui::palette::Palette::default().bar_accent,
        "the leading key token is accented with the bar's own accent"
    );
    // Resting text is " C-g" (4 cells) plus one cell of padding = 5 cells; the bar is
    // fit to that, so it stops well short of the nav column's width instead of filling it.
    let bar_w = 5;
    assert_eq!(
        buf[(bar_w - 1, y)].bg,
        bg,
        "the last padded cell of the bar is also bar bg"
    );
    assert_ne!(
        buf[(bar_w, y)].bg,
        bg,
        "the bar is fit to content - cells past the text are not painted"
    );
}

#[test]
fn hint_bar_text_reflects_configured_prefix() {
    // The hint_bar always-visible key-hints must show the active prefix, not a
    // hardcoded "C-g", so a user who sets a different binding sees the right hint.
    let mut state = crate::state::State::default();
    state.chrome.set_ui_prefix("C-Space".into());
    let text = state.chrome.hint_bar_text(200, &state);
    assert!(
        text.contains("C-Space"),
        "custom prefix must appear in hint_bar:\n{text:?}"
    );
    assert!(
        !text.contains("C-g"),
        "hardcoded C-g must not appear when prefix is C-Space:\n{text:?}"
    );

    // Default prefix (no setter) must still show C-g.
    let state_default = crate::state::State::default();
    let text_default = state_default.chrome.hint_bar_text(200, &state_default);
    assert!(
        text_default.contains("C-g"),
        "default prefix C-g must appear in hint_bar:\n{text_default:?}"
    );
}

#[tokio::test]
async fn the_selected_card_is_painted_in_the_terminals_own_reverse_video() {
    // With no `[ui] selection-style` set, xmux picks no colour for the selection at all:
    // the row carries REVERSED and the terminal swaps its own pair, so the selection is
    // as legible as that theme's text on every theme. Both the fg and the bg are pinned
    // to Reset, which is what stops the swap from striping the row in the card's level
    // colours cell by cell.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await; // step onto local/editor's card
    let sel = h.nav_row_of("editor").expect("editor row");
    let other = h.nav_row_of("inference").expect("inference row");
    let cell = h.buf()[(4, sel)].clone();
    assert!(
        cell.modifier.contains(Modifier::REVERSED),
        "the selected row inverts: {cell:?}"
    );
    assert_eq!(cell.fg, Color::Reset, "no colour of xmux's own under it");
    assert_eq!(cell.bg, Color::Reset, "nor behind it");
    assert!(
        !h.buf()[(4, other)].modifier.contains(Modifier::REVERSED),
        "and only that row inverts: {other}"
    );
    assert_eq!(
        h.buf()[(CARD_INDENT, sel)].symbol(),
        super::render::SELECTED_MARK,
        "the selection mark stands in the selected card's address column"
    );
}

#[tokio::test]
async fn filter_narrows() {
    let mut h = Harness::new(sample());
    h.ch('/').await;
    for c in "infer".chars() {
        h.ch(c).await;
    }
    h.key(KeyCode::Enter).await;
    let out = h.text();
    assert!(
        out.contains("inference"),
        "filter should keep inference:\n{out}"
    );
    assert!(
        !out.contains("editor"),
        "filter should drop non-matches:\n{out}"
    );
    assert!(
        !out.contains("build"),
        "filter should drop non-matches:\n{out}"
    );
    assert!(
        out.contains("filter: infer"),
        "active filter shows in title:\n{out}"
    );
}

#[tokio::test]
async fn create_adds_and_selects() {
    // A reachable empty host shows a host card; n on it creates a session, then selects it.
    let scan = Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let mut h = Harness::new(scan);
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"),
        "the lone empty host card is auto-selected"
    );
    h.ch('n').await; // n on a host card ⇒ create a session
    h.sw.set_input_text("scratch", &mut h.state);
    h.key(KeyCode::Enter).await;
    assert_eq!(*h.ops.created.lock().unwrap(), vec!["local/scratch"]);
    assert_eq!(cur_session_name(&h).as_deref(), Some("scratch"));
}

#[tokio::test]
async fn slow_op_is_deferred_off_the_key_path() {
    // The key-handling path must NOT perform the network create (which would
    // freeze the UI on a slow remote); it only queues the op for the loop.
    let scan = Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let mut h = Harness::new(scan); // the lone empty host card is auto-selected
    h.ch('n').await; // open New (create a session) on local
    h.sw.set_input_text("scratch", &mut h.state);
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    ); // raw: not pumped
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "create must be deferred off the key path, not run inline"
    );
    let op = only_run_op(cmds).expect("a create was queued for the loop");
    let r = run_op(&op, &h.ops).await;
    assert_eq!(
        h.ops.created.lock().unwrap().len(),
        1,
        "the op runs only when the loop pumps it"
    );
    h.sw.apply_op_result(r, &mut h.state);
    assert!(
        h.sw.row_of_session(&crate::session::Address::new("local", "scratch"))
            .is_some(),
        "applying the result folds the new session into the tree"
    );
}

#[tokio::test]
async fn n_on_a_session_card_opens_new_for_its_host() {
    // `n` starts a new SESSION on the selected card's host/mux. A session card
    // names its source, so `n` there opens the create input seeded with it rather
    // than refusing - you can add a session to a host that already has sessions.
    let mut h = Harness::new(sample());
    assert!(h
        .sw
        .select_address(&crate::session::Address::new("local", "editor"), &h.state));
    h.ch('n').await;
    assert!(
        h.state.is_inputting(),
        "the new-session input opens on a session card: {}",
        h.text()
    );
    match &h.state.modal {
        Some(Modal::Input(i)) => {
            assert!(matches!(i.mode, InputMode::New), "new-session mode");
            assert_eq!(
                i.source.as_deref(),
                Some("local"),
                "seeded with the selected card's source"
            );
        }
        _ => panic!("expected a New input modal"),
    }
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "nothing is created yet"
    );
}

#[tokio::test]
async fn filter_leaves_cursor_on_visible_session() {
    // Filter to a session - selection must land on it once the filter is in effect.
    // The filter applies live while the input is open (set_input_text applies it as a
    // real edit would), so Enter only closes it.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "live", 2, true),
            sess("local", "xmux-probeL", 1, false),
        ],
        None,
        &mut h.state,
    );
    h.ch('/').await;
    h.sw.set_input_text("probeL", &mut h.state);
    h.key(KeyCode::Enter).await; // close the input
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("a session row is visible");
    assert_eq!(
        t.target.as_str(),
        "xmux-probeL",
        "selection on filtered session"
    );
}

#[tokio::test]
async fn filter_host_enter_targets_visible_session() {
    // Under the filter the top card is the visible (matching) session, not a
    // filtered-out one - so current_attach_target yields it. The filter is in effect
    // while the input is open; Enter only closes it.
    let mut h = Harness::from_sources(&["alpha"]);
    h.sw.apply_source_result(
        "alpha".into(),
        vec![
            sess("alpha", "keep-me", 1, false),
            sess("alpha", "other", 1, false),
        ],
        None,
        &mut h.state,
    );
    h.ch('/').await;
    h.sw.set_input_text("keep", &mut h.state);
    h.key(KeyCode::Enter).await; // close the input
    h.key(KeyCode::Home).await; // the first (only) visible card
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("a visible session card is present");
    assert_eq!(
        t.target.as_str(),
        "keep-me",
        "current_attach_target under the filter yields the visible session"
    );
}

#[tokio::test]
async fn filter_applies_live_while_typing() {
    // The list re-filters on every keystroke, before any Enter: typing "in" narrows
    // the cards to the one match, and the active filter follows the buffer.
    let mut h = Harness::new(sample());
    h.ch('/').await;
    h.ch('i').await;
    assert!(h.state.is_inputting(), "the input stays open while typing");
    h.ch('n').await;
    let out = h.nav_cards_text();
    assert!(out.contains("inference"), "the matching card stays:\n{out}");
    assert!(
        !out.contains("editor") && !out.contains("build"),
        "the non-matching cards are gone before Enter:\n{out}"
    );
    assert_eq!(h.state.filter, "in", "the active filter follows the buffer");
    // Enter only closes; the filtered list is already in effect.
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting(), "Enter closes the input");
    assert_eq!(h.state.filter, "in", "Enter applies nothing new");
}

#[tokio::test]
async fn filter_esc_restores_the_opening_filter() {
    // The input remembers the filter it opened from, so cancelling undoes every live
    // edit back to it - the list returns to exactly the state it was in before `/`.
    let mut h = Harness::new(sample());
    // Establish a filter first.
    h.ch('/').await;
    for c in "infer".chars() {
        h.ch(c).await;
    }
    h.key(KeyCode::Enter).await;
    assert_eq!(h.state.filter, "infer", "the first filter is applied");
    let filtered_rows = h.sw.rows.len();
    assert!(
        filtered_rows < 6,
        "the filter narrows the list: {filtered_rows}"
    );
    // Reopen, edit the filter, then cancel: Esc restores the opening filter.
    h.ch('/').await;
    h.ch('x').await; // "inferx" matches nothing
    assert_eq!(h.state.filter, "inferx", "the live filter follows the edit");
    h.key(KeyCode::Esc).await;
    assert!(!h.state.is_inputting(), "Esc closes the input");
    assert_eq!(
        h.state.filter, "infer",
        "Esc restores the filter the input opened with"
    );
    assert_eq!(
        h.sw.rows.len(),
        filtered_rows,
        "and the list returns with it"
    );
}

#[tokio::test]
async fn nav_esc_clears_an_applied_filter() {
    let mut h = Harness::new(sample());
    let all_rows = h.sw.rows.len();
    h.ch('/').await;
    for c in "infer".chars() {
        h.ch(c).await;
    }
    h.key(KeyCode::Enter).await;
    assert_eq!(h.state.filter, "infer", "the filter is applied");
    assert!(h.sw.rows.len() < all_rows, "the filter narrows the list");
    h.key(KeyCode::Esc).await;
    assert!(
        h.state.filter.is_empty(),
        "Esc in the nav clears the filter"
    );
    assert_eq!(h.sw.rows.len(), all_rows, "the full list returns");
    h.key(KeyCode::Esc).await;
    assert!(h.state.filter.is_empty(), "Esc with no filter is a no-op");
    assert_eq!(h.sw.rows.len(), all_rows);
}

#[tokio::test]
async fn filter_keeps_the_selection_on_a_surviving_card_while_typing() {
    // As the live filter shrinks the list, the selection never sits on a card that
    // just filtered out: it holds its session while that survives, then lands on the
    // first card the narrower list still shows. The selection starts on build (the
    // first card in name-sorted order).
    let mut h = Harness::new(sample());
    h.ch('/').await;
    h.ch('i').await; // keeps build, editor, inference - the selection's session survives
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.name == "build"
        ),
        "the selection holds its card while it survives"
    );
    h.ch('n').await; // "in" keeps only inference
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.name == "inference"
        ),
        "a filtered-out card is never the selection; it lands on the survivor"
    );
}

#[tokio::test]
async fn create_on_unreachable_host_refused() {
    let mut h = Harness::new(sample());
    // jump to the last host row - the unreachable db-2.
    h.key(KeyCode::End).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Host {
                unreachable: true,
                ..
            })
        ),
        "expected to reach the unreachable db-2 host"
    );
    h.ch('n').await;
    assert!(
        h.state.chrome.flash.to_lowercase().contains("unreachable"),
        "create on unreachable host should flash unreachable, got {:?}",
        h.state.chrome.flash
    );
    assert!(h.ops.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_reachable_host_shows_its_host_screen() {
    // A reachable host with no sessions renders its host screen (the name, the state, the
    // keys that apply) in the terminal view, not a blank grid.
    let scan = Scan {
        groups: vec![Group {
            source: "fresh".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let h = Harness::new(scan);
    // The lone selectable row is that empty host, so it is auto-selected.
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "fresh"),
        "selection is on the empty host row"
    );
    let view = h.view_text();
    assert!(
        view.contains("fresh"),
        "the screen is headed by the host's name:\n{view}"
    );
    assert!(
        view.contains("no sessions"),
        "under it, the same state word its card carries:\n{view}"
    );
    assert!(
        view.contains("start a new session"),
        "then the key that answers that state:\n{view}"
    );
}

#[tokio::test]
async fn host_with_sessions_has_no_host_screen() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // the top card - a session of a host that HAS sessions
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "local"),
        "the top card is a session of a reachable host with sessions"
    );
    assert!(
        !h.view_text().contains("start a new session"),
        "a host with sessions must not show a host screen"
    );
}

#[tokio::test]
async fn both_host_screens_share_one_grammar() {
    // The unreachable screen and the empty screen are ONE screen in two states, so what
    // is pinned here is the SHAPE both hold to, not either one's words: the name as the
    // headline, the state word under it, one rule column for every row that carries a
    // cell, and the rescan key both offer. A state added later has this to answer to.
    let mut dead = Harness::from_sources(&["prod"]);
    dead.state.chrome.set_login_defaults(
        Default::default(),
        std::collections::HashMap::from([(
            "prod".into(),
            "Host prod\n    HostName 10.0.0.1\n".into(),
        )]),
    );
    dead.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut dead.state,
    );
    dead.draw();
    let empty = Harness::new(Scan {
        groups: vec![Group {
            source: "fresh".into(),
            err: None,
            sessions: vec![],
        }],
    });
    for (label, view, name, word) in [
        ("unreachable", dead.view_text(), "prod", "unreachable"),
        ("empty", empty.view_text(), "fresh", "no sessions"),
    ] {
        let lines: Vec<&str> = view.lines().collect();
        assert_eq!(lines[0].trim(), "", "{label}: opens on a blank row");
        assert_eq!(
            lines[1].trim_end(),
            format!(" {name}"),
            "{label}: the host name is the headline"
        );
        assert_eq!(
            lines[2].trim_end(),
            format!(" {word}"),
            "{label}: its state word sits under the name"
        );
        assert_eq!(
            lines[3].trim(),
            "",
            "{label}: a blank row parts the header from the rows"
        );
        let rules: Vec<usize> = lines.iter().filter_map(|l| l.find('│')).collect();
        assert!(
            !rules.is_empty() && rules.iter().all(|c| *c == rules[0]),
            "{label}: every row meets one rule column, got {rules:?}"
        );
        assert!(
            view.contains("re-scan every host"),
            "{label}: both screens offer the rescan key:\n{view}"
        );
    }
}

#[tokio::test]
async fn levels_render_from_the_switchers_palette() {
    // The selection parks on a remote card so the local rows render UNSELECTED: the
    // section title reads dim, in the decoration role, the session name in the accent.
    let mut h = Harness::new(sample());
    h.sw.set_palette(crate::ui::palette::resolve(
        "auto-light",
        crate::ui::palette::Overrides::default(),
    ));
    assert!(h.sw.select_address(
        &crate::session::Address::new("jupiter00", "inference"),
        &h.state
    ));
    h.draw();
    assert_eq!(
        h.nav_fg_of("local"),
        Some(h.sw.palette().decoration),
        "the section title is dim"
    );
    assert_eq!(
        h.nav_fg_of("editor"),
        Some(h.sw.palette().accent),
        "the session name is the accent target"
    );
}

#[tokio::test]
async fn the_session_reads_bold_on_its_card() {
    // The session - the level a user actually picks - is the one element that leaves
    // the text colour, and it is BOLD; the host and mux on the context line stay plain
    // text so the session remains the detail line's anchor.
    let h = Harness::new(sample());
    assert!(
        h.nav_mod_of("editor").unwrap().contains(Modifier::BOLD),
        "the session reads bold on its card"
    );
    assert!(
        !h.nav_mod_of("local").unwrap().contains(Modifier::BOLD),
        "the host stays plain"
    );
}

/// A session stamped with its mux kind, for the context-line tests.
fn sess_mux(source: &str, name: &str, mux: &str) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        mux: mux.into(),
        windows: 1,
        attached: false,
    }
}

/// One host carrying `sessions`.
fn one_host_scan(source: &str, sessions: Vec<Session>) -> Scan {
    Scan {
        groups: vec![Group {
            source: source.into(),
            err: None,
            sessions,
        }],
    }
}

/// Several sources, each carrying `sessions`.
fn sources_scan(sources: Vec<(&str, Vec<Session>)>) -> Scan {
    let groups = sources
        .into_iter()
        .map(|(source, sessions)| Group {
            source: source.into(),
            err: None,
            sessions,
        })
        .collect();
    Scan { groups }
}

#[tokio::test]
async fn a_sources_cards_are_contiguous_and_the_order_is_deterministic() {
    // A source's cards sit together under their source's one section title: alpha's
    // before beta's, never interleaved with another host's, and inside each host the
    // name order holds (a-new, then a-old).
    let mut h = Harness::new(sources_scan(vec![
        (
            "alpha",
            vec![
                sess_mux("alpha", "a-old", "tmux"),
                sess_mux("alpha", "a-new", "tmux"),
            ],
        ),
        (
            "beta",
            vec![
                sess_mux("beta", "b-new", "tmux"),
                sess_mux("beta", "b-old", "tmux"),
            ],
        ),
    ]));
    // The app resolves every source's reach before its first frame; set it here so the
    // section titles name their mux exactly as the live app's do.
    h.state.chrome.set_source_reach(
        [
            ("alpha".to_string(), reach("tmux", "alpha", "", "tmux ls")),
            ("beta".to_string(), reach("tmux", "beta", "", "tmux ls")),
        ]
        .into_iter()
        .collect(),
    );
    h.sw.rebuild(&mut h.state);
    h.draw();
    let out = h.nav_text();
    let row = |name: &str| {
        h.nav_row_of(name).unwrap_or_else(|| {
            panic!(
                "{name}:
{out}"
            )
        })
    };
    let (a_new, a_old, b_new, b_old) = (row("a-new"), row("a-old"), row("b-new"), row("b-old"));
    assert!(
        a_new < a_old && a_old < b_new && b_new < b_old,
        "alpha then beta, each by name: a-new {a_new}, a-old {a_old}, b-new {b_new}, b-old {b_old}
{out}"
    );
    // One section title per source: the title names the whole group, the cards below
    // it carry the sessions alone.
    assert_eq!(
        out.matches("alpha/tmux").count(),
        1,
        "alpha named once:
{out}"
    );
    assert_eq!(
        out.matches("beta/tmux").count(),
        1,
        "beta named once:
{out}"
    );
}

#[tokio::test]
async fn a_session_found_later_lands_inside_its_own_source() {
    // The order is frozen once the hosts settle, so a session that appears afterwards
    // cannot be placed by re-sorting. It is inserted after the last card of its own
    // source - never appended to the bottom, which would strand it under another host's
    // context line and split the source it belongs to.
    let mut h = Harness::new(sources_scan(vec![
        ("alpha", vec![sess_mux("alpha", "a-one", "tmux")]),
        ("beta", vec![sess_mux("beta", "b-one", "tmux")]),
    ]));
    h.sw.apply_source_result(
        "alpha".into(),
        vec![
            sess_mux("alpha", "a-one", "tmux"),
            sess_mux("alpha", "a-two", "tmux"),
        ],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.nav_text();
    let row = |name: &str| {
        h.nav_row_of(name).unwrap_or_else(|| {
            panic!(
                "{name}:
{out}"
            )
        })
    };
    assert!(
        row("a-one") < row("a-two") && row("a-two") < row("b-one"),
        "the new session joins alpha's run, above beta:
{out}"
    );
}

#[tokio::test]
async fn the_section_title_shows_host_mux_and_the_session_takes_the_accent() {
    // The `{host}/{mux}` label lives on the SECTION TITLE, both halves in the quiet
    // header role; the session card under it is the name alone, the accent target.
    // The mux comes from the resolved reach, exactly as the app resolves every source
    // before its first frame.
    let mut h = Harness::new(selection_parked_elsewhere(one_host_scan(
        "srv",
        vec![sess_mux("srv", "alpha", "tmux")],
    )));
    h.state.chrome.set_source_reach(
        [("srv".to_string(), reach("tmux", "srv", "", "tmux ls"))]
            .into_iter()
            .collect(),
    );
    h.sw.rebuild(&mut h.state);
    h.draw();
    let out = h.nav_text();
    assert!(
        out.contains("srv/tmux"),
        "section title names the pair:\n{out}"
    );
    assert_eq!(
        h.nav_fg_of("srv"),
        Some(crate::ui::palette::Palette::default().decoration),
        "the section title is dim, host half"
    );
    assert_eq!(
        h.nav_fg_of("tmux"),
        Some(crate::ui::palette::Palette::default().decoration),
        "and mux half"
    );
    assert_eq!(
        h.nav_fg_of("alpha"),
        Some(crate::ui::palette::Palette::default().accent),
        "the session name is the accent target"
    );
}

#[tokio::test]
async fn a_section_title_stands_alone_over_its_cards() {
    // The dim title and the indent under it mark a group at every position, so the
    // title row and the card rows carry no rule or connector glyph.
    let side = Harness::new(sample());
    assert_eq!(side.plan.layout, ViewLayout::Column, "landscape → Side");
    let y = side.nav_row_of("local").expect("the section title");
    let painted = nav_line(&side, y);
    assert!(
        !painted.contains(BAND_RULE),
        "the side list's title stands alone:\n{painted}"
    );

    let top = Harness::new_sized(sample(), 60, 70);
    assert_eq!(top.plan.layout, ViewLayout::Band, "portrait → Top");
    let w = top.buf().area.width;
    let y = row_of(top.buf(), "local", w).expect("the section title");
    let painted = band_line(&top, y);
    assert!(
        !painted.contains(BAND_RULE),
        "the band's title stands alone:\n{painted}"
    );
    for name in ["build", "editor"] {
        let painted = band_line(&top, row_of(top.buf(), name, w).expect(name));
        assert!(
            painted.starts_with("  "),
            "{name} is indented under its title with nothing in the indent:\n{painted}"
        );
    }
}

#[tokio::test]
async fn a_split_sections_cards_read_at_one_offset_in_every_column() {
    // Every card of a section reads at one offset INSIDE its column whichever column it
    // landed in, a continuation included. Measured against the rect the plan recorded,
    // since the columns start wherever the widths put them.
    let h = Harness::new_sized(scan_with_sessions(10), 60, 12);
    assert_eq!(h.plan.layout, ViewLayout::Band, "portrait → Top");
    let w = h.buf().area.width;
    let (s0, _) = locate(h.buf(), "s0", w).expect("s0");
    let (s5, _) = locate(h.buf(), "s5", w).expect("s5");
    let title_x = locate(h.buf(), "local", w).expect("the title").0;
    let repeat_x = locate(h.buf(), "local …", w).expect("the repeated title").0;
    assert_eq!(
        s0 - title_x,
        s5 - repeat_x,
        "a continuation's card reads at the same offset under its repeated title"
    );
}

#[tokio::test]
async fn the_selections_inversion_stops_at_the_card_and_spares_the_indent() {
    // The indent is the title's, not the card's. The selection paints a card by
    // inverting its whole rect, so the rect starts past the indent and the indent stays
    // blank on the selected card's row.
    let h = Harness::new_sized(sample(), 60, 70);
    assert_eq!(h.plan.layout, ViewLayout::Band, "portrait → Top");
    let sel = h.sw.selected;
    let (_, rect) = h
        .plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == sel)
        .expect("the selected card's rect");
    assert!(
        rect.x >= CARD_INDENT,
        "the selected card is a session card, which stands past the indent"
    );
    let buf = h.buf();
    assert!(
        buf[(rect.x, rect.y)].modifier.contains(Modifier::REVERSED),
        "the card itself is painted in the terminal's own reverse video"
    );
    let strip = &buf[(rect.x - CARD_INDENT, rect.y)];
    assert_eq!(strip.symbol(), " ", "the indent is blank");
    assert!(
        !strip.modifier.contains(Modifier::REVERSED),
        "and the inversion does not reach it"
    );
}

#[tokio::test]
async fn a_split_sections_continuation_columns_repeat_the_title() {
    // Only a section taller than a whole column splits. The continuation keeps its
    // column's top row for the title, dim and followed by `…`, so a column read alone
    // still says whose cards it holds; its cards start on the row under it.
    let h = Harness::new_sized(scan_with_sessions(10), 60, 12);
    assert_eq!(h.plan.layout, ViewLayout::Band, "portrait → Top");
    let band = h.plan.nav_inner;
    let painted: String = (band.y..band.y + band.height)
        .map(|y| band_line(&h, y))
        .collect::<Vec<_>>()
        .join("\n");
    let first_row = band_line(&h, band.y);
    assert_eq!(
        first_row.matches("local").count(),
        first_row.matches("local …").count() + 1,
        "the title once, then a repeat over every continuation:\n{painted}"
    );
    let cells = cells_of(&h.plan);
    let split = (1..cells.len())
        .find(|i| cells[i].x > cells[&(i - 1)].x)
        .expect("the section really did split");
    assert_eq!(
        cells[&split].y,
        band.y + 1,
        "the continuation's first card hangs under the repeated title"
    );
}
#[tokio::test]
async fn a_column_is_never_narrower_than_the_title_naming_it() {
    // A column is as wide as the WIDEST thing in it, and the section title is one of
    // those things. Sessions named in one character must therefore not shrink the column
    // under the `{host}/{mux}` above them: the title is the only row saying where the
    // cards are, and a host cut in half names a machine that does not exist. It holds
    // its one row while doing it - the fix is the column's width, never a second row.
    let mut h = Harness::new_sized(
        sources_scan(vec![
            (
                "build-runner-eu-west",
                vec![sess_mux("build-runner-eu-west", "z", "zellij")],
            ),
            ("jupiter00", vec![sess_mux("jupiter00", "a", "tmux")]),
        ]),
        60,
        12,
    );
    h.state.chrome.set_source_reach(
        [
            (
                "build-runner-eu-west".to_string(),
                reach("zellij", "build-runner-eu-west", "", "zellij ls"),
            ),
            (
                "jupiter00".to_string(),
                reach("tmux", "jupiter00", "", "tmux ls"),
            ),
        ]
        .into_iter()
        .collect(),
    );
    h.sw.rebuild(&mut h.state);
    h.draw();
    assert_eq!(h.plan.layout, ViewLayout::Band, "portrait → Top");
    let band = h.plan.nav_inner;
    let painted: String = (band.y..band.y + band.height)
        .map(|y| band_line(&h, y))
        .collect::<Vec<_>>()
        .join("\n");
    for title in ["build-runner-eu-west/zellij", "jupiter00/tmux"] {
        assert!(
            painted.contains(title),
            "the one-character session did not shrink the column under {title}:\n{painted}"
        );
    }
    // On its own row, whole: the title never carries onto a second one.
    let cells = cells_of(&h.plan);
    assert_eq!(cells[&0].height, 1, "the title is one row");
    assert_eq!(
        cells[&1].y,
        cells[&0].y + 1,
        "and its session hangs directly under it"
    );
    assert!(
        cells[&0].width >= "build-runner-eu-west/zellij".len() as u16,
        "the column carries the whole title: {:?}",
        cells[&0]
    );
}

#[tokio::test]
async fn a_host_card_gives_its_mux_the_secondary() {
    // A host-state card has no session to take the accent, so its mux - the lowest
    // level it displays - stays with the host half; both read in the secondary role.
    // The separator keeps its own furniture role.
    let scan = Scan {
        groups: vec![
            Group {
                source: "srv:zellij".into(),
                err: None,
                sessions: vec![sess_mux("srv:zellij", "alpha", "zellij")],
            },
            // A reachable machine with no session left: the host-state card.
            Group {
                source: "srv:psmux".into(),
                err: None,
                sessions: vec![],
            },
        ],
    };
    let h = Harness::new(scan);
    let out = h.nav_text();
    assert!(
        out.contains("srv/psmux"),
        "the host card names its mux:
{out}"
    );
    assert_eq!(
        h.nav_fg_of("psmux"),
        Some(crate::ui::palette::Palette::default().secondary),
        "the mux shares the host half's secondary role"
    );
    // The separator is furniture on both card kinds, and the host half is secondary.
    let (x, y) = locate(h.buf(), "srv/psmux", NAV_WIDTH).expect("the host card");
    assert_eq!(
        h.buf()[(x, y)].fg,
        crate::ui::palette::Palette::default().secondary,
        "the host half"
    );
    assert_eq!(
        h.buf()[(x + 3, y)].fg,
        crate::ui::palette::Palette::default().decoration,
        "the separator is its own role"
    );
}

/// A scan holding `n` sessions on one reachable source plus one unreachable source, so
/// the nav has both of its bands: session cards over a host-state card.
fn scan_with_bands(n: usize) -> Scan {
    let mut scan = scan_with_sessions(n);
    scan.groups.push(Group {
        source: "db-2".into(),
        err: Some("connection timed out".into()),
        sessions: vec![],
    });
    scan
}

/// The rect the paint gave card `idx`.
fn card_rect(h: &Harness, idx: usize) -> Rect {
    h.plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == idx)
        .map(|(_, r)| *r)
        .expect("the card was drawn")
}

#[tokio::test]
async fn every_host_state_card_sits_below_every_session_card() {
    // The dead host is FIRST in group order, and its card still lands last: a host with
    // no session to show is the tail of the list, whatever order the hosts were scanned in.
    let mut groups = vec![Group {
        source: "db-2".into(),
        err: Some("connection timed out".into()),
        sessions: vec![],
    }];
    groups.extend(sample().groups.into_iter().filter(|g| g.err.is_none()));
    let h = Harness::new(Scan { groups });
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    assert!(boundary > 0, "the session cards come first");
    for (i, row) in h.sw.rows.iter().enumerate() {
        let host_card = matches!(row.reference, RowRef::Host { .. });
        assert_eq!(
            host_card,
            i >= boundary,
            "card {i} is on the wrong side of the boundary"
        );
    }
}

#[tokio::test]
async fn the_bands_part_with_the_rows_left_over() {
    // Both bands fit, so the parting is the blank rows between them: the session cards
    // hold the top edge and the host-state card is pushed to the bottom.
    let h = Harness::new(sample());
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    let host = card_rect(&h, boundary);
    let last_session = card_rect(&h, boundary - 1);
    let region = h.plan.nav_inner;
    assert_eq!(
        host.y + host.height,
        region.y + region.height,
        "the host-state band ends on the region's bottom edge"
    );
    assert!(
        host.y > last_session.y + last_session.height,
        "blank rows part the two bands"
    );
    for y in (last_session.y + last_session.height)..host.y {
        assert_eq!(
            nav_line(&h, y).trim(),
            "",
            "the parting is blank, not a rule, while both bands fit"
        );
    }
}

#[tokio::test]
async fn a_scrolling_list_parts_its_bands_with_a_rule() {
    // Too many cards to fit: the gap would part what is no longer on screen together, so
    // the list closes it up and a box-drawing rule takes the boundary's row instead.
    let mut h = Harness::new(scan_with_bands(40));
    h.key(KeyCode::End).await; // scroll down to the boundary
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    let host = card_rect(&h, boundary);
    assert!(host.y > 0, "the rule needs a row above the host card");
    // Across the CARDS' width: the seam beside them carries the thumb.
    let rule: String = (host.x..host.x + host.width)
        .map(|x| h.buf()[(x, host.y - 1)].symbol().to_string())
        .collect();
    assert!(
        rule.chars().all(|c| c.to_string() == BAND_RULE),
        "a rule sits directly above the host-state band: {rule:?}"
    );
    let last_session = card_rect(&h, boundary - 1);
    assert_eq!(
        last_session.y + last_session.height,
        host.y - 1,
        "the rule is the only thing between the bands"
    );
}

#[tokio::test]
async fn the_bands_never_touch_on_screen() {
    // 27 session cards plus a still-scanning host card fill this nav's rows exactly
    // (every card is one row now), so the parting has no row left to take: rather than
    // let the bands meet, the list scrolls a row early and the rule takes the boundary's
    // row. Scroll to the boundary first - the host sits below the fold at the top.
    let mut h = Harness::from_sources(&["local", "db-2"]);
    let sessions: Vec<crate::session::Session> = (0..27)
        .map(|i| sess("local", &format!("s{i}"), 1, false))
        .collect();
    h.sw.apply_source_result("local".into(), sessions, None, &mut h.state);
    h.draw();
    let cards: u16 = h.sw.rows.len() as u16;
    assert_eq!(
        cards, h.plan.nav_inner.height,
        "the precondition: the cards alone fill the region exactly"
    );
    h.key(KeyCode::End).await; // scroll down to the boundary
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    let host = card_rect(&h, boundary);
    let last_session = card_rect(&h, boundary - 1);
    assert_eq!(
        last_session.y + last_session.height,
        host.y - 1,
        "a row stands between the bands"
    );
    let rule: String = (host.x..host.x + host.width)
        .map(|x| h.buf()[(x, host.y - 1)].symbol().to_string())
        .collect();
    assert!(
        rule.chars().all(|c| c.to_string() == BAND_RULE),
        "and that row is the rule: {rule:?}"
    );
    // The list scrolls a row before the cards themselves would need it, so the seam
    // carries the thumb.
    let seam_x = h.plan.regions.view_border.x;
    assert!(
        (h.plan.nav_inner.y..h.plan.nav_inner.y + h.plan.nav_inner.height)
            .any(|y| h.buf()[(seam_x, y)].symbol() == "┃"),
        "the seam thumb is drawn"
    );
}

#[tokio::test]
async fn scanning_hosts_anchor_to_the_bottom_until_found() {
    // Before ANY session is found, every host is a scanning card and the nav is the
    // host band ALONE: it anchors to the BOTTOM, the blank rows above it being where
    // the sessions that will be found land.
    let h = Harness::from_sources(&["local", "jupiter00"]);
    let txt = h.nav_cards_text();
    let rows: Vec<&str> = txt.lines().collect();
    let region_bottom = (h.plan.nav_inner.y + h.plan.nav_inner.height) as usize;
    let card_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        card_rows,
        vec![region_bottom - 2, region_bottom - 1],
        "both scanning hosts sit on the bottom edge:\n{card_rows:?}"
    );
    assert!(
        card_rows[0] > 0,
        "blank rows stand above them, where found sessions will land"
    );
    // One source resolves: its section and cards MOVE to the top, the other stays below.
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    let section =
        h.sw.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Section { .. }))
            .expect("the resolved source gains a section title");
    assert_eq!(card_rect(&h, section).y, 0, "the section leads the list");
    assert_eq!(card_rect(&h, section + 1).y, 1, "its card follows");
    let host =
        h.sw.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Host { .. }))
            .expect("the still-scanning host keeps a card");
    assert!(
        card_rect(&h, host).y > card_rect(&h, section + 1).y,
        "the undiscovered host stays below the found session"
    );
}

#[tokio::test]
async fn a_click_on_the_parting_selects_nothing() {
    let mut h = Harness::new(sample());
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    let before = h.sw.selected;
    let gap_y = card_rect(&h, boundary - 1);
    let gap_y = gap_y.y + gap_y.height;
    h.sw.mouse_select(&h.plan, h.plan.nav_inner.x, gap_y, &h.state);
    assert_eq!(
        h.sw.selected, before,
        "the blank parting is not a card, so a click on it moves nothing"
    );
}

/// The full content of screen row `y`, across the nav width.
fn nav_line(h: &Harness, y: u16) -> String {
    (0..NAV_WIDTH.min(h.buf().area.width))
        .map(|x| h.buf()[(x, y)].symbol().to_string())
        .collect()
}

/// One screen row read across the WHOLE window - what the portrait band needs, whose
/// nav is a wide strip rather than the side list's column.
fn band_line(h: &Harness, y: u16) -> String {
    (0..h.buf().area.width)
        .map(|x| h.buf()[(x, y)].symbol().to_string())
        .collect()
}

#[tokio::test]
async fn a_sources_sessions_are_each_a_single_row_under_one_section_title() {
    // Every session of one source is a single-row card (the number + the name), stacked
    // directly under the one section title that names the whole group. The side list
    // draws no connector and no per-card context line: it is one full-width run, where
    // the title and the rule under it already draw the group.
    let mut h = Harness::new(one_host_scan(
        "srv",
        vec![
            sess_mux("srv", "alpha", "tmux"),
            sess_mux("srv", "beta", "tmux"),
            sess_mux("srv", "gamma", "tmux"),
            sess_mux("srv", "zeta", "tmux"),
        ],
    ));
    h.state.chrome.set_source_reach(
        [("srv".to_string(), reach("tmux", "srv", "", "tmux ls"))]
            .into_iter()
            .collect(),
    );
    h.sw.rebuild(&mut h.state);
    h.draw();
    let out = h.nav_text();
    assert!(
        out.contains("srv/tmux"),
        "the section title names the group:\n{out}"
    );
    let title_row = h.nav_row_of("srv").expect("the section title");
    for (k, name) in ["alpha", "beta", "gamma", "zeta"].iter().enumerate() {
        let r = h.nav_row_of(name).expect(name);
        assert_eq!(
            r,
            title_row + 1 + k as u16,
            "{name} is a single row directly under the title"
        );
        assert!(
            !nav_line(&h, r).contains("srv") && !nav_line(&h, r).contains("tmux"),
            "the session row is the name alone: {:?}",
            nav_line(&h, r)
        );
    }
    assert!(
        !out.contains("├") && !out.contains("└") && !out.contains('│'),
        "no connector draws a group the title already draws:\n{out}"
    );
}

#[tokio::test]
async fn focus_changes_only_the_address_column() {
    // Focus does NOT expand a card: the selection landing on a session card leaves its
    // row count and its content untouched, and only the address column changes - the
    // number becomes the selection mark. The section title never takes the mark.
    let mut h = Harness::new(one_host_scan(
        "srv",
        vec![
            sess_mux("srv", "alpha", "tmux"),
            sess_mux("srv", "beta", "tmux"),
        ],
    ));
    let beta_row = h.nav_row_of("beta").expect("beta detail");
    assert_eq!(
        beta_row, 2,
        "beta is a one-row card under the title and alpha"
    );
    h.key(KeyCode::Down).await; // select beta
    assert_eq!(
        h.nav_row_of("beta"),
        Some(beta_row),
        "selecting beta does not move or expand it"
    );
    // The mark stands in the address column, on the same row that carries the session.
    assert_eq!(
        h.buf()[(CARD_INDENT, beta_row)].symbol(),
        super::render::SELECTED_MARK,
        "the selection mark replaces the number in the address column"
    );
    // Nothing above beta changed: no context line grew, the title row is untouched.
    assert_eq!(
        h.nav_row_of("alpha"),
        Some(beta_row - 1),
        "the card above stays where it was"
    );
    assert!(
        nav_line(&h, 0).contains("srv"),
        "the section title row is untouched by the selection"
    );
    h.key(KeyCode::Up).await; // move off
    assert_eq!(
        h.nav_row_of("beta"),
        Some(beta_row),
        "unselected, beta keeps its one row - no collapse, no expansion"
    );
}

#[tokio::test]
async fn navigation_wraps_around() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::End).await; // last card = db-2 host
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"));
    h.key(KeyCode::Down).await; // wrap bottom → first SESSION card (row 1, under its title)
    assert_eq!(
        h.sw.selected, 1,
        "↓ from the last card wraps to the first session card"
    );
    h.key(KeyCode::Up).await; // wrap top → bottom
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"));
}

#[tokio::test]
async fn horizontal_steps_one_host_and_lands_on_its_first_card() {
    // ↑/↓ and ←/→ name the two things the list is made of. ←/→ cross a whole category
    // at a time: from a session of one source the selection lands on the FIRST card of
    // the next, so a list of many hosts is crossed without stepping over every session
    // between them. The host band is the last category, entered at its first card.
    // (`sample`: local holds two sessions, jupiter00 one, db-2 is unreachable.)
    let mut h = Harness::new(sample());
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "local"),
        "the launch cursor is a local session card"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "jupiter00" && sess.name == "inference"
        ),
        "→ lands on the next source's first session"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"),
        "the host band is entered at its first card"
    );
}

#[tokio::test]
async fn the_host_band_is_one_stop_however_many_cards_it_holds() {
    // The sources with nothing to show are ONE category to ←/→, not one each: a
    // list of machines with nothing running on them is a single thing to reach past
    // rather than a run of places to be carried into one at a time. Every one of them is
    // still a card, so ↑/↓ reach each.
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await; // local → jupiter00
    h.key(KeyCode::Right).await; // jupiter00 → the band
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"),
        "→ enters the band at its first card"
    );
    h.key(KeyCode::Down).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-3"),
        "↓ still walks the band card by card"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "local"),
        "→ crosses the whole band in one step, from any card in it"
    );
}

#[tokio::test]
async fn leaving_the_host_band_backwards_lands_on_the_last_source_with_sessions() {
    // The band is left the same way in either direction, and from any card in it: ← from
    // its second card returns to the source before it, not to its own first card.
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await; // local → jupiter00
    h.key(KeyCode::Right).await; // jupiter00 → the band
    h.key(KeyCode::Down).await; // the band's second card
    h.key(KeyCode::Left).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "jupiter00"
        ),
        "← leaves the band for the source before it"
    );
}

#[tokio::test]
async fn horizontal_leaves_the_host_from_any_of_its_cards() {
    // The step is by SOURCE, not by card: it leaves the host the selection is on
    // wherever inside that host the selection sits. Stepping to the next CARD from the
    // last session of a source would look the same from that one card alone, so the
    // selection is moved off the first card of a two-session source first.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.source == "local" && sess.name == "editor"
        ),
        "the second local session"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "jupiter00"),
        "→ leaves the source from a card that is not its last"
    );
}

#[tokio::test]
async fn horizontal_wraps_at_both_ends() {
    // The category step wraps exactly as the card step does, so neither end of the list
    // is a dead stop.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Left).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"),
        "← from the first source wraps to the last"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.source == "local"),
        "→ from the last source wraps to the first"
    );
}

#[tokio::test]
async fn double_click_selects_node() {
    let mut h = Harness::new(sample());
    // inference preselected; double-click inside the tree moves the selection.
    let before = h.sw.selected;
    h.sw.mouse_attach(&h.plan, 5, 4, &h.state);
    // selection moved (or stayed on the same selectable row - just check no panic
    // and current_attach_target is populated).
    assert!(
        h.sw.current_attach_target(&h.state).is_some(),
        "double click yields an attach target"
    );
    let _ = before; // used
}

#[tokio::test]
async fn single_click_moves_cursor() {
    let mut h = Harness::new(sample());
    // Click the card the renderer actually drew at that screen row: card heights vary and
    // the bands are parted, so the hit-test must read the paint, not a fixed row pitch.
    let target = row_index(
        &h,
        |r| matches!(r, RowRef::Session { sess } if sess.name == "build"),
    );
    h.draw();
    let (x, y) = row_screen_pos(&h, target);
    h.sw.mouse_select(&h.plan, x, y, &h.state);
    assert_eq!(
        h.sw.selected, target,
        "a click lands on the card drawn at that row"
    );
}

#[tokio::test]
async fn mouse_hit_testing_reads_the_plan_produced_for_the_frame() {
    let mut h = Harness::new(sample());
    let target = row_index(
        &h,
        |r| matches!(r, RowRef::Session { sess } if sess.name == "build"),
    );
    let frame_plan = h.plan.clone();
    let (_, rect) = frame_plan
        .nav_cells
        .iter()
        .find(|(idx, _)| *idx == target)
        .expect("the target card is in the frame plan");
    h.sw.set_selected(0, &h.state);

    h.sw.mouse_select(&RenderPlan::default(), rect.x, rect.y, &h.state);
    assert_ne!(h.sw.selected, target, "a different plan has no card there");

    h.sw.mouse_select(&frame_plan, rect.x, rect.y, &h.state);
    assert_eq!(h.sw.selected, target, "the frame plan resolves the click");
}

/// The screen (col,row) of the card at `idx`: its FIRST screen row, read from the frame
/// plan - the same geometry the renderer and mouse hit-testing use.
fn row_screen_pos(h: &Harness, idx: usize) -> (u16, u16) {
    let (_, rect) = h
        .plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == idx)
        .expect("the card was drawn");
    (rect.x, rect.y)
}

fn row_index<F: Fn(&RowRef) -> bool>(h: &Harness, pred: F) -> usize {
    h.sw.rows
        .iter()
        .position(|r| pred(&r.reference))
        .expect("row exists")
}

#[tokio::test]
async fn help_overlay_renders_and_closes_on_q() {
    let mut h = Harness::new(sample());
    assert!(!h.text().contains("keys"), "help hidden initially");
    h.sw.show_help(&mut h.state); // driven by the app's `prefix ?`
    h.draw();
    let out = h.text();
    assert!(
        out.contains("keys"),
        "show_help opens the help modal:\n{out}"
    );
    assert!(out.contains("fuzzy filter"), "help should list keybindings");
    // Modal dismissal (tmux view-mode): the app routes keys to feed_reader_key
    // above the tree/terminal split - q closes it; other keys are swallowed (no nav).
    assert!(
        h.sw.feed_reader_key(b"q", &mut h.state),
        "q is consumed while help is open"
    );
    h.draw();
    assert!(
        !h.text().contains("fuzzy filter"),
        "q closes the help modal"
    );
}

#[tokio::test]
async fn terminal_view_target_follows_cursor() {
    let mut h = Harness::new(sample());
    // On a session card, the target is that session (its active window follows).
    assert!(h
        .sw
        .select_address(&crate::session::Address::new("local", "editor"), &h.state));
    let t = h.sw.terminal_view_target();
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    // Step to the next card (the next session) - the target follows the cursor.
    h.key(KeyCode::Down).await;
    let t = h.sw.terminal_view_target();
    assert_eq!(
        (t.source.as_str(), t.target.as_str()),
        ("jupiter00", "inference")
    );
}

#[tokio::test]
async fn render_terminal_view_draws_live_grid() {
    use crate::display::grid::Grid;
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await; // a normal non-xmux pane
    let mut g = Grid::new(28, 50);
    g.feed(b"LIVE-GRID-CONTENT");
    // Render with the live grid supplied.
    let sw = &mut h.sw;
    h.term
        .draw(|f| sw.render_test(f, Some(&g), false, NavSize::visible(NAV_WIDTH), &h.state))
        .unwrap();
    let out = buffer_text(h.term.backend().buffer());
    assert!(
        out.contains("LIVE-GRID-CONTENT"),
        "the terminal view renders the live grid's contents:\n{out}"
    );
}

#[test]
fn render_terminal_view_none_grid_is_blank_not_attaching() {
    // The "(attaching…)" placeholder is removed entirely. A None grid (only at
    // first launch, before any session is confirmed on screen) renders blank -
    // never the placeholder. The display keeps the last confirmed session until
    // the next is ready (stale-while-revalidate), so a transitional placeholder
    // has no purpose.
    let mut state = crate::state::State::from_sources(vec!["local".into(), "jupiter06".into()]);
    let sw = Switcher::from_sources(&mut state);
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| sw.render_test(f, None, true, NavSize::hidden(NAV_WIDTH), &state))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        !out.contains("attaching"),
        "no attaching placeholder when grid is None:\n{out}"
    );
}

// --- j/k nav, select=attach, spinner, hint_bar/help, title --------

fn cur_row_label(h: &Harness) -> String {
    h.sw.rows
        .get(h.sw.selected)
        .map(|r| match &r.reference {
            RowRef::Session { sess } => sess.address().display(),
            RowRef::Host { source, .. } | RowRef::Section { source, .. } => source.clone(),
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn j_k_navigate_like_arrows() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // the first card
    let at_top = cur_row_label(&h);
    h.ch('j').await; // down
    assert_ne!(cur_row_label(&h), at_top, "j moves the selection down");
    h.ch('k').await; // back up
    assert_eq!(cur_row_label(&h), at_top, "k moves the selection up");
}

#[tokio::test]
async fn enter_and_bare_q_are_noops() {
    // Enter is consumed by the app (focus the terminal), not the switcher; bare q does
    // nothing - quit is `prefix q` at the app level. Neither moves the selection or
    // opens an input here.
    let mut h = Harness::new(sample());
    let before = cur_row_label(&h);
    h.key(KeyCode::Enter).await;
    h.ch('q').await;
    assert!(!h.state.is_inputting(), "neither opens an input");
    assert_eq!(cur_row_label(&h), before, "neither moves the selection");
}

#[tokio::test]
async fn cursor_move_yields_attach_target() {
    let mut h = Harness::new(sample()); // launch on the first local session's card
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("a session card yields a target");
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "build"));
    h.key(KeyCode::Down).await; // ↓ to the next local session's card
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("still a target");
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
}

#[tokio::test]
async fn current_host_tracks_cursor_source() {
    // The app ensures this host on every move; every card yields its source, so the
    // host's tree can be fetched.
    let mut h = Harness::new(sample()); // launch on the first local session's card
    assert_eq!(h.sw.current_host().as_deref(), Some("local"));
    h.key(KeyCode::End).await; // jump to the last card (the db-2 host card)
    assert_eq!(h.sw.current_host().as_deref(), Some("db-2"));
}

#[test]
fn long_flash_wraps_in_narrow_hint_bar_instead_of_clipping() {
    // The hint_bar lives in the tree column; a long flash must wrap across lines rather
    // than clip at the column edge (a narrow tree would otherwise hide most of it).
    let mut state = crate::state::State::from_scan(sample());
    state.chrome.flash = "host unreachable, cannot create here".into();
    let lines = state.chrome.hint_bar_lines(20, &state);
    assert!(
        lines.len() > 1,
        "long flash wraps across lines, got {lines:?}"
    );
    assert!(
        lines.iter().all(|l| l.chars().count() <= 20),
        "every wrapped line fits the width, got {lines:?}"
    );
    let joined = lines.join("").replace("  ", " ");
    assert!(
        joined.contains("cannot create here"),
        "no text is lost: {joined:?}"
    );
}

#[test]
fn a_collapsed_nav_renders_every_wrapped_flash_line() {
    let mut state = crate::state::State::from_scan(sample());
    state.chrome.flash = "host unreachable, cannot create here".into();
    let sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(24, 8)).unwrap();
    let width = collapsed_nav_width("C-g");
    let nav = NavSize {
        natural: NAV_WIDTH,
        width,
        height: 0,
        position: NavPosition::Left,
        collapsed: true,
    };
    term.draw(|f| sw.render_test(f, None, false, nav, &state))
        .unwrap();

    let buf = term.backend().buffer();
    // The bar opens across the terminal view beside the collapsed column.
    let lines = state
        .chrome
        .hint_bar_lines(buf.area.width - width - 1, &state);
    assert!(lines.len() > 1);
    let first = buf.area.height - lines.len() as u16;
    let painted = (first..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ");
    for word in ["host", "unreachable,", "cannot", "create", "here"] {
        assert!(
            painted.contains(word),
            "all wrapped flash text remains visible: {painted:?}"
        );
    }
}

#[tokio::test]
async fn flash_clears_on_next_key_restoring_the_hint_bar() {
    // A flash (e.g. "host unreachable, cannot create here") is transient: any key
    // dismisses it so the normal help/status hint_bar returns. Regression: it persisted
    // because only the input-opening actions cleared it, so navigation never did.
    let mut h = Harness::new(sample());
    h.state.chrome.flash = "host unreachable, cannot create here".into();
    h.key(KeyCode::Down).await;
    assert!(
        h.state.chrome.flash.is_empty(),
        "navigation clears the flash, got {:?}",
        h.state.chrome.flash
    );
}

#[test]
fn hiding_the_nav_leaves_the_layout_where_it_was() {
    use ratatui::layout::Rect;
    // Auto-hide takes the nav's width away for as long as the terminal holds focus. The
    // layout must not move with it: it follows the attachment position, which the hidden
    // nav carries unchanged, so the resize keys keep driving the same axis and the nav
    // comes back the shape it left.
    let portrait = Rect::new(0, 0, 100, 34); // a 31-wide nav leaves 68 over 34 rows: square
    let shown = compute_regions(
        portrait,
        NavSize::visible(31).with_position(NavPosition::Top),
        1,
    );
    let gone = compute_regions(
        portrait,
        NavSize::hidden(31).with_position(NavPosition::Top),
        1,
    );
    assert_eq!(shown.layout, ViewLayout::Band);
    assert_eq!(
        gone.layout,
        ViewLayout::Band,
        "hiding the nav is not a reflow"
    );
    assert_eq!(
        gone.terminal, portrait,
        "and the terminal owns the whole area"
    );
    assert_eq!(gone.tree, Rect::default());
    // The same holds the other way round: a wide window stays a column while hidden.
    let landscape = Rect::new(0, 0, 260, 40);
    assert_eq!(
        compute_regions(landscape, NavSize::hidden(31), 1).layout,
        ViewLayout::Column
    );
    assert_eq!(
        compute_regions(landscape, NavSize::visible(31), 1).layout,
        ViewLayout::Column
    );
    // And on the mirrored column: the aspect cannot move a pinned placement either.
    assert_eq!(
        compute_regions(
            landscape,
            NavSize::visible(31).with_position(NavPosition::Right),
            1
        )
        .layout,
        ViewLayout::Column
    );
    assert_eq!(
        compute_regions(
            landscape,
            NavSize::hidden(31).with_position(NavPosition::Right),
            1
        )
        .layout,
        ViewLayout::Column
    );
}

#[test]
fn compute_regions_side_top_and_hidden() {
    use ratatui::layout::Rect;
    // Landscape → Column: tree left, 1-col border, terminal right. The hint bar is the
    // NAV column's bottom row, so the border and the terminal keep the full height.
    let land = Rect::new(0, 0, 140, 30);
    let s = compute_regions(land, NavSize::visible(48), 1);
    assert_eq!(s.layout, ViewLayout::Column);
    assert_eq!(s.tree, Rect::new(0, 0, 48, 29));
    assert_eq!(s.view_border, Rect::new(48, 0, 1, 30));
    assert_eq!(s.terminal, Rect::new(49, 0, 91, 30));
    assert_eq!(s.hint_bar, Rect::new(0, 29, 48, 1));
    // A landscape SCREEN can still carry the band when the side tree would squeeze the
    // terminal view into a portrait shape; the pinned Top states that placement directly:
    // 100 wide, tree 48 → terminal view ~51 wide vs 80 tall, so a band beats a column
    // even though the screen itself is wider than tall.
    let squeezed = compute_regions(
        Rect::new(0, 0, 140, 60),
        NavSize::visible(48).with_position(NavPosition::Top),
        1,
    );
    assert_eq!(squeezed.layout, ViewLayout::Band);
    // Portrait → band on top: tree band on top, 1-row border, terminal below. Every band
    // row holds cards, and the hint bar rests on the view border row itself.
    let port = Rect::new(0, 0, 40, 100);
    let t = compute_regions(
        port,
        NavSize::visible(48).with_position(NavPosition::Top),
        1,
    );
    assert_eq!(t.layout, ViewLayout::Band);
    assert_eq!(t.tree.y, 0);
    assert_eq!(t.tree.width, 40);
    let band_h = t.tree.height;
    assert_eq!(t.view_border, Rect::new(0, band_h, 40, 1));
    assert_eq!(t.hint_bar, t.view_border);
    assert_eq!(t.terminal.x, 0);
    assert_eq!(t.terminal.y, band_h + 1);
    assert_eq!(t.terminal.width, 40);
    // Tree-hidden sentinel: the terminal owns the whole area, no hint bar / border.
    let hidden = compute_regions(land, NavSize::hidden(48), 1);
    assert_eq!(hidden.terminal, land);
    assert_eq!(hidden.hint_bar, Rect::default());
    assert_eq!(hidden.view_border, Rect::default());
}

#[test]
fn compute_regions_right_column() {
    use ratatui::layout::Rect;
    // Pinned right: terminal left, 1-col border, tree right. The nav region's inner
    // layout is the left column's unchanged - the mirror flips only what sits on which
    // side of the view border - so the hint bar is still the nav region's bottom row.
    let land = Rect::new(0, 0, 140, 30);
    let s = compute_regions(
        land,
        NavSize::visible(48).with_position(NavPosition::Right),
        1,
    );
    assert_eq!(s.layout, ViewLayout::Column);
    assert_eq!(s.terminal, Rect::new(0, 0, 91, 30));
    assert_eq!(s.view_border, Rect::new(91, 0, 1, 30));
    assert_eq!(s.tree, Rect::new(92, 0, 48, 29));
    assert_eq!(s.hint_bar, Rect::new(92, 29, 48, 1));
    // The hidden sentinel keeps the position's shape: the terminal owns the whole area,
    // the tree/border/hint bar default, and the layout stays the pinned column.
    let gone = compute_regions(
        land,
        NavSize::hidden(48).with_position(NavPosition::Right),
        1,
    );
    assert_eq!(gone.terminal, land);
    assert_eq!(gone.layout, ViewLayout::Column);
    assert_eq!(gone.tree, Rect::default());
    assert_eq!(gone.view_border, Rect::default());
    assert_eq!(gone.hint_bar, Rect::default());
}

#[test]
fn compute_regions_bottom_band() {
    use ratatui::layout::Rect;
    // Pinned bottom: terminal above, 1-row border, tree band below. Every band row holds
    // cards, and the hint bar rests on the view border row above them.
    let port = Rect::new(0, 0, 40, 100);
    let b = compute_regions(
        port,
        NavSize::visible(48).with_position(NavPosition::Bottom),
        1,
    );
    assert_eq!(b.layout, ViewLayout::Band);
    assert_eq!(b.terminal, Rect::new(0, 0, 40, 59));
    assert_eq!(b.view_border, Rect::new(0, 59, 40, 1));
    assert_eq!(b.tree, Rect::new(0, 60, 40, 40));
    assert_eq!(b.hint_bar, b.view_border);
}

#[tokio::test]
async fn wheel_moves_the_selection_like_the_arrow_keys() {
    // The plain wheel and ↑/↓ share nav_vertical, so one notch lands on the same row as one
    // arrow press - in either layout (column siblings / band within-host).
    let mut a = Harness::new(sample());
    a.sw.mouse_scroll(true, &a.state);
    let by_wheel = a.sw.selected;
    let mut b = Harness::new(sample());
    b.key(KeyCode::Down).await;
    assert_eq!(
        by_wheel, b.sw.selected,
        "wheel down lands where ↓ does (column)"
    );

    let mut c = Harness::new_sized(sample(), 60, 70);
    c.sw.mouse_scroll(true, &c.state);
    let by_wheel_top = c.sw.selected;
    let mut d = Harness::new_sized(sample(), 60, 70);
    d.key(KeyCode::Down).await;
    assert_eq!(
        by_wheel_top, d.sw.selected,
        "wheel down lands where ↓ does (band)"
    );
}

#[tokio::test]
async fn a_digit_opens_the_jump_popup_and_lands_on_that_card() {
    // The digit is applied at once (so `prefix 2` IS the jump) and the popup stays open
    // holding it, ready to grow into a two-digit number. The numbers count the SELECTABLE
    // cards, section titles excepted.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Char('2')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        2,
        "the seeding digit jumps immediately"
    );
    assert!(h.state.is_inputting(), "the popup stays open to extend it");
    // Editing the number re-targets live: 2 → 1 moves without submitting anything.
    h.key(KeyCode::Backspace).await;
    h.key(KeyCode::Char('1')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "each edit re-targets the selection"
    );
    // Enter only closes; the selection is already where the live jump put it.
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting(), "Enter closes the popup");
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "Enter is a no-op on the selection"
    );
}

#[tokio::test]
async fn cancelling_a_jump_restores_the_starting_card() {
    let mut h = Harness::new(sample());
    let start = h.sw.selected;
    h.key(KeyCode::Char('3')).await;
    assert_ne!(h.sw.selected, start, "the jump moved");
    h.key(KeyCode::Esc).await;
    assert!(!h.state.is_inputting(), "Esc closes the popup");
    assert_eq!(
        h.sw.selected, start,
        "Esc returns to the card the jump started from"
    );
}

#[tokio::test]
async fn a_jump_past_the_last_card_is_inert() {
    // Typing a number that does not exist yet must not snap to an edge - the number is
    // still being typed, and `9` on the way to `95` should not jerk the selection. The
    // digits are still taken into the buffer; only the selection refuses to move.
    let mut h = Harness::new(sample());
    let n = h.sw.rows.len();
    h.key(KeyCode::Char('1')).await;
    let one = h.sw.selected;
    for c in n.to_string().chars() {
        h.key(KeyCode::Char(c)).await;
    }
    assert_eq!(
        h.sw.selected, one,
        "an out-of-range number leaves the selection alone"
    );
    assert_eq!(
        h.input_buffer(),
        format!("1{n}"),
        "the out-of-range number stays in the buffer"
    );
    // A letter typed into a card number is dropped rather than breaking the parse.
    h.key(KeyCode::Char('x')).await;
    assert_eq!(h.sw.selected, one, "a non-digit is ignored");
    assert_eq!(
        h.input_buffer(),
        format!("1{n}"),
        "and never enters the buffer"
    );
}

#[tokio::test]
async fn selecting_a_card_never_moves_its_session_name() {
    // The point of hiding the number and keeping the connector: a card's session name
    // must sit in the same screen column whether or not it is selected. Anything that
    // shifts it makes the list twitch as the cursor runs down it, which is exactly what
    // dropping two columns of connector did.
    let mut h = Harness::new(one_host_scan(
        "srv",
        vec![
            sess_mux("srv", "alpha", "tmux"),
            sess_mux("srv", "beta", "tmux"),
        ],
    ));
    // The COLUMN, not the byte offset: the address column and the `└` connector hold
    // multi-byte glyphs, so a byte index would report a shift that is not on screen.
    let col_of = |h: &Harness, name: &str| -> Option<usize> {
        let row = h.nav_row_of(name)?;
        let line = nav_line(h, row);
        let at = line.find(name)?;
        Some(line[..at].chars().count())
    };
    let unselected = col_of(&h, "beta").expect("beta unselected");
    h.key(KeyCode::Down).await; // select beta
    let selected = col_of(&h, "beta").expect("beta selected");
    assert_eq!(
        selected, unselected,
        "the session name holds its column across selection"
    );
    // And the name still lines up with the OTHER card's name, which never moved.
    let alpha = col_of(&h, "alpha").expect("alpha");
    assert_eq!(selected, alpha, "both names share one column");
}

#[test]
fn paint_does_not_mutate_switcher_render_state() {
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let area = Rect::new(0, 0, 140, 30);
    let plan = sw.layout(
        area,
        NavSize::visible(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    let before = plan.clone();
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();

    term.draw(|f| sw.render(f, None, false, &state, &plan))
        .unwrap();

    assert_eq!(plan, before, "paint writes only the frame");
}

#[test]
fn every_unselected_card_carries_its_1_based_number_beside_its_session() {
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let plan = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::visible(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    term.draw(|f| sw.render(f, None, false, &state, &plan))
        .unwrap();
    let buf = term.backend().buffer();
    // The address column starts at column 0, right-aligned in one width for the whole
    // frame, on the card's single row. The SELECTED card holds the mark there instead of
    // a number: it is the address you would type to get where you already are. A section
    // title carries no number at all - it is not a card, and it is never the selection.
    let selected = sw.selected;
    let num_w = sw.selectable_count().to_string().len().max(1) as u16;
    let read =
        |x: u16, y: u16, w: u16| -> String { (x..x + w).map(|c| buf[(c, y)].symbol()).collect() };
    // Read each card where the PLAN put it: the side list parts its two bands, so a card
    // is not always the sum of the heights above it.
    assert_eq!(plan.nav_cells.len(), sw.rows.len(), "every row was drawn");
    for (i, rect) in plan.nav_cells.iter().copied() {
        if matches!(sw.rows[i].reference, RowRef::Section { .. }) {
            assert_ne!(i, selected, "the selection never lands on a section title");
            // The section title is flush left - its host name occupies the address
            // column - so it must simply never carry a number or the mark.
            let first = read(rect.x, rect.y, num_w).trim().to_string();
            assert!(
                first.parse::<usize>().is_err() && first != super::render::SELECTED_MARK,
                "row {i} (a section title) carries no number or mark, got {first:?}"
            );
            continue;
        }
        let want = if i == selected {
            super::render::SELECTED_MARK.to_string()
        } else {
            sw.card_number(i).to_string()
        };
        // Every card is one row, so the number sits on that single row.
        assert_eq!(
            read(rect.x, rect.y, num_w).trim(),
            want,
            "card {i} address on its row (selected={selected})"
        );
    }
}

#[test]
fn the_armed_hint_bar_floats_across_the_whole_window() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    // A grid packed edge to edge, so any cell the bar fails to cover shows an `X`.
    let mut grid = crate::display::grid::Grid::new(30, 140);
    let mut fill = Vec::new();
    for r in 0..30u16 {
        fill.extend(format!("[{};1H", r + 1).bytes());
        fill.extend(std::iter::repeat_n(b'X', 140));
    }
    grid.feed(&fill);
    let g = grid;
    let draw =
        |term: &mut Terminal<TestBackend>, sw: &mut Switcher, state: &crate::state::State| {
            term.draw(|f| sw.render_test(f, Some(&g), false, NavSize::visible(NAV_WIDTH), state))
                .unwrap();
        };
    draw(&mut term, &mut sw, &state);
    let y = term.backend().buffer().area.height - 1;
    let row = |term: &Terminal<TestBackend>, y: u16| -> String {
        let buf = term.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    };
    // At rest the bar is the nav's prefix indicator, so the columns past the nav belong
    // to the view below it - the bar does not reach them.
    let resting = row(&term, y);
    assert!(
        resting[NAV_WIDTH as usize..].trim().is_empty() || !resting.trim_end().ends_with("quit"),
        "the resting bar stays in the nav column: {resting:?}"
    );
    // Where the cards sit is what must not move when the prefix is armed.
    let cards_before = row(&term, 0);
    state.chrome.set_armed(true);
    draw(&mut term, &mut sw, &state);
    let armed = row(&term, y);
    assert!(
        armed.contains("quit") || armed.contains("q "),
        "the armed bar spans past the nav column: {armed:?}"
    );
    let after_nav: String = armed.chars().skip(NAV_WIDTH as usize).collect();
    assert!(
        armed.chars().count() > NAV_WIDTH as usize && !after_nav.trim().is_empty(),
        "and paints over the view beneath it: {armed:?}"
    );
    // Covering, not just recolouring: a style alone leaves the grid's own characters in
    // the columns the bar's text does not reach, which reads as text spilled across the
    // screen rather than a bar over it.
    assert!(
        !armed.contains('X'),
        "the armed bar covers the grid across its whole row: {armed:?}"
    );
    assert_eq!(
        row(&term, 0),
        cards_before,
        "arming the prefix only widens the paint, so no card moves"
    );
}

#[tokio::test]
async fn the_input_hint_bar_floats_across_the_whole_window() {
    // An open input must be seen even with the nav hidden (auto-hide + terminal
    // focus): like the armed bar, it floats to the window's bottom row and covers
    // the grid, so what is being typed never disappears.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.open_input(InputMode::Filter, &mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    // A grid packed edge to edge, so any cell the bar fails to cover shows an `X`.
    let mut grid = crate::display::grid::Grid::new(30, 140);
    let mut fill = Vec::new();
    for r in 0..30u16 {
        fill.extend(format!("\x1b[{};1H", r + 1).bytes());
        fill.extend(std::iter::repeat_n(b'X', 140));
    }
    grid.feed(&fill);
    term.draw(|f| sw.render_test(f, Some(&grid), true, NavSize::hidden(NAV_WIDTH), &state))
        .unwrap();
    let y = term.backend().buffer().area.height - 1;
    let row: String = (0..140)
        .map(|x| term.backend().buffer()[(x, y)].symbol())
        .collect();
    assert!(
        row.contains("[filter] filter sessions · 4 matches · 0 hidden hosts:"),
        "the input bar floats onto the hidden-nav bottom row: {row:?}"
    );
    assert!(
        !row.contains('X'),
        "and covers the grid across the whole row: {row:?}"
    );
}

#[tokio::test]
async fn a_jump_holds_out_of_range_numbers_and_vets_at_enter() {
    // The number goes into the buffer whatever it addresses; the existence check is
    // Enter-time. While the number names no card the selection stays put, and Enter on
    // it flashes the range and keeps the popup open.
    let mut h = Harness::new(sample());
    let n = h.sw.rows.len();
    assert!(n < 10, "sample() is a single-digit list");
    let start = h.sw.selected;
    // The seeding digit itself is not vetted: an out-of-range digit opens the popup
    // holding it, and leaves the selection alone.
    h.key(KeyCode::Char(char::from_digit(n as u32, 10).unwrap()))
        .await;
    assert!(
        h.state.is_inputting(),
        "an out-of-range digit opens the popup"
    );
    assert_eq!(h.input_buffer(), n.to_string(), "the buffer holds it");
    assert_eq!(
        h.sw.selected, start,
        "while no card carries the number, the selection stays"
    );
    // Enter on a dead number flashes the range and keeps the popup open.
    h.key(KeyCode::Enter).await;
    assert!(h.state.is_inputting(), "the popup stays open");
    assert!(
        h.state.chrome.flash.contains("no session"),
        "the flash names the dead number: {}",
        h.state.chrome.flash
    );
    let bar = h.hint_bar_text();
    assert!(
        bar.contains("no session"),
        "the flash shows over the open input: {bar:?}"
    );
    // A fresh edit clears the flash and the input line returns.
    h.key(KeyCode::Backspace).await;
    assert!(h.state.chrome.flash.is_empty(), "a key clears the flash");
    // In range, the popup opens and each further digit is taken as typed.
    h.key(KeyCode::Char('1')).await;
    assert!(h.state.is_inputting(), "an in-range digit opens the popup");
    assert_eq!(h.sw.card_number(h.sw.selected), 1, "the digit lands");
    h.key(KeyCode::Char('0')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "10 is out of range, so the selection keeps 1"
    );
    assert_eq!(
        h.input_buffer(),
        "10",
        "the buffer holds the out-of-range extension"
    );
}

#[tokio::test]
async fn a_jump_enter_on_an_empty_buffer_keeps_the_popup_open() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Char('1')).await;
    h.key(KeyCode::Backspace).await; // empty the buffer
    h.key(KeyCode::Enter).await;
    assert!(
        h.state.is_inputting(),
        "Enter on an empty buffer keeps the popup open"
    );
    assert!(h.state.chrome.flash.is_empty(), "and flashes nothing");
    assert_eq!(h.input_buffer(), "", "the buffer is still empty");
}

#[tokio::test]
async fn cancelling_a_jump_from_a_dead_number_still_restores() {
    // Esc after typing a dead extension returns to where the jump started, exactly as
    // a live one does: the selection that never moved is still the starting card.
    let mut h = Harness::new(sample());
    let start = h.sw.selected;
    h.key(KeyCode::Char('1')).await; // live jump to 1
    h.key(KeyCode::Char('9')).await; // 19 is dead; the selection stays on 1
    assert_eq!(h.input_buffer(), "19");
    h.key(KeyCode::Esc).await;
    assert!(!h.state.is_inputting(), "Esc closes the popup");
    assert_eq!(
        h.sw.selected, start,
        "Esc returns to where the jump started"
    );
}

#[tokio::test]
async fn a_jump_walks_into_a_two_digit_number() {
    let mut h = Harness::new(scan_with_sessions(24));
    h.key(KeyCode::Char('1')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "the seeding digit lands immediately"
    );
    h.key(KeyCode::Char('7')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        17,
        "the second digit extends the number live"
    );
    h.key(KeyCode::Backspace).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "backspace walks it back"
    );
    h.key(KeyCode::Char('9')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        19,
        "a different second digit re-lands"
    );
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting(), "Enter closes the popup");
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        19,
        "and keeps where the jump landed"
    );
}

#[tokio::test]
async fn card_numbers_count_from_1_and_the_last_card_carries_the_count() {
    // The number a card carries is its 1-based rank among the selectable cards:
    // the first card is 1 and the last carries the card count. A section title
    // carries no number, so the ranks count the cards only.
    let mut h = Harness::new(sample());
    let selectable: Vec<usize> = (0..h.sw.rows.len())
        .filter(|&i| h.sw.rows[i].selectable())
        .collect();
    for (rank, &i) in selectable.iter().enumerate() {
        assert_eq!(
            h.sw.card_number(i),
            rank + 1,
            "card {i}'s number is its rank"
        );
    }
    // `prefix 1` lands on the card numbered 1, the first selectable card.
    h.key(KeyCode::Char('1')).await;
    assert_eq!(h.sw.selected, selectable[0], "1 addresses the first card");
}

#[tokio::test]
async fn a_jump_on_0_opens_the_input_and_names_no_card() {
    // No card carries 0: `prefix 0` opens the jump input holding 0 and the
    // selection stays put; Enter flashes the 1-based range and keeps the popup.
    let mut h = Harness::new(sample());
    h.key(KeyCode::End).await; // start far from where 0 used to point
    let start = h.sw.selected;
    h.key(KeyCode::Char('0')).await;
    assert!(h.state.is_inputting(), "0 still opens the jump input");
    assert_eq!(h.input_buffer(), "0", "the input holds the 0");
    assert_eq!(
        h.sw.selected, start,
        "no card carries 0, so the selection stays"
    );
    let bar = h.hint_bar_text();
    assert!(
        bar.contains("jump to a session (1 - 4)"),
        "the guide states the 1-based range: {bar:?}"
    );
    h.key(KeyCode::Enter).await;
    assert!(h.state.is_inputting(), "the popup stays open");
    assert!(
        h.state.chrome.flash.contains("no session 0 (1 - 4)"),
        "the flash names the dead number and the 1-based range: {}",
        h.state.chrome.flash
    );
}

#[tokio::test]
async fn a_leading_zero_names_its_value() {
    // The number is read as its value, spelling included: 01 is 1, the card 1
    // addresses. The dead 0 comes alive the moment the digit giving it its
    // value lands, and Enter closes on the card the value names.
    let mut h = Harness::new(sample());
    h.key(KeyCode::End).await;
    h.key(KeyCode::Char('0')).await;
    h.key(KeyCode::Char('1')).await;
    let first = h.sw.rows.iter().position(Row::selectable).unwrap();
    assert_eq!(h.sw.selected, first, "01 names the card 1 names");
    assert_eq!(h.sw.card_number(h.sw.selected), 1, "which carries number 1");
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting(), "Enter closes on the card 01 names");
}

#[tokio::test]
async fn the_two_digit_boundary_starts_at_exactly_ten_cards() {
    // Ten is where the numbers gain a digit: the address column is two wide, so a
    // single-digit number takes a leading blank and 10 paints both of its digits.
    // It is also the first count a two-digit number can name: 10 is the LAST card,
    // and 11 is already past the end.
    let mut h = Harness::new(scan_with_sessions(10));
    let selectable: Vec<usize> = (0..h.sw.rows.len())
        .filter(|&i| h.sw.rows[i].selectable())
        .collect();
    assert_eq!(
        selectable.len(),
        10,
        "the fixture sits exactly on the boundary"
    );
    let last = *selectable.last().unwrap();
    // The painted address is the width made visible: the first three columns of a
    // card's row (the number right-aligned in two, then the separating blank).
    let address_of = |h: &Harness, row: usize| -> String {
        let (_, rect) = h
            .plan
            .nav_cells
            .iter()
            .find(|(i, _)| *i == row)
            .expect("every row was drawn");
        (rect.x..rect.x + 3)
            .map(|x| h.buf()[(x, rect.y)].symbol())
            .collect()
    };
    assert_eq!(
        address_of(&h, selectable[0]),
        format!(" {} ", super::render::SELECTED_MARK),
        "the selected card's mark sits right-aligned in the two-wide column"
    );
    assert_eq!(
        address_of(&h, last).trim_end(),
        "10",
        "the last card paints its two-digit number"
    );
    // The jump starts far away so its steps are real moves: End sits on the last
    // card, `prefix 1` walks back to the first, and the second digit walks forward
    // into the first two-digit number, the last card again.
    h.key(KeyCode::End).await;
    assert_eq!(h.sw.selected, last, "End starts on the last card");
    h.key(KeyCode::Char('1')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "the seeding digit lands immediately"
    );
    h.key(KeyCode::Char('0')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        10,
        "10 addresses the first two-digit number"
    );
    assert_eq!(h.sw.selected, last, "which is the last selectable card");
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting(), "Enter closes the popup");
    assert_eq!(h.sw.selected, last, "and keeps where the jump landed");
    // One past the boundary is already dead: the extension to 11 leaves the
    // selection on the seeded card, and Enter flashes the range and keeps the
    // popup open.
    h.key(KeyCode::Char('1')).await;
    h.key(KeyCode::Char('1')).await;
    assert_eq!(
        h.sw.card_number(h.sw.selected),
        1,
        "11 names no card, so the selection keeps the seeded card"
    );
    assert_eq!(h.input_buffer(), "11", "the buffer holds the dead number");
    h.key(KeyCode::Enter).await;
    assert!(h.state.is_inputting(), "the popup stays open");
    assert!(
        h.state.chrome.flash.contains("no session 11 (1 - 10)"),
        "the flash names the dead number and the 1-based range: {}",
        h.state.chrome.flash
    );
}

#[test]
fn a_hidden_nav_keeps_no_status_line_until_it_has_something_to_say() {
    // Auto-hide with the terminal focused gives the mux the whole screen (nav_width 0).
    // At rest that includes the bottom row: xmux takes none of it.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut grid = crate::display::grid::Grid::new(30, 140);
    let mut fill = Vec::new();
    for r in 0..30u16 {
        fill.extend(format!("\x1b[{};1H", r + 1).bytes());
        fill.extend(std::iter::repeat_n(b'X', 140));
    }
    grid.feed(&fill);
    let row = |term: &Terminal<TestBackend>| -> String {
        let buf = term.backend().buffer();
        let y = buf.area.height - 1;
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    };
    let draw =
        |term: &mut Terminal<TestBackend>, sw: &mut Switcher, state: &crate::state::State| {
            term.draw(|f| sw.render_test(f, Some(&grid), true, NavSize::hidden(NAV_WIDTH), state))
                .unwrap();
        };

    draw(&mut term, &mut sw, &state);
    assert!(
        row(&term).chars().all(|c| c == 'X'),
        "at rest a hidden nav leaves the bottom row to the mux: {:?}",
        row(&term)
    );

    // Armed: the prefix must still answer, so the bar floats on the window's bottom row.
    state.chrome.set_armed(true);
    draw(&mut term, &mut sw, &state);
    let armed = row(&term);
    assert!(
        armed.contains("C-g") && !armed.contains('X'),
        "an armed prefix floats the bar over the full width even with the nav hidden: {armed:?}"
    );
    state.chrome.set_armed(false);

    // A refusal is the other thing that must be seen: with no nav row to hold it, it
    // floats too. A flash the user cannot see is worse than a row borrowed for a moment.
    state.flash("nope".to_string());
    draw(&mut term, &mut sw, &state);
    let flashed = row(&term);
    assert!(
        flashed.contains("nope") && !flashed.contains('X'),
        "a refusal floats over a hidden nav: {flashed:?}"
    );
    state.chrome.clear_flash();

    // Scan progress does NOT float: it persists, and the user asked for the whole screen.
    state.scanning.insert("local".to_string());
    draw(&mut term, &mut sw, &state);
    assert!(
        row(&term).chars().all(|c| c == 'X'),
        "persistent states stay out of a hidden nav: {:?}",
        row(&term)
    );
}

#[tokio::test]
async fn hint_bar_and_help_reflect_new_model() {
    let mut h = Harness::new(sample());
    // At rest the bar names the prefix alone. The keys it unlocks are one keypress away,
    // so they do not crowd the nav's bottom row.
    let resting = h.hint_bar_text();
    assert_eq!(resting.trim(), "C-g");

    // Armed, it becomes the cheatsheet for exactly those keys.
    h.state.chrome.set_armed(true);
    h.draw();
    let armed = h.hint_bar_text();
    assert!(
        armed.contains("q") && armed.contains("?"),
        "the armed bar lists the chords the prefix unlocks:\n{armed}"
    );
    h.state.chrome.set_armed(false);
    h.draw();
    h.sw.show_help(&mut h.state); // driven by the app's `prefix ?`
    h.draw();
    let help = h.text();
    assert!(
        help.contains("focus the terminal"),
        "help explains focusing the terminal view:\n{help}"
    );
    assert!(
        help.contains("collapse / expand the nav"),
        "help explains the collapse key:\n{help}"
    );
    assert!(
        help.contains("previous / next host/mux (host cards as one)"),
        "help names what ←/→ walk, since the two steps differ:\n{help}"
    );
    assert!(
        !help.contains("select = attach"),
        "no useless 'select = attach' noise in help:\n{help}"
    );
    assert!(
        !help.contains("dwell") && !help.to_lowercase().contains("previous foreground"),
        "no stale dwell/esc-return strings:\n{help}"
    );
}

#[tokio::test]
async fn view_border_uses_configured_colors() {
    // The `[ui] view-*-border-style` colours drive the whole view border: active for
    // nav focus, inactive for terminal focus, and hover overrides either state.
    let backend = TestBackend::new(140, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    state.chrome.set_view_border_colors(ViewBorderColors {
        active: Color::Blue,
        inactive: Color::Gray,
        hover: Color::Red,
    });
    let x = NAV_WIDTH;
    let (top, bottom) = (2u16, 27u16);
    let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

    // Nav focused: the whole rule is active.
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, top),
        Color::Blue,
        "configured active across the rule"
    );
    assert_eq!(
        fg(&buf, bottom),
        Color::Blue,
        "configured active across the rule"
    );

    // Terminal focused: the whole rule is inactive.
    term.draw(|f| sw.render_test(f, None, true, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(fg(&buf, top), Color::Gray);
    assert_eq!(fg(&buf, bottom), Color::Gray);

    // Hovering the rule overrides with the configured hover colour.
    state.chrome.set_view_border_hovered(true);
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, top),
        Color::Red,
        "configured hover colour while hovered"
    );
}

#[tokio::test]
async fn view_border_uses_one_color_for_both_focus_states() {
    let pal = crate::ui::palette::Palette::default();
    let backend = TestBackend::new(140, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let x = NAV_WIDTH;
    let (top, bottom) = (2u16, 27u16); // within the top / bottom halves of height 30
    let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

    // Terminal focused: every cell uses the inactive colour.
    term.draw(|f| sw.render_test(f, None, true, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(buf[(x, top)].symbol(), "│", "view border still drawn");
    assert_eq!(
        fg(&buf, bottom),
        pal.disabled,
        "terminal focus: whole rule inactive"
    );
    assert_eq!(
        fg(&buf, top),
        pal.disabled,
        "terminal focus: whole rule inactive"
    );

    // Nav focused: every cell uses the active colour.
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(fg(&buf, top), pal.primary, "nav focus: whole rule active");
    assert_eq!(
        fg(&buf, bottom),
        pal.primary,
        "nav focus: whole rule active"
    );
}

#[test]
fn compute_regions_collapsed_geometry_for_all_positions() {
    use ratatui::layout::Rect;

    let area = Rect::new(0, 0, 140, 30);
    let width = collapsed_nav_width("C-g");
    let left = compute_regions(
        area,
        NavSize {
            natural: 48,
            width,
            height: 0,
            position: NavPosition::Left,
            collapsed: true,
        },
        1,
    );
    assert_eq!(left.tree, Rect::default());
    assert_eq!(left.hint_bar, Rect::new(0, 29, width, 1));
    assert_eq!(left.view_border, Rect::new(width, 0, 1, 30));
    assert_eq!(left.terminal, Rect::new(width + 1, 0, 140 - width - 1, 30));

    let right = compute_regions(
        area,
        NavSize {
            natural: 48,
            width,
            height: 0,
            position: NavPosition::Right,
            collapsed: true,
        },
        1,
    );
    assert_eq!(right.tree, Rect::default());
    assert_eq!(right.hint_bar, Rect::new(140 - width, 29, width, 1));
    assert_eq!(right.view_border, Rect::new(139 - width, 0, 1, 30));
    assert_eq!(right.terminal, Rect::new(0, 0, 139 - width, 30));

    let top = compute_regions(
        area,
        NavSize {
            collapsed: true,
            position: NavPosition::Top,
            ..NavSize::visible(48)
        },
        1,
    );
    assert!(
        top.tree.is_empty(),
        "a collapsed band is the seam line only"
    );
    assert_eq!(top.view_border, Rect::new(0, 0, 140, 1));
    assert_eq!(
        top.hint_bar, top.view_border,
        "the prefix rests on the seam"
    );
    assert_eq!(top.terminal, Rect::new(0, 1, 140, 29));

    let bottom = compute_regions(
        area,
        NavSize {
            collapsed: true,
            position: NavPosition::Bottom,
            ..NavSize::visible(48)
        },
        1,
    );
    assert!(
        bottom.tree.is_empty(),
        "a collapsed band is the seam line only"
    );
    assert_eq!(bottom.terminal, Rect::new(0, 0, 140, 29));
    assert_eq!(bottom.view_border, Rect::new(0, 29, 140, 1));
    assert_eq!(
        bottom.hint_bar, bottom.view_border,
        "the prefix rests on the seam"
    );
}

#[test]
fn a_floating_bar_opens_from_the_prefix_indicator_toward_the_terminal() {
    use super::render::hint_bar_rect;
    let area = Rect::new(0, 0, 24, 8);
    let full = Rect::new(0, 0, 24, 8);
    // A top band opens below its seam, collapsed or not, and grows down.
    let top = hint_bar_rect(
        Rect::new(0, 0, 24, 1),
        Rect::new(0, 1, 24, 7),
        area,
        3,
        true,
        NavPosition::Top,
    );
    assert_eq!(top, Rect::new(0, 1, 24, 3));
    let top_band = hint_bar_rect(
        Rect::new(20, 3, 4, 1),
        Rect::new(0, 4, 24, 4),
        area,
        1,
        true,
        NavPosition::Top,
    );
    assert_eq!(top_band, Rect::new(0, 4, 24, 1), "the row below the seam");
    // A bottom band opens above its seam and grows up.
    let bottom = hint_bar_rect(
        Rect::new(20, 5, 4, 1),
        Rect::new(0, 0, 24, 5),
        area,
        1,
        true,
        NavPosition::Bottom,
    );
    assert_eq!(bottom, Rect::new(0, 4, 24, 1), "the row above the seam");
    // A side column opens on its bottom row across the terminal view's columns.
    let left = hint_bar_rect(
        Rect::new(0, 7, 7, 1),
        Rect::new(8, 0, 16, 8),
        area,
        3,
        true,
        NavPosition::Left,
    );
    assert_eq!(left, Rect::new(8, 5, 16, 3));
    let right = hint_bar_rect(
        Rect::new(17, 7, 7, 1),
        Rect::new(0, 0, 16, 8),
        area,
        1,
        true,
        NavPosition::Right,
    );
    assert_eq!(right, Rect::new(0, 7, 16, 1));
    // A hidden nav has no indicator: the bar borrows the window's bottom rows.
    let hidden = hint_bar_rect(Rect::default(), full, area, 2, true, NavPosition::Left);
    assert_eq!(hidden, Rect::new(0, 6, 24, 2));
    // At rest the bar is the indicator itself.
    let rest = hint_bar_rect(
        Rect::new(0, 7, 7, 1),
        Rect::new(8, 0, 16, 8),
        area,
        1,
        false,
        NavPosition::Left,
    );
    assert_eq!(rest, Rect::new(0, 7, 7, 1));
}

#[tokio::test]
async fn view_border_color_is_independent_of_nav_position() {
    let pal = crate::ui::palette::Palette::default();
    let fg = |buf: &Buffer, x: u16, y: u16| buf[(x, y)].fg;

    // Column with the nav pinned right: the 1-col border at x=91, terminal to its left.
    let right = NavSize::visible(NAV_WIDTH).with_position(NavPosition::Right);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    state.chrome.set_nav_position(NavPosition::Right);
    let x = 91;
    let (top, bottom) = (2u16, 27u16);

    // Nav focused: both ends use the active colour.
    term.draw(|f| sw.render_test(f, None, false, right, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, x, bottom),
        pal.primary,
        "right nav focus: whole rule active"
    );
    assert_eq!(
        fg(&buf, x, top),
        pal.primary,
        "right nav focus: whole rule active"
    );

    // Terminal focused: both ends use the inactive colour.
    term.draw(|f| sw.render_test(f, None, true, right, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, x, top),
        pal.disabled,
        "right terminal focus: whole rule inactive"
    );
    assert_eq!(
        fg(&buf, x, bottom),
        pal.disabled,
        "right terminal focus: whole rule inactive"
    );

    // Band with the nav pinned bottom: the 1-row border at y=59 across 40 columns.
    let bottom = NavSize::visible(NAV_WIDTH).with_position(NavPosition::Bottom);
    let mut term = Terminal::new(TestBackend::new(40, 100)).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    state.chrome.set_nav_position(NavPosition::Bottom);
    // The far end of the seam row holds the resting prefix, so the right sample stands
    // clear of it.
    let (y, left, right_col) = (59u16, 0u16, 30u16);

    // Nav focused: both ends use the active colour.
    term.draw(|f| sw.render_test(f, None, false, bottom, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, right_col, y),
        pal.primary,
        "bottom nav focus: whole rule active"
    );
    assert_eq!(
        fg(&buf, left, y),
        pal.primary,
        "bottom nav focus: whole rule active"
    );

    // Terminal focused: both ends use the inactive colour.
    term.draw(|f| sw.render_test(f, None, true, bottom, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, left, y),
        pal.disabled,
        "bottom terminal focus: whole rule inactive"
    );
    assert_eq!(
        fg(&buf, right_col, y),
        pal.disabled,
        "bottom terminal focus: whole rule inactive"
    );
}

#[tokio::test]
async fn view_border_highlights_on_hover() {
    // Hover swaps the rule to the HEAVY vertical (┃) - box-drawing has no bold form,
    // so the thicker glyph IS the weight cue - and recolours it brighter. No fill.
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let x = NAV_WIDTH;
    state.chrome.set_view_border_hovered(true);
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    for y in [2u16, 27u16] {
        let cell = &buf[(x, y)];
        assert_eq!(
            cell.symbol(),
            "┃",
            "hover: heavy (thick) rule glyph at row {y}"
        );
        assert_eq!(
            cell.fg,
            crate::ui::palette::Palette::default().accent,
            "hover: the border-hover cue reads in the accent role at row {y}"
        );
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "hover: not reversed/filled (no block) at row {y}",
        );
    }
}

#[tokio::test]
async fn view_border_glyph_reflects_auto_hide_mode() {
    // ║ (double) when auto-hide-nav mode is on, │ (single) when off - so a visible
    // tree that will vanish on blur is distinguishable from a pinned one.
    let backend = TestBackend::new(140, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
    let (x, y) = (NAV_WIDTH, 2u16);

    state.chrome.set_auto_hide(false);
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    assert_eq!(
        term.backend().buffer()[(x, y)].symbol(),
        "│",
        "mode off → single line"
    );

    state.chrome.set_auto_hide(true);
    term.draw(|f| sw.render_test(f, None, false, NavSize::visible(NAV_WIDTH), &state))
        .unwrap();
    assert_eq!(
        term.backend().buffer()[(x, y)].symbol(),
        "║",
        "mode on → double line"
    );
}

#[tokio::test]
async fn every_popup_type_is_opaque_over_a_colored_grid() {
    // A grid filled with a blue background; each popup type drawn over it must leave
    // zero interior cells showing the grid's background (the shared render_popup is
    // opaque - this locks it in across help / input / confirm).
    fn blue_grid() -> crate::display::grid::Grid {
        let mut g = crate::display::grid::Grid::new(30, 100);
        let mut fill = Vec::from(&b"\x1b[44m"[..]);
        for r in 0..30u16 {
            fill.extend(format!("\x1b[{};1H", r + 1).bytes());
            fill.extend(std::iter::repeat_n(b'X', 100));
        }
        g.feed(&fill);
        g
    }
    fn interior_blue(buf: &Buffer) -> usize {
        let mut tl = None;
        'o: for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == "╭" {
                    tl = Some((x, y));
                    break 'o;
                }
            }
        }
        let Some((x0, y0)) = tl else {
            return usize::MAX;
        };
        let mut w = 0;
        while x0 + w < buf.area.width - 1 && buf[(x0 + w, y0)].symbol() != "╮" {
            w += 1;
        }
        let mut hgt = 0;
        while y0 + hgt < buf.area.height - 1 && buf[(x0, y0 + hgt)].symbol() != "╰" {
            hgt += 1;
        }
        let mut n = 0;
        for y in (y0 + 1)..(y0 + hgt) {
            for x in (x0 + 1)..(x0 + w) {
                if buf[(x, y)].bg == Color::Indexed(4) {
                    n += 1;
                }
            }
        }
        n
    }

    let mut h = Harness::new(sample());
    h.sw.show_help(&mut h.state);
    let g = blue_grid();
    h.term
        .draw(|f| {
            h.sw.render_test(f, Some(&g), true, NavSize::hidden(NAV_WIDTH), &h.state)
        })
        .unwrap();
    assert_eq!(
        interior_blue(h.buf()),
        0,
        "help popup interior must be opaque"
    );

    let mut h = Harness::new(sample());
    let build = row_index(
        &h,
        |r| matches!(r, RowRef::Session { sess } if sess.name == "build"),
    );
    h.sw.set_selected(build, &h.state);
    h.sw.user_moved = true;
    h.sw.show_help(&mut h.state);
    let g = blue_grid();
    h.term
        .draw(|f| {
            h.sw.render_test(f, Some(&g), false, NavSize::visible(NAV_WIDTH), &h.state)
        })
        .unwrap();
    assert_eq!(
        interior_blue(h.buf()),
        0,
        "help popup interior must be opaque"
    );
}

#[test]
fn popup_border_press_then_drag_moves_the_rect() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state); // the help popup, the one popup that remains
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    let before_plan = sw.layout(
        Rect::new(0, 0, 140, 40),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    term.draw(|f| sw.render(f, None, false, &state, &before_plan))
        .unwrap();
    let before = before_plan.popup_rect;
    let (bx, by) = (before.x, before.y); // top-left corner is on the border
    assert!(
        sw.begin_popup_drag_in_plan(&before_plan, bx, by, &state),
        "press on the border grabs"
    );
    sw.drag_popup(bx + 5, by + 1);
    let after_plan = sw.layout(
        Rect::new(0, 0, 140, 40),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &before_plan,
    );
    term.draw(|f| sw.render(f, None, false, &state, &after_plan))
        .unwrap();
    assert_eq!(after_plan.popup_rect.x, before.x + 5, "moved right by 5");
    assert_eq!(after_plan.popup_rect.y, before.y + 1, "moved down by 1");
    sw.end_popup_drag();
    assert!(!sw.popup_drag_active());
}

#[test]
fn modals_are_mutually_exclusive() {
    // Opening either modal closes the other, so the drawn popup always matches where
    // keystrokes route.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.open_input(InputMode::Filter, &mut state);
    assert!(state.is_inputting(), "input opened");
    sw.show_help(&mut state);
    assert!(
        matches!(state.modal, Some(Modal::Help)) && !state.is_inputting(),
        "help closes the input"
    );
    sw.open_input(InputMode::Filter, &mut state);
    assert!(
        state.is_inputting() && !matches!(state.modal, Some(Modal::Help)),
        "the input closes help"
    );
}

#[test]
fn closed_popup_cannot_be_grabbed_even_with_a_stale_rect() {
    // popup_rect is refreshed only on render; a popup closed by a keystroke leaves a
    // stale rect. A press must NOT grab a popup that is no longer open.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let plan = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    let r = plan.popup_rect;
    state.modal = None; // close WITHOUT re-rendering → popup_rect is stale
    assert!(
        !sw.begin_popup_drag_in_plan(&plan, r.x, r.y, &state),
        "a stale rect must not grab a closed popup"
    );
}

#[test]
fn popup_renders_without_panicking_on_a_narrow_screen() {
    // A terminal narrower than the popup's 24-col minimum must not panic
    // (the width is `.max(24).min(width)`, never `clamp(24, width)`).
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let mut term = Terminal::new(TestBackend::new(10, 10)).unwrap();
    let plan = sw.layout(
        Rect::new(0, 0, 10, 10),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    term.draw(|f| sw.render(f, None, false, &state, &plan))
        .unwrap();
    assert!(plan.popup_rect.width <= 10, "popup fits the narrow screen");
}

#[test]
fn popup_interior_press_does_not_grab() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let plan = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    let r = plan.popup_rect;
    assert!(
        !sw.begin_popup_drag_in_plan(&plan, r.x + 2, r.y + 2, &state),
        "interior press does not start a drag"
    );
}

#[test]
fn popup_drag_clamps_within_screen() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let plan = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    let r = plan.popup_rect;
    assert!(sw.begin_popup_drag_in_plan(&plan, r.x, r.y, &state));
    sw.drag_popup(r.x.saturating_sub(50), r.y); // yank far left, past the edge
    let next = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &plan,
    );
    term.draw(|f| sw.render(f, None, false, &state, &next))
        .unwrap();
    assert_eq!(next.popup_rect.x, 0, "clamped to the left screen edge");
}

#[test]
fn toggle_help_flips_visibility() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    assert!(!matches!(state.modal, Some(Modal::Help)));
    sw.toggle_help(&mut state);
    assert!(matches!(state.modal, Some(Modal::Help)));
    sw.toggle_help(&mut state);
    assert!(!matches!(state.modal, Some(Modal::Help)));
}

#[test]
fn feed_reader_key_is_modal_and_closes_on_q_or_esc() {
    // tmux view-mode style: while open, every key is consumed; q/Esc closes, the
    // rest are swallowed; while closed, nothing is consumed (falls through).
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    assert!(
        !sw.feed_reader_key(b"q", &mut state),
        "closed → not consumed, routes normally"
    );

    sw.toggle_help(&mut state);
    assert!(sw.feed_reader_key(b"j", &mut state), "open → consumed");
    assert!(
        matches!(state.modal, Some(Modal::Help)),
        "a non-close key is swallowed but keeps help open"
    );
    assert!(
        sw.feed_reader_key(b"\x1b[A", &mut state),
        "an arrow (ESC [) is swallowed, not a close"
    );
    assert!(
        matches!(state.modal, Some(Modal::Help)),
        "arrow keeps help open"
    );

    assert!(sw.feed_reader_key(b"q", &mut state), "q → consumed");
    assert!(!matches!(state.modal, Some(Modal::Help)), "q closes help");

    sw.toggle_help(&mut state);
    assert!(
        sw.feed_reader_key(b"\x1b", &mut state),
        "lone Esc → consumed"
    );
    assert!(!matches!(state.modal, Some(Modal::Help)), "Esc closes help");
}

#[tokio::test]
async fn input_renders_in_the_hint_bar() {
    // The input is not a centered popup any more: it lives in the hint bar, which
    // floats across the window (like the armed bar) and reads `[filter] filter
    // sessions: <buffer>` on its bottom row. No bordered box appears anywhere.
    let mut h = Harness::new(sample());
    h.ch('/').await; // open the filter input
    assert!(h.state.is_inputting(), "input open");
    let w = h.buf().area.width;
    let last = h.buf().area.height - 1;
    let bottom: String = (0..w).map(|x| h.buf()[(x, last)].symbol()).collect();
    assert!(
        bottom.contains("[filter] filter sessions · 4 matches · 0 hidden hosts:"),
        "the bar shows the feature head and guide: {bottom:?}"
    );
    let whole: String = (0..h.buf().area.height)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| h.buf()[(x, y)].symbol().to_string())
        .collect();
    assert!(!whole.contains('╭'), "no popup box is drawn anywhere");
    // Typing lands in the bar's input area.
    h.ch('b').await;
    h.ch('u').await;
    let bottom: String = (0..w).map(|x| h.buf()[(x, last)].symbol()).collect();
    assert!(
        bottom.contains(": bu"),
        "typed text lands in the bar: {bottom:?}"
    );
}

#[tokio::test]
async fn input_esc_cancels_without_acting() {
    // `n` starts a session on a REACHABLE host card, so the fixture is one empty host.
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![],
        }],
    });
    h.ch('n').await;
    assert!(h.state.is_inputting(), "input open");
    h.key(KeyCode::Esc).await;
    assert!(!h.state.is_inputting(), "Esc closes the input");
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "Esc must not create anything"
    );
}

#[test]
fn a_renamed_session_keeps_the_selection_and_the_displayed_record() {
    // The selected, displayed session is renamed under the user: the card they are on
    // is still the card they are on, under its new name, and the selection and the
    // displayed record follow it, so nothing reads the rename as a move elsewhere.
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 1, false), sess("jup", "zeta", 1, false)],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    sw.user_moved = true;
    assert!(matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "api"));
    // The loop syncs the selection off the switcher; stand in for it.
    state.selection = crate::model::Selection {
        source: "jup".into(),
        session: "api".into(),
    };
    state.displayed = state.selection.clone();

    let renamed = sw.apply_source_result(
        "jup".into(),
        vec![sess("jup", "web", 1, false), sess("jup", "zeta", 1, false)],
        None,
        &mut state,
    );
    assert_eq!(renamed, Some(("api".into(), "web".into())));
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "web"),
        "the selection stays on the renamed card"
    );
    assert_eq!(state.selection.session, "web");
    assert_eq!(state.displayed.session, "web");
}

#[test]
fn selection_survives_a_rebuild() {
    // Selection on jup/api's card survives a bare rebuild (the same node, so the
    // selection stays put).
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state); // launch preselects the api card
    sw.user_moved = true;
    assert!(matches!(sw.current_ref(), Some(RowRef::Session { .. })));
    sw.rebuild(&mut state);
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "api"),
        "the card survives a rebuild"
    );
}

#[test]
fn render_nav_width_zero_gives_terminal_full_width() {
    use crate::display::grid::Grid;
    // A two-source skeleton is enough. With nav_width == 0 the tree column and
    // its view border are gone, so the terminal view owns the left edge (x=0): the
    // live grid's content begins at column 0.
    let mut state = crate::state::State::from_sources(vec!["local".into(), "jupiter06".into()]);
    let sw = Switcher::from_sources(&mut state);
    // 60 wide keeps the 20-wide nav in its column (39 against 20 rows counted double).
    let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
    let mut g = Grid::new(10, 60);
    g.feed(b"EDGE-CONTENT");

    // nav_width == 0 → no tree column, no view border: the terminal view starts at x=0.
    term.draw(|f| sw.render_test(f, Some(&g), true, NavSize::hidden(NAV_WIDTH), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    // Column 0 row 0 must NOT be the view border rule '│' (the view border is gone).
    assert_ne!(
        buf[(0, 0)].symbol(),
        "│",
        "view border must be absent when tree hidden"
    );
    // The live grid content begins at x=0, proving the terminal view owns the left edge.
    let row0: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(
        row0.starts_with("EDGE-CONTENT"),
        "terminal view fills row 0 from x=0: {row0:?}"
    );

    // Sanity: with a normal width the view border rule IS present at the tree edge.
    term.draw(|f| sw.render_test(f, Some(&g), true, NavSize::visible(20), &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        buf[(20, 0)].symbol(),
        "│",
        "view border present at x=nav_width when shown"
    );
}

#[test]
fn mux_cursor_maps_into_terminal_view_area() {
    use ratatui::layout::{Position, Rect};
    let pos = terminal_cursor_pos(Rect::new(49, 0, 80, 24), (3, 2));
    assert_eq!(pos, Position { x: 52, y: 2 });
    // clamped to the area:
    let pos = terminal_cursor_pos(Rect::new(49, 0, 4, 2), (100, 100));
    assert_eq!(pos, Position { x: 52, y: 1 });
}

#[test]
fn help_lines_reflects_configured_prefix() {
    // The focus-section rows must show the active prefix, not a hardcoded "C-g".
    let mut state = crate::state::State::default();
    state.chrome.set_ui_prefix("C-Space".into());
    let palette = crate::ui::palette::Palette::default();
    let (_title, lines) = modal::help_lines(
        &state.chrome.ui_prefix,
        crate::ui::switcher::NavPosition::Left,
        &palette,
    );
    let text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("C-Space"),
        "custom prefix must appear in help:\n{text}"
    );
    assert!(
        !text.contains("C-g"),
        "hardcoded C-g must not appear when prefix is C-Space:\n{text}"
    );

    // Default prefix (no setter) must still show C-g.
    let state_default = crate::state::State::default();
    let (_title, lines_default) = modal::help_lines(
        &state_default.chrome.ui_prefix,
        crate::ui::switcher::NavPosition::Left,
        &palette,
    );
    let text_default: String = lines_default
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text_default.contains("C-g"),
        "default prefix C-g must appear in help:\n{text_default}"
    );
}

#[test]
fn select_address_moves_cursor_to_named_session() {
    use crate::session::Session;
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    source: "jup".into(),
                    name: "api".into(),
                    mux: String::new(),
                    windows: 1,
                    attached: false,
                },
                Session {
                    source: "jup".into(),
                    name: "db".into(),
                    mux: String::new(),
                    windows: 1,
                    attached: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    // Selection starts on the first session row (api). Jump to db by address.
    assert!(
        sw.select_address(&crate::session::Address::new("jup", "db"), &state),
        "moved to jup/db"
    );
    assert_eq!(sw.terminal_view_target().target, "db");
    // Already-there → no move; unknown address → no move, selection unchanged.
    assert!(
        !sw.select_address(&crate::session::Address::new("jup", "db"), &state),
        "already on jup/db"
    );
    assert!(
        !sw.select_address(&crate::session::Address::new("jup", "ghost"), &state),
        "no such session row"
    );
    assert_eq!(
        sw.terminal_view_target().target,
        "db",
        "selection unchanged on a miss"
    );
}

#[test]
fn fit_selects_by_display_width() {
    // "한국" has display width 4. A budget of 3 cannot fit it; a budget of 4 can.
    let cands = vec!["한국".to_string(), "x".to_string()];
    assert_eq!(
        fit(&cands, 3),
        "x",
        "width-4 candidate rejected at budget 3"
    );
    assert_eq!(
        fit(&cands, 4),
        "한국",
        "width-4 candidate accepted at budget 4"
    );
}

// --- the portrait band's column flow ------------------------------------

/// `n` sources of two sessions each, named so every card is the same width.
fn column_flow_scan(sources: &[&str], name_len: usize) -> Scan {
    let counts: Vec<(&str, usize)> = sources.iter().map(|s| (*s, 2)).collect();
    column_flow_scan_sized(&counts, name_len)
}

/// Sources carrying the given session counts, every card the same width. The counts
/// set each host/mux RUN's height (one expanded card over the rest collapsed), which
/// is what the column flow packs.
fn column_flow_scan_sized(sources: &[(&str, usize)], name_len: usize) -> Scan {
    let pad = "x".repeat(name_len.saturating_sub(2));
    let mut out: Vec<(&str, Vec<Session>)> = Vec::new();
    for (src, n) in sources {
        let mut sessions = Vec::new();
        for k in 0..*n {
            sessions.push(sess(src, &format!("{src}{pad}{k}"), 1, false));
        }
        out.push((*src, sessions));
    }
    sources_scan(out)
}

/// Renders `scan` into a `w`x`h` portrait backend and returns the switcher, so a test
/// can read the card rects the frame plan recorded.
fn portrait(scan: Scan, w: u16, h: u16) -> (Switcher, RenderPlan, Terminal<TestBackend>) {
    let mut state = crate::state::State::from_scan(scan);
    let sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let area = Rect::new(0, 0, w, h);
    let plan = sw.layout(
        area,
        auto_nav(NAV_WIDTH, area),
        &state,
        &RenderPlan::default(),
    );
    term.draw(|f| sw.render(f, None, false, &state, &plan))
        .unwrap();
    assert_eq!(
        plan.layout,
        ViewLayout::Band,
        "the backend must be portrait"
    );
    (sw, plan, term)
}

/// Card rects by card index in one frame's layout.
fn cells_of(plan: &RenderPlan) -> std::collections::HashMap<usize, Rect> {
    plan.nav_cells.iter().map(|(i, r)| (*i, *r)).collect()
}

#[test]
fn the_portrait_band_flows_cards_down_then_right() {
    // A three-row band: each source's section (a title over its two sessions) fills a
    // column exactly, so the next source opens the column to its right. Reading order is
    // the fill order - down a column, then right - which is what the numbers count in.
    let (_sw, plan, _t) = portrait(column_flow_scan(&["aa", "bb", "cc"], 2), 60, 12);
    let cells = cells_of(&plan);
    assert_eq!(cells.len(), 9, "every row is placed: {cells:?}");
    for base in [0usize, 3, 6] {
        let (title, a, b) = (cells[&base], cells[&(base + 1)], cells[&(base + 2)]);
        // One column, but a session card starts at the group indent while the
        // title it hangs under holds the column's left edge.
        assert_eq!(a.x, title.x + CARD_INDENT, "a source's rows share a column");
        assert_eq!(b.x, a.x, "and the session cards line up with each other");
        assert_eq!(title.y, 0, "the section title starts its column");
        assert_eq!(title.height, 1, "a title is one row");
        assert_eq!(a.y, 1, "the first session hangs directly under it");
        assert_eq!(a.height, 1, "a session card is one row");
        assert_eq!(b.y, 2, "the second session under that");
    }
    assert!(
        cells[&0].x < cells[&3].x && cells[&3].x < cells[&6].x,
        "later sources open columns to the right: {cells:?}"
    );
}

#[tokio::test]
async fn moving_selection_does_not_reflow_host_cards_in_a_band() {
    let scan = Scan {
        groups: [
            (
                "alpha",
                "dev@alpha: Permission denied (publickey,password).",
            ),
            ("bravo", "connection refused"),
            ("charlie", "connection refused"),
        ]
        .into_iter()
        .map(|(source, error)| Group {
            source: source.into(),
            err: Some(error.into()),
            sessions: vec![],
        })
        .collect(),
    };
    let mut h = Harness::new_sized(scan, 60, 12);
    assert_eq!(h.plan.layout, ViewLayout::Band);
    let before = cells_of(&h.plan);
    h.key(KeyCode::Down).await;
    assert_eq!(cells_of(&h.plan), before, "selection keeps every card rect");
}

#[test]
fn a_column_holds_whole_sections() {
    // An eight-row band holds two three-row sections with TWO rows to spare - room for
    // the third section's title, but not for the section. It moves right ENTIRE rather
    // than leaving a card behind at the foot of the column: a source's rows stay
    // together, and the title naming them stays at the top of them.
    let (_sw, plan, _t) = portrait(column_flow_scan(&["aa", "bb", "cc"], 2), 60, 21);
    let cells = cells_of(&plan);
    assert_eq!(cells.len(), 9);
    let x0 = cells[&0].x;
    assert_eq!(cells[&3].x, x0, "both titles hold the column's left edge");
    for i in [1usize, 2, 4, 5] {
        assert_eq!(
            cells[&i].x,
            x0 + CARD_INDENT,
            "sections one and two share the first column, their cards past the strip"
        );
    }
    assert_eq!(
        cells[&3].y, 3,
        "the second section follows the first down the column"
    );
    assert!(
        cells[&6].x > x0,
        "the section that does not fit starts a column instead of splitting: {cells:?}"
    );
    assert_eq!(cells[&6].y, 0, "at the top of it");
    assert_eq!(
        cells[&7].x,
        cells[&6].x + CARD_INDENT,
        "with its sessions under it"
    );
}

#[test]
fn the_portrait_band_parts_sessions_left_and_hosts_right() {
    // The host band never shares a column with session cards, and while the band has
    // room it is pushed to the RIGHT edge, blank columns parting it from the sessions -
    // the portrait transpose of the side list's top/bottom parting (point 5).
    let scan = Scan {
        groups: vec![
            Group {
                source: "aa".into(),
                err: None,
                sessions: vec![sess("aa", "a0", 1, false), sess("aa", "a1", 1, false)],
            },
            Group {
                source: "bb".into(),
                err: None,
                sessions: vec![sess("bb", "b0", 1, false), sess("bb", "b1", 1, false)],
            },
            Group {
                source: "dead".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ],
    };
    let (_sw, plan, term) = portrait(scan, 60, 12);
    let cells = cells_of(&plan);
    // Two sections (6 rows) + one host card.
    assert_eq!(cells.len(), 7, "every row is placed: {cells:?}");
    let host = cells[&6];
    let sess = cells[&0];
    assert!(
        host.x > sess.x,
        "the host card is in a column of its own, right of the sessions"
    );
    // The gap parting pushes the host against the band's right edge.
    let band_w = term.backend().buffer().area.width;
    assert_eq!(
        host.x + host.width,
        band_w,
        "the host band sits flush against the right edge (gap parting)"
    );
    assert!(host.x > sess.x + sess.width, "blank columns part the bands");
}

#[test]
fn portrait_scanning_hosts_anchor_to_the_right_until_found() {
    // Before ANY session is found, the portrait band is the host band ALONE: it anchors
    // to the RIGHT edge, the blank columns left of it being where found sessions land.
    let scan = Scan {
        groups: vec![
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                source: "prod".into(),
                err: None,
                sessions: vec![],
            },
        ],
    };
    let (_sw, plan, term) = portrait(scan, 60, 12);
    let cells = cells_of(&plan);
    let band_w = term.backend().buffer().area.width;
    let x0 = cells[&0].x;
    assert!(
        x0 > 0,
        "the host band leaves blank columns on the left:\n{cells:?}"
    );
    assert_eq!(
        cells[&0].x + cells[&0].width,
        band_w,
        "and sits flush against the right edge"
    );
    for i in 1..3 {
        assert_eq!(cells[&i].x, x0, "every scanning host shares that column");
    }
}

#[test]
fn the_hidden_columns_are_counted_on_the_seam() {
    // Columns too wide to all fit leave cards off screen. The seam says which way they
    // went and how many, at the end they went off: the count is in CARDS, because what
    // the reader is hunting for is a session, not a column. The count costs no row: every
    // band row holds cards, and the seam row is the line the nav already draws.
    let (_sw, _plan, mut term) = portrait(
        column_flow_scan_sized(&[("aa", 2), ("bb", 3), ("cc", 2)], 26),
        60,
        20,
    );
    let seam_y = 8; // the band is 8 rows of cards, then the seam
    let row = |t: &Terminal<TestBackend>| -> String {
        let buf = t.backend().buffer();
        (0..buf.area.width)
            .map(|x| buf[(x, seam_y)].symbol())
            .collect()
    };
    let at_left = row(&term);
    assert!(
        at_left.trim_end().ends_with("C-g"),
        "the prefix owns the far end of the seam: {at_left:?}"
    );
    assert!(
        at_left.contains(" \u{203a}"),
        "the cards off to the right are counted at that end: {at_left:?}"
    );
    assert!(
        !at_left.contains('\u{2039}'),
        "nothing is off to the left from the first column: {at_left:?}"
    );
    // The prefix sits on its own background, sized to itself; the counts do not.
    let bar_bg = crate::ui::palette::Palette::default().bar_bg;
    let buf = term.backend().buffer();
    let lit = (0..buf.area.width)
        .filter(|x| buf[(*x, seam_y)].bg == bar_bg)
        .count();
    assert!(
        lit > 0 && lit < buf.area.width as usize / 2,
        "the prefix is a label, not a slab: {lit} of {} cells",
        buf.area.width
    );
    // Walk to the last card: now the hidden columns are behind us, so the count swaps ends.
    let mut state = crate::state::State::from_scan(column_flow_scan_sized(
        &[("aa", 2), ("bb", 3), ("cc", 2)],
        26,
    ));
    let mut sw = Switcher::new(&mut state);
    sw.move_to(-1, &state);
    term.draw(|f| sw.render_test(f, None, false, auto_nav(NAV_WIDTH, f.area()), &state))
        .unwrap();
    let at_right = row(&term);
    assert!(
        at_right.contains("\u{2039} "),
        "the cards left behind are counted at the left end: {at_right:?}"
    );
}
#[test]
fn the_portrait_prefix_is_a_label_until_the_prefix_is_armed() {
    // Nothing off screen, so the seam carries only the prefix. It paints only what it
    // has to say plus a cell of padding: a full-width slab of bar colour across a wide
    // window is a lot of paint for one word.
    let (_sw, _plan, mut term) = portrait(column_flow_scan(&["aa", "bb", "cc"], 2), 60, 20);
    let bar_bg = crate::ui::palette::Palette::default().bar_bg;
    let seam_y = 8;
    {
        let buf = term.backend().buffer();
        let row: String = (0..buf.area.width)
            .map(|x| buf[(x, seam_y)].symbol())
            .collect();
        assert!(row.contains("C-g"), "the seam names the prefix: {row:?}");
        assert_eq!(
            buf[(buf.area.width - 1, seam_y)].bg,
            bar_bg,
            "on its own background at the right end: {row:?}"
        );
        let lit = (0..buf.area.width)
            .filter(|x| buf[(*x, seam_y)].bg == bar_bg)
            .count();
        assert!(
            lit < buf.area.width as usize / 2,
            "sized to the content, not the row: {lit} of {} cells",
            buf.area.width
        );
    }
    // Arming the prefix opens the cheatsheet below the seam, across the whole width: it
    // has to be readable over everything it now covers, and the seam keeps the prefix.
    let mut state = crate::state::State::from_scan(column_flow_scan(&["aa", "bb", "cc"], 2));
    let sw = Switcher::new(&mut state);
    state.chrome.set_armed(true);
    term.draw(|f| sw.render_test(f, None, false, auto_nav(NAV_WIDTH, f.area()), &state))
        .unwrap();
    let buf = term.backend().buffer();
    let text = |y: u16| {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    };
    assert!(
        (0..buf.area.width).all(|x| buf[(x, seam_y + 1)].bg == bar_bg),
        "the armed bar fills the row below the seam: {:?}",
        text(seam_y + 1)
    );
    assert!(text(seam_y + 1).contains("C-g"), "{:?}", text(seam_y + 1));
    assert!(
        text(seam_y).trim_end().ends_with("C-g"),
        "the seam keeps the prefix: {:?}",
        text(seam_y)
    );
}
#[test]
fn the_side_lists_overflow_thickens_the_seam_and_spares_every_card() {
    // When the side list overflows, the seam beside the cards on screen turns heavy. No
    // column of the nav is given up for it, so the cards keep the nav's full width.
    let mut state = crate::state::State::from_scan(column_flow_scan(&["aa", "bb", "cc"], 2));
    let sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 8)).unwrap();
    let plan = sw.layout(
        Rect::new(0, 0, 140, 8),
        NavSize::visible(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    term.draw(|f| sw.render(f, None, false, &state, &plan))
        .unwrap();
    assert_eq!(plan.layout, ViewLayout::Column);
    let buf = term.backend().buffer();
    let seam: String = (0..buf.area.height)
        .map(|y| buf[(NAV_WIDTH, y)].symbol())
        .collect();
    assert!(seam.contains('┃'), "the seam thickens: {seam:?}");
    let thumb = plan.seam_thumb;
    let thick_rows: Vec<u16> = (0..buf.area.height)
        .filter(|&y| buf[(NAV_WIDTH, y)].symbol() == "┃")
        .collect();
    assert_eq!(
        thick_rows,
        (thumb.y..thumb.bottom()).collect::<Vec<_>>(),
        "the thick segment covers exactly the thumb's rows: {seam:?}"
    );
    assert!(
        thick_rows.len() < plan.nav_inner.height as usize,
        "the thumb is a proportion of the card rows, not all of them: {seam:?}"
    );
    assert!(
        seam.contains('│'),
        "only where the cards on screen are: {seam:?}"
    );
    let selected = sw.selected;
    let sel_rect = plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == selected)
        .map(|(_, r)| *r)
        .unwrap();
    assert_eq!(
        sel_rect.right(),
        NAV_WIDTH,
        "the selected card reaches the nav's last column"
    );
    assert!(
        buf[(sel_rect.x, sel_rect.y)]
            .modifier
            .contains(Modifier::REVERSED),
        "the selected card itself is still inverted"
    );
}

#[tokio::test]
async fn a_host_card_names_its_mux_even_where_the_id_does_not() {
    // A machine serving ONE mux carries no mux in its source id. The card still names it:
    // a machine that reads `local/psmux` on one card and `local` on the next reads as two
    // machines.
    let mut h = Harness::from_sources(&["local"]);
    h.state.chrome.set_source_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result("local".into(), vec![], None, &mut h.state);
    h.draw();
    let out = h.text();
    assert!(
        out.contains("local/psmux"),
        "the host card names the machine and its mux:\n{out}"
    );
}

#[tokio::test]
async fn a_session_with_no_stamped_mux_takes_its_source_mux() {
    // A session created since the last enumeration carries no mux of its own. Its card
    // takes the source's, so it does not stand out from the cards beside it.
    let mut h = Harness::from_sources(&["local"]);
    h.state.chrome.set_source_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![sess_mux("local", "fresh", "")],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.text();
    assert!(
        out.contains("local/psmux"),
        "the context line names the mux anyway:\n{out}"
    );
}

#[tokio::test]
async fn a_host_screen_headline_reads_as_host_over_mux() {
    let mut h = Harness::from_sources(&["prod:zellij"]);
    h.state.chrome.set_source_reach(
        [(
            "prod:zellij".to_string(),
            reach("zellij", "ssh to prod", "", "ssh -- prod zellij ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result(
        "prod:zellij".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    h.draw();
    let out = h.view_text();
    assert!(
        out.contains("prod/zellij"),
        "the headline is the label:\n{out}"
    );
    assert!(
        !out.contains("prod:zellij"),
        "an id's own separator never reaches a screen:\n{out}"
    );
}

/// A reach entry for `source`, so a screen test states what the app would have resolved.
/// A host xmux never reached is offered under the mux it WOULD have tried. That guess
/// must not reach the screen wearing the grammar every confirmed pair wears: the screen
/// for such a host reads the host alone.
#[tokio::test]
async fn a_host_that_answered_nothing_headlines_without_a_mux() {
    let mut h = Harness::from_sources(&["prod"]);
    // The reach record carries the mux that was ASKED FOR, which is what the diagnostic
    // rows state; it is not an answer, and the headline must not read it as one.
    h.state.chrome.set_source_reach(
        [(
            "prod".to_string(),
            reach("tmux", "ssh to prod", "", "ssh -- prod tmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    h.draw();
    let out = h.view_text();
    assert!(
        out.lines().any(|l| l.trim() == "prod"),
        "the headline is the host alone:\n{out}"
    );
    assert!(
        !out.contains("prod/tmux"),
        "no mux is claimed for a host that answered nothing:\n{out}"
    );
    // What was ASKED is still stated, in the rows that say what was tried.
    assert!(
        out.contains("tmux"),
        "the diagnostic still says what it tried:\n{out}"
    );
}

/// A host that answered enumerated THROUGH its mux, so the pair is a fact and the screen
/// reads it. An empty host answered - having no session is an answer.
#[tokio::test]
async fn a_host_that_answered_headlines_with_its_mux() {
    let mut h = Harness::from_sources(&["fresh"]);
    h.state.chrome.set_source_reach(
        [(
            "fresh".to_string(),
            reach("tmux", "ssh to fresh", "", "ssh -- fresh tmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result("fresh".into(), vec![], None, &mut h.state);
    h.draw();
    let out = h.view_text();
    assert!(
        out.contains("fresh/tmux"),
        "an answered host's screen names the pair:\n{out}"
    );
}

fn reach(mux: &str, machine: &str, socket: &str, probe: &str) -> crate::ui::chrome::SourceReach {
    crate::ui::chrome::SourceReach {
        probe: probe.into(),
        machine: machine.into(),
        mux: mux.into(),
        // The binary a test names IS its kind: no test reaches a mux through an alias.
        kind: mux.into(),
        socket: socket.into(),
    }
}

/// Selects the first unreachable host card, whatever else the nav holds.
async fn select_unreachable_host(h: &mut Harness) {
    h.key(KeyCode::End).await;
    for _ in 0..64 {
        if matches!(
            h.sw.current_ref(),
            Some(RowRef::Host {
                unreachable: true,
                ..
            })
        ) {
            return;
        }
        h.key(KeyCode::Up).await;
    }
    panic!("no unreachable host card in the nav");
}

#[tokio::test]
async fn unreachable_host_screen_states_what_was_asked_and_over_what() {
    // The message alone says a host failed, not what xmux asked of it. The mux and the
    // machine are separate rows because they are the two things that can be wrong
    // independently, and the probe is the command itself, so the user can run it by hand
    // instead of taking the app's word for the failure.
    let mut h = Harness::from_sources(&["prod"]);
    h.state.chrome.set_source_reach(
        [(
            "prod".to_string(),
            reach(
                "tmux",
                "ssh to prod, given 5s to connect",
                "/tmp/cm-prod.sock",
                "ssh -o BatchMode=yes -- prod tmux list-sessions",
            ),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    for want in [
        "mux",
        "tmux",
        "machine",
        "ssh to prod, given 5s to connect",
        "socket",
        "/tmp/cm-prod.sock",
        "probe",
        "prod tmux list-sessions",
    ] {
        assert!(out.contains(want), "the screen states {want:?}:\n{out}");
    }
}

#[tokio::test]
async fn a_source_nothing_was_resolved_for_gets_no_reach_rows() {
    // An empty map is not "reached by nothing": it is nothing resolved. Those rows are
    // absent rather than blank, the provider row's own rule, so the screen never names a
    // datum it does not have.
    let mut h = Harness::from_sources(&["prod"]);
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    for absent in ["probe", "socket", "machine"] {
        assert!(!out.contains(absent), "no {absent:?} row:\n{out}");
    }
    assert!(
        out.contains("connection refused"),
        "the reason stands:\n{out}"
    );
}

#[tokio::test]
async fn unreachable_host_screen_names_the_other_muxes_on_the_machine() {
    // Which HALF is down is the question a bare error cannot answer: a sibling mux on the
    // same machine serving sessions says the box is up and this mux is not. The row
    // carries each sibling's own state, so the answer is on the screen rather than being
    // something the user reconstructs from the nav.
    let mut h = Harness::from_sources(&["prod:tmux", "prod:zellij", "local"]);
    h.sw.apply_source_result(
        "prod:zellij".into(),
        vec![sess_mux("prod:zellij", "infer", "zellij")],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result("local".into(), vec![], None, &mut h.state);
    h.sw.apply_source_result(
        "prod:tmux".into(),
        vec![],
        Some("no server running".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    h.draw();
    let out = h.view_text();
    assert!(out.contains("same machine"), "the row is named:\n{out}");
    assert!(
        out.contains("prod/zellij · 1 session"),
        "and states the sibling's own answer, in the label's grammar:\n{out}"
    );
    assert!(
        !out.contains("prod:zellij"),
        "an id's own separator never reaches the screen:\n{out}"
    );
    assert!(
        !out.contains("local ·"),
        "a source on ANOTHER machine is not a sibling:\n{out}"
    );
}

#[tokio::test]
async fn unreachable_host_screen_separates_a_standing_failure_from_a_blip() {
    // One failed sweep and a host that has not answered since launch read identically in
    // the message. The run length is what parts them, and it clears the moment the host
    // answers - a stale count would keep calling a live host a standing failure.
    let mut h = Harness::from_sources(&["prod"]);
    for _ in 0..3 {
        h.sw.apply_source_result(
            "prod".into(),
            vec![],
            Some("connection refused".into()),
            &mut h.state,
        );
    }
    h.draw();
    let out = h.view_text();
    assert!(out.contains("failures"), "the row is named:\n{out}");
    assert!(out.contains("3 in a row"), "and counts them:\n{out}");

    h.sw.apply_source_result(
        "prod".into(),
        vec![sess("prod", "editor", 1, false)],
        None,
        &mut h.state,
    );
    assert!(
        !h.state.failure_runs.contains_key("prod"),
        "an answer clears the run: {:?}",
        h.state.failure_runs
    );
}

#[tokio::test]
async fn unreachable_host_screen_names_the_log_file() {
    // Everything xmux dispatched and what came back is written down. The screen names the
    // file, so the full history is findable rather than being something the user has to
    // already know about.
    let mut h = Harness::from_sources(&["prod"]);
    h.state
        .chrome
        .set_log_path("/home/h/.xmux/xmux.log.<date>".into());
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    assert!(out.contains("log"), "the row is named:\n{out}");
    assert!(
        out.contains("/home/h/.xmux/xmux.log.<date>"),
        "and carries the path:\n{out}"
    );
}

#[tokio::test]
async fn moving_into_the_terminal_view_from_a_session_card_hides_the_host_band() {
    let mut h = Harness::new(scan_with_a_host_band());
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Session { .. })));
    h.sw.sync_view_focus(true);
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        !nav.contains("db-2") && !nav.contains("db-3"),
        "no host card is painted:\n{nav}"
    );
    assert!(nav.contains("editor"), "the session cards stay:\n{nav}");
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    assert!(
        h.plan.nav_cells.iter().all(|(i, _)| *i < boundary),
        "a hidden card takes no click"
    );
    h.sw.sync_view_focus(false);
    h.draw();
    assert!(
        h.nav_cards_text().contains("db-2"),
        "the move back into the nav shows the band again"
    );
}

#[tokio::test]
async fn moving_into_the_terminal_view_from_a_host_card_keeps_the_host_band() {
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await; // local → jupiter00
    h.key(KeyCode::Right).await; // jupiter00 → the band
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { .. })));
    h.sw.sync_view_focus(true);
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        nav.contains("db-2") && nav.contains("db-3"),
        "the host band stays:\n{nav}"
    );
}

#[tokio::test]
async fn the_decision_holds_while_the_terminal_view_keeps_the_focus() {
    // Decided once, on the move: a later sync with the terminal view still focused does
    // not re-decide, whatever the selection is by then.
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await;
    h.key(KeyCode::Right).await; // a host card
    h.sw.sync_view_focus(true);
    h.key(KeyCode::Left).await; // back onto a session card, the band still shown
    h.sw.sync_view_focus(true);
    h.draw();
    assert!(h.nav_cards_text().contains("db-2"), "the band stays shown");
}

#[tokio::test]
async fn a_live_prefix_paints_the_hidden_host_band_until_it_ends() {
    // The hint bar offers a jump to any card by number, so the cards it can reach are on
    // screen while the prefix lasts; the band stays hidden underneath and returns to
    // hidden once the prefix ends.
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    h.draw();
    assert!(!h.nav_cards_text().contains("db-2"));
    h.sw.sync_prefix(true);
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        nav.contains("db-2") && nav.contains("db-3"),
        "the prefix paints the host band:
{nav}"
    );
    h.sw.sync_prefix(false);
    h.draw();
    assert!(
        !h.nav_cards_text().contains("db-2"),
        "the band is hidden again once the prefix ends"
    );
}

#[tokio::test]
async fn a_hidden_band_shows_again_once_the_selection_lands_in_it() {
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    h.draw();
    assert!(!h.nav_cards_text().contains("db-2"));
    h.key(KeyCode::Right).await;
    h.key(KeyCode::Right).await; // the selection reaches the band
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { .. })));
    let nav = h.nav_cards_text();
    assert!(
        nav.contains("db-2"),
        "a selected card is never hidden:\n{nav}"
    );
}

#[test]
fn an_empty_row_list_shows_the_host_band_again() {
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let mut sw = Switcher::new(&mut state);
    assert!(sw.rows.is_empty(), "no group yields no row");
    sw.host_band_hidden = true;
    sw.set_selected(0, &state);
    assert!(
        !sw.host_band_hidden,
        "with no session row selected the host band shows again"
    );
}
