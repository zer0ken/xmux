use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;
use std::sync::Mutex;
use unicode_width::UnicodeWidthStr;

use super::tests_support::auto_nav;

/// A cell painted in the hard selection's look under the default `auto-dark` theme: the
/// LightGreen accent behind it.
fn on_accent(cell: &ratatui::buffer::Cell) -> bool {
    cell.bg == Color::LightGreen
}

// --- mock ops -----------------------------------------------------------

#[derive(Default)]
struct RecordOps {
    created: Mutex<Vec<String>>,
    logged_in: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Ops for RecordOps {
    fn hosts(&self) -> Vec<String> {
        Vec::new()
    }
    async fn list_sessions(&self, _host: &str) -> anyhow::Result<Vec<Session>> {
        Ok(Vec::new())
    }
    async fn new_session(&self, host: &str, name: &str) -> anyhow::Result<Session> {
        self.created.lock().unwrap().push(format!("{host}/{name}"));
        Ok(Session {
            host: host.into(),
            name: name.into(),
            windows: 1,
            ..Default::default()
        })
    }
    async fn login_command(
        &self,
        host: &str,
        _login: &crate::transport::Login,
        _password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>> {
        self.logged_in.lock().unwrap().push(host.to_string());
        // A child that exits 0 at once: the conversation this stands in for is one that
        // needed nothing typed.
        Ok(Some(crate::transport::CommandSpec::from_argv(vec![
            "true".to_string()
        ])))
    }
    fn write_login_stanza(
        &self,
        _host: &str,
        _login: &crate::transport::Login,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn register_login_key(
        &self,
        _host: &str,
        _login: &crate::transport::Login,
        _register: crate::ui::ops::KeyRegistration,
    ) -> crate::ui::ops::RegistrationOutcome {
        crate::ui::ops::RegistrationOutcome::NotRequested
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

    fn from_hosts(aliases: &[&str]) -> Self {
        let backend = TestBackend::new(140, 30);
        let term = Terminal::new(backend).unwrap();
        let aliases = aliases.iter().map(|s| s.to_string()).collect();
        let mut state = crate::state::State::from_hosts(aliases);
        let mut h = Harness {
            sw: Switcher::from_hosts(&mut state),
            plan: RenderPlan::default(),
            state,
            term,
            ops: RecordOps::default(),
        };
        h.draw();
        h
    }

    /// The hint bar's row, read at the width it actually paints: the nav column at
    /// rest, the whole window while a floating bar (a selection hint) is up. Reading the nav width unconditionally would clip the floating bar.
    fn hint_bar_text(&self) -> String {
        let buf = self.buf();
        let y = buf.area.height - 1;
        let limit = if hint_bar_floats(&self.state) {
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

    /// Draws with the terminal view holding focus, the state in which the login pane
    /// takes keys.
    fn draw_terminal_focused(&mut self) {
        let sw = &self.sw;
        let state = &self.state;
        let previous = self.plan.clone();
        let mut next = None;
        self.term
            .draw(|f| {
                let plan = sw.layout(f.area(), auto_nav(NAV_WIDTH, f.area()), state, &previous);
                sw.render(f, None, true, state, &plan);
                next = Some(plan);
            })
            .unwrap();
        self.plan = next.expect("draw produced a render plan");
    }

    /// The terminal-view row holding `text`, with the style of its first cell.
    fn view_cell_of(&self, text: &str) -> Option<(u16, ratatui::style::Style)> {
        let buf = self.buf();
        let first = NAV_WIDTH + 1;
        let needle: Vec<char> = text.chars().collect();
        for y in 0..buf.area.height {
            let mut x = first;
            while (x as usize) + needle.len() <= buf.area.width as usize {
                if needle
                    .iter()
                    .enumerate()
                    .all(|(i, &c)| buf[(x + i as u16, y)].symbol() == c.to_string())
                {
                    return Some((y, buf[(x, y)].style()));
                }
                x += 1;
            }
        }
        None
    }

    /// The terminal-view text of row `y`, trimmed.
    fn view_row(&self, y: u16) -> String {
        let buf = self.buf();
        (NAV_WIDTH + 1..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
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

    /// Ctrl held with `code`: Ctrl+↑/↓ walk the hierarchy.
    fn ctrl(&mut self, code: KeyCode) {
        self.sw
            .handle_key(KeyEvent::new(code, KeyModifiers::CONTROL), &mut self.state);
        self.draw();
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

    /// Row `i` of the open popup, its top border being row 0; empty with no popup.
    fn popup_row(&self, i: u16) -> String {
        let r = self.plan.popup_rect;
        if r.is_empty() || i >= r.height {
            return String::new();
        }
        let buf = self.buf();
        (r.x..r.right())
            .map(|x| buf[(x, r.y + i)].symbol().to_string())
            .collect()
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

fn sess(host: &str, name: &str, windows: i64, attached: bool) -> Session {
    Session {
        host: host.into(),
        name: name.into(),
        mux: String::new(),
        id: String::new(),
        windows,
        attached,
        stopped: false,
    }
}

/// Adds a decoy session on a host whose name sorts first, so the card under test is
/// NOT the selected one. The selected card is painted in the accent pair, which flattens
/// every level colour on it by design, so a colour assertion has to read an unselected
/// card. The decoy's own words (`aaa`, `parked`, `psmux`) collide with no needle in
/// these tests.
fn selection_parked_elsewhere(mut scan: Scan) -> Scan {
    scan.groups.push(Group {
        host: "aaa".into(),
        err: None,
        sessions: vec![sess_mux("aaa", "parked", "psmux")],
    });
    scan
}

fn sample() -> Scan {
    let groups = vec![
        Group {
            host: "local".into(),
            err: None,
            sessions: vec![
                sess("local", "editor", 2, true),
                sess("local", "build", 1, false),
            ],
        },
        Group {
            host: "jupiter00".into(),
            err: None,
            sessions: vec![sess("jupiter00", "inference", 1, false)],
        },
        Group {
            host: "db-2".into(),
            err: Some("connection timed out".into()),
            sessions: vec![],
        },
    ];
    Scan { groups }
}

/// Two hosts with a session each and TWO with none, so the host band holds more than
/// one card. Used where the band's own SIZE is the point: to ←/→ it is one category
/// however many cards it holds.
fn scan_with_a_host_band() -> Scan {
    Scan {
        groups: vec![
            Group {
                host: "local".into(),
                err: None,
                sessions: vec![sess("local", "editor", 1, false)],
            },
            Group {
                host: "jupiter00".into(),
                err: None,
                sessions: vec![sess("jupiter00", "inference", 1, false)],
            },
            Group {
                host: "db-2".into(),
                err: Some("connection timed out".into()),
                sessions: vec![],
            },
            Group {
                host: "db-3".into(),
                err: Some("connection timed out".into()),
                sessions: vec![],
            },
        ],
    }
}

/// One reachable host carrying `n` sessions, so the nav holds exactly `n` cards
/// numbered `1..=n`. Used where the card COUNT is the point (a two-digit jump needs
/// more cards than [`sample`] has).
fn scan_with_sessions(n: usize) -> Scan {
    let sessions = (0..n)
        .map(|i| sess("local", &format!("s{i}"), 1, false))
        .collect();
    Scan {
        groups: vec![Group {
            host: "local".into(),
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

/// The session names of one host's group, in `state.groups` (display) order.
fn group_session_names(h: &Harness, host: &str) -> Vec<String> {
    h.state
        .groups
        .iter()
        .find(|g| g.host == host)
        .map(|g| g.sessions.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

/// The group hosts in `state.groups` (display) order.
fn group_order(h: &Harness) -> Vec<String> {
    h.state.groups.iter().map(|g| g.host.clone()).collect()
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
            host: "jup".into(),
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
    // step a card: from the first card of the first host, → leaves that host.
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_ne!(sw.selected, next, "→ is not ↓");
    let next_category = sw.selected;
    sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state);
    assert_eq!(
        sw.selected, start,
        "← returns to the first host's first card"
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
        "h == ←: back to the first host's first card"
    );
}

// --- tests --------------------------------------------------------------

#[tokio::test]
async fn renders_a_session_card_per_session() {
    // One card per session: a `{machine}/{mux}` context line over the session name on
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
        "▲", // unreachable machine marker (the reason lives on the machine screen)
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
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    // A remote streams in and must NOT pull the cursor down.
    h.sw.apply_host_result(
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
            Some(RowRef::Session { sess }) if sess.host == "local" && sess.name == "editor"
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
fn initial_host_seed_does_not_arm_a_rescan() {
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);

    assert!(
        !h.sw.take_rescan_kick(),
        "launch discovery is started directly, not by the first user input"
    );
}

// --- streaming model (render-first, per-element) ------------------------

#[tokio::test]
async fn from_hosts_renders_scanning_skeletons() {
    // The first frame: one host-skeleton row per host, each in a scanning
    // state, before ANY probe result lands. Structure first, data later.
    let h = Harness::from_hosts(&["local", "jupiter00"]);
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
    // Every navigation row is one line now, a scanning host included: the machine name,
    // the confirmed mux, and ONE spinner trailing the line - in the same trailing
    // place whether or not the mux is already known, so all scanning cards read alike
    // and none leaves a blank second row.
    let sp = crate::ui::spinner_glyph(0);
    let non_empty = |h: &Harness| {
        h.nav_cards_text()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
    };

    // A bare host id has no confirmed mux yet: the card is machine + trailing spinner.
    let h = Harness::from_hosts(&["local"]);
    let rows = non_empty(&h);
    assert_eq!(rows.len(), 1, "one row, no blank second line:\n{rows:?}");
    assert_eq!(rows[0], format!("1 local {sp} scanning"));

    // A qualified id already confirms its mux: same shape, the mux in the middle.
    let h = Harness::from_hosts(&["local:zellij"]);
    let rows = non_empty(&h);
    assert_eq!(rows.len(), 1, "one row, no blank second line:\n{rows:?}");
    assert_eq!(rows[0], format!("1 local/zellij {sp} scanning"));
}

#[tokio::test]
async fn remove_host_drops_the_card_and_everything_keyed_to_it() {
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    h.sw.apply_host_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "api", 2, false)],
        None,
        &mut h.state,
    );

    h.sw.remove_host("jupiter00", &mut h.state);
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
async fn remove_host_ignores_a_host_the_nav_does_not_show() {
    let mut h = Harness::from_hosts(&["local"]);
    let before = h.state.groups.len();
    h.sw.remove_host("jupiter00", &mut h.state);
    assert_eq!(h.state.groups.len(), before, "idempotent");
}

#[tokio::test]
async fn apply_host_result_turns_scanning_into_sessions() {
    let mut h = Harness::from_hosts(&["local"]);
    assert!(
        spins(&h.nav_cards_text()),
        "the host card spins before the result"
    );
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result(
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
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result(
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
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "w", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "b", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
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
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false),
            sess("local", "db", 1, false),
        ],
        None,
        &mut h.state,
    ); // → db, web
    h.sw.apply_host_result(
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
    // The `R` re-scan clears sessions + re-seeds scanning; the next result re-applies
    // the deterministic name order, identical to the poll's.
    h.sw.request_rescan(&mut h.state);
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "build", 1, false)],
        None,
        &mut h.state,
    );
    // infer is the launch preselect - the top card - so a select_address
    // to it is a no-op; pin it as a deliberate user selection so a rebuild won't drift it.
    h.sw.select_address(&crate::session::Address::new("jupiter00", "infer"));
    h.sw.interest = super::Interest::Selected;
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
        Some(RowRef::Host { host, .. }) => assert_eq!(
            host, "jupiter00",
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
    h.sw.apply_host_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "build", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
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

/// Selects the host card of `host`, as a user move does.
fn select_host_card(h: &mut Harness, host: &str) {
    let i =
        h.sw.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Host { host: s, .. } if s == host))
            .expect("the host card");
    h.sw.note_user_move();
    h.sw.set_selected(i);
}

#[test]
fn a_scanning_host_card_shows_its_scanning_screen_over_another_hosts_display() {
    let mut h = Harness::from_hosts(&["local", "prod"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.state.displayed = crate::model::Selection {
        host: "local".into(),
        session: "web".into(),
    };
    select_host_card(&mut h, "prod");
    assert!(h.state.scanning.contains("prod"));
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(crate::model::ViewScreen::Scanning),
        "local/web must not show under the scanning prod card"
    );
}

#[tokio::test]
async fn a_full_rescan_keeps_the_collapsed_sessions_grid_until_the_selection_moves() {
    let mut h = three_hosts_cursor_on_middle().await;
    h.state.displayed = crate::model::Selection {
        host: "jupiter00".into(),
        session: "infer".into(),
    };
    h.sw.request_rescan(&mut h.state);
    assert!(matches!(
        h.sw.current_ref(),
        Some(RowRef::Host { host, .. }) if host == "jupiter00"
    ));
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        None,
        "the collapsed session keeps its grid"
    );
    // Another host answering does not move the selection.
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    assert_eq!(h.sw.current_view_screen(&h.state), None);
    // Moving away and back ends the exception.
    h.key(KeyCode::Down).await;
    select_host_card(&mut h, "jupiter00");
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(crate::model::ViewScreen::Scanning)
    );
}

#[test]
fn a_scanning_host_screen_states_its_headline_word_and_facts() {
    let mut h = Harness::from_hosts(&["local", "prod"]);
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.state.failure_runs.insert("prod".into(), 2);
    select_host_card(&mut h, "prod");
    h.draw();
    assert_eq!(h.plan.view_screen, Some(crate::model::ViewScreen::Scanning));
    let view = h.view_text();
    let lines: Vec<&str> = view.lines().map(str::trim).collect();
    let headline = lines
        .iter()
        .position(|l| l.starts_with("host prod"))
        .unwrap_or_else(|| {
            panic!(
                "the headline:
{view}"
            )
        });
    assert_eq!(lines[headline + 1], "scanning", "{view}");
    assert!(
        view.contains("failures") && view.contains("2 in a row"),
        "{view}"
    );
    assert!(
        !view.contains("re-scan"),
        "a scan in flight offers no key:
{view}"
    );
}

#[tokio::test]
async fn rescan_interest_dropped_when_user_navigates_away() {
    let mut h = three_hosts_cursor_on_middle().await;
    h.sw.request_rescan(&mut h.state);
    // The user navigates to the last host during the skeleton phase.
    h.key(KeyCode::End).await;
    // Sessions re-stream - the selection must NOT get yanked back to infer.
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "web", 1, false)],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result(
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
async fn apply_host_result_empty_shows_empty_status() {
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result("local".into(), vec![], None, &mut h.state);
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result("local".into(), vec![], None, &mut h.state);
    h.draw();
    let cards = h.nav_cards_text();
    assert!(
        cards.contains("local"),
        "the card still names the host:\n{cards}"
    );
}

#[tokio::test]
async fn apply_host_result_marks_the_card_and_states_the_reason_on_the_screen() {
    let mut h = Harness::from_hosts(&["prod"]);
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("command failed (exit 255): ssh: connect to prod port 22: connection refused".into()),
        &mut h.state,
    );
    h.draw();
    // Nav: the one-cell marker and nothing more. No part of the message reaches the card -
    // the screen is where it is stated, and a card is too narrow to hold it whole.
    let tree = h.nav_text();
    assert!(
        tree.contains('▲'),
        "the machine row is marked with ▲:\n{tree}"
    );
    for absent in ["connection refused", "command failed"] {
        assert!(
            !tree.contains(absent),
            "the card states no reason, found {absent:?}:\n{tree}"
        );
    }
    // The lone unreachable machine is auto-selected → its machine screen states it is
    // unreachable and shows why.
    let out = h.text();
    assert!(
        out.contains("unreachable"),
        "the machine screen states unreachable:\n{out}"
    );
    assert!(
        out.contains("connection refused"),
        "the machine screen shows the failure reason:\n{out}"
    );
}

#[tokio::test]
async fn host_failures_use_distinct_one_cell_glyphs_and_selected_state_words() {
    let scan = Scan {
        groups: vec![
            Group {
                host: "login-box".into(),
                err: Some("alice@login-box: Permission denied (publickey,password).".into()),
                sessions: vec![],
            },
            Group {
                host: "list-box".into(),
                err: Some("invalid tuios session listing: expected value".into()),
                sessions: vec![],
            },
            Group {
                host: "dead-box".into(),
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
    let mut h = Harness::from_hosts(&["scanbox", "deadbox"]);
    h.sw.apply_host_result(
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
                host: "local".into(),
                err: None,
                sessions: vec![Session {
                    host: "local".into(),
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
async fn open_filter_reports_matches_and_bolds_matching_cells() {
    let mut h = Harness::new(Scan {
        groups: vec![
            Group {
                host: "host".into(),
                err: None,
                sessions: vec![Session {
                    host: "host".into(),
                    name: "alpha".into(),
                    ..Default::default()
                }],
            },
            Group {
                host: "alpine".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
        ],
    });
    h.sw.rebuild(&mut h.state);
    h.key(KeyCode::Char('/')).await;
    h.ch('l').await;
    h.ch('p').await;
    let top = h.popup_row(0);
    assert!(top.contains(" 2 of 2 "), "match count:\n{top}");
    assert!(h.popup_row(1).contains(" / lp"), "{}", h.popup_row(1));
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
async fn filter_highlights_the_session_part_of_the_matched_address() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            host: "host".into(),
            err: None,
            sessions: vec![Session {
                host: "host".into(),
                name: "alpha".into(),
                ..Default::default()
            }],
        }],
    });
    h.key(KeyCode::Char('/')).await;
    h.ch('h').await;
    h.ch('l').await;
    assert!(
        h.nav_mod_of("h")
            .is_some_and(|m| m.contains(Modifier::BOLD)),
        "section titles keep their fixed bold weight:\n{}",
        h.nav_cards_text()
    );
    assert!(
        h.nav_mod_of("l")
            .is_some_and(|m| m.contains(Modifier::BOLD)),
        "the session character that completes the address match is bold:\n{}",
        h.nav_cards_text()
    );
}

#[tokio::test]
async fn filter_matches_and_marks_the_three_level_path_of_a_one_mux_machine() {
    // A machine serving one mux carries no mux in its host id, yet every surface writes
    // the session as machine/mux/session. The filter reads that same path: the whole of
    // it, a prefix of it, or the session name alone, and marks the typed session part.
    for (typed, kept, marked) in [
        ("gpu-01/tmux/train-llm", "1 of 2", true),
        ("gpu-01/tm", "2 of 2", false),
        ("train-llm", "1 of 2", true),
    ] {
        let mut h = Harness::new(Scan {
            groups: vec![Group {
                host: "gpu-01".into(),
                err: None,
                sessions: vec![
                    sess("gpu-01", "notebook", 1, false),
                    sess("gpu-01", "train-llm", 1, false),
                ],
            }],
        });
        h.state.chrome.set_host_reach(
            [("gpu-01".to_string(), reach("tmux", "gpu-01", "", "tmux ls"))]
                .into_iter()
                .collect(),
        );
        h.sw.rebuild(&mut h.state);
        h.key(KeyCode::Char('/')).await;
        for ch in typed.chars() {
            h.ch(ch).await;
        }
        let top = h.popup_row(0);
        assert!(top.contains(&format!(" {kept} ")), "{typed}: {top}");
        h.key(KeyCode::Enter).await;
        let buf = h.buf();
        let (x, y) = locate(buf, "train-llm", NAV_WIDTH)
            .unwrap_or_else(|| panic!("{typed} keeps train-llm:\n{}", h.nav_cards_text()));
        let bold: Vec<bool> = (x..x + 9)
            .map(|x| buf[(x, y)].modifier.contains(Modifier::BOLD))
            .collect();
        assert_eq!(bold, vec![marked; 9], "{typed}:\n{}", h.nav_cards_text());
    }
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
async fn empty_filter_counts_every_card() {
    let mut h = Harness::new(Scan {
        groups: vec![
            Group {
                host: "local".into(),
                err: None,
                sessions: vec![Session {
                    host: "local".into(),
                    name: "alpha".into(),
                    ..Default::default()
                }],
            },
            Group {
                host: "hidden".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            Group {
                host: "kept".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
        ],
    });
    h.state.logged_in.insert("kept".into());
    h.sw.rebuild(&mut h.state);
    h.key(KeyCode::Char('/')).await;
    let top = h.popup_row(0);
    assert!(top.contains(" 3 of 3 "), "card count: {top}");
}

#[tokio::test]
async fn list_failure_allows_a_new_session_on_the_answering_host() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            host: "list-box:tmux".into(),
            err: Some("invalid tmux session listing: expected value".into()),
            sessions: vec![],
        }],
    });
    h.ch('n').await;
    assert!(h.state.is_inputting(), "new-session input opens");
    assert!(
        h.state.notify.toasts.is_empty(),
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
        h.popup_row(0).contains(" 0 of 4 "),
        "fallback host cards are not matches: {}",
        h.popup_row(0)
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
            host: "local".into(),
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
    let r = h.plan.popup_rect;
    let inner = (r.width - 2, r.height - 2);
    h.sw.feed_reader_key(b"new session", 0x07, &mut false, inner, &mut h.state);
    h.draw();
    assert!(key_shape(&h, "C-g n").contains(Modifier::BOLD));
}

#[tokio::test]
async fn an_unselected_unreachable_card_keeps_the_warning_mark() {
    // The ▲ mark keeps the error colour. The SELECTED card is painted in
    // the accent pair, which flattens every level colour on it by design, so the colour
    // assertion reads an UNSELECTED unreachable card.
    let scan = selection_parked_elsewhere(Scan {
        groups: vec![Group {
            host: "dead".into(),
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
async fn a_locked_machine_card_reads_locked_with_the_lock_mark() {
    let mut h = Harness::from_hosts(&["prod"]);
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    // The card carries the lock mark on its machine row (the screen state and reason are
    // the panel's own assertions).
    let tree = h.nav_text();
    assert!(
        tree.lines()
            .any(|l| l.contains("prod") && l.contains(crate::ui::chrome::BLOCK_MARK)),
        "the locked machine row carries the lock mark:\n{tree}"
    );
}

#[tokio::test]
async fn login_pane_draws_its_fields_with_the_password_masked() {
    // The login pane is a feature of the terminal view, not a modal: the connection
    // values sit in the panel, driven from `State::login`. It renders them in the clear
    // and the password as bullets, and no plaintext reaches the frame.
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        host: "pwbox".into(),
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
async fn login_pane_shows_one_selected_after_login_choice() {
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    let screen = h.text();
    assert!(screen.contains("(*) do nothing"), "{screen}");
    assert!(
        screen.contains("( ) save connection to ssh config"),
        "{screen}"
    );
    assert!(screen.contains("( ) register my public key"), "{screen}");
    assert!(screen.contains("save connection to ssh config"), "{screen}");
    assert!(screen.contains("register my public key"), "{screen}");
}

#[tokio::test]
async fn login_pane_marks_required_fields_and_hints_the_optional_one() {
    // A field carries its own emptiness: a required one is marked in its label, and the
    // optional one says so in the space its value would occupy.
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
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
async fn login_pane_hides_the_ssh_config_choice_when_the_values_are_already_saved() {
    let mut h = Harness::from_hosts(&["pwbox"]);
    let value = |value: &str| crate::provision::env::LoginValue {
        value: value.into(),
        provenance: "from ssh config",
    };
    h.state.chrome.set_login_defaults(
        std::collections::HashMap::from([(
            "pwbox".into(),
            crate::provision::env::LoginDefaults {
                address: value("192.0.2.7"),
                port: value("2222"),
                username: value("alice"),
                ssh_effective: Some(crate::transport::Login {
                    address: Some("192.0.2.7".into()),
                    port: Some(2222),
                    user: Some("alice".into()),
                }),
            },
        )]),
        Default::default(),
    );
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    let screen = h.text();
    assert!(
        !screen.contains("save connection to ssh config"),
        "{screen}"
    );
    assert!(screen.contains("(*) do nothing"), "{screen}");
    assert!(screen.contains("( ) register my public key"), "{screen}");

    // A changed value makes recording meaningful again.
    h.state.feed_login("pwbox", b"\t9");
    h.draw();
    assert!(
        h.text().contains("( ) save connection to ssh config"),
        "{}",
        h.text()
    );
}

/// The facts the roster resolution reads for `machine` from `config_text`, with `ssh -G`
/// reporting what it reports for any name: the alias as the host name, port 22 unless a
/// block sets another, and the user a block sets, else the local one.
fn ssh_facts_of(config_text: &str, machine: &str) -> crate::provision::env::SshFacts {
    let stanza = crate::provision::config::stanza_login(config_text, machine);
    let effective = crate::transport::Login {
        address: stanza.address.or(Some(machine.into())),
        port: stanza.port.or(Some(22)),
        user: stanza.user.or(Some("local-user".into())),
    };
    crate::provision::env::SshFacts {
        defaults: crate::provision::config::login_defaults(
            machine,
            None,
            Some(&effective),
            config_text,
        ),
        stanza: crate::provision::config::host_stanza(config_text, machine),
    }
}

#[tokio::test]
async fn login_pane_follows_a_logout_that_removed_the_machines_ssh_config_entry() {
    let before = "Host gpu-01 web-01 db-01
    User dev
";
    let mut h = Harness::from_hosts(&["db-01"]);
    let facts = ssh_facts_of(before, "db-01");
    h.state.chrome.set_login_defaults(
        [("db-01".to_string(), facts.defaults)].into(),
        [("db-01".to_string(), facts.stanza)].into(),
    );
    h.sw.apply_host_result(
        "db-01".into(),
        vec![],
        Some("dev@db-01: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.feed_login("db-01", b"");
    h.draw();
    assert!(
        h.view_text().contains("from ssh config"),
        "{}",
        h.view_text()
    );
    assert!(
        !h.text().contains("save connection to ssh config"),
        "{}",
        h.text()
    );

    let (after, _) = crate::provision::config::remove_host_entries(before, "db-01", true);
    h.state
        .set_ssh_facts("db-01", ssh_facts_of(&after, "db-01"));
    h.draw();
    assert!(
        !h.view_text().contains("from ssh config"),
        "no entry names db-01 any more:
{}",
        h.view_text()
    );
    assert!(
        h.text().contains("( ) save connection to ssh config"),
        "{}",
        h.text()
    );
    assert_eq!(h.state.login.as_ref().unwrap().username, "");
}

#[tokio::test]
async fn machine_screen_shows_the_user_and_stanza_a_login_saved() {
    let mut h = Harness::from_hosts(&["db-01"]);
    h.state.chrome.host_reach.insert(
        "db-01".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    let before = "Host gpu-01 web-01
    User dev
";
    let facts = ssh_facts_of(before, "db-01");
    h.state.chrome.set_login_defaults(
        [("db-01".to_string(), facts.defaults)].into(),
        [("db-01".to_string(), facts.stanza)].into(),
    );
    h.sw.apply_host_result(
        "db-01".into(),
        vec![sess("db-01", "api", 1, false)],
        None,
        &mut h.state,
    );
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    assert!(
        h.view_text().contains("(no matching entry)"),
        "{}",
        h.view_text()
    );

    let saved = crate::provision::config::upsert_managed_stanza(
        before,
        "db-01",
        &crate::transport::Login {
            address: Some("10.0.0.5".into()),
            port: Some(22),
            user: Some("dev".into()),
        },
    );
    h.state
        .set_ssh_facts("db-01", ssh_facts_of(&saved, "db-01"));
    h.draw();
    let user = h.view_cell_of("user").unwrap().0;
    assert!(h.view_row(user).contains("dev"), "{}", h.view_text());
    let stanza = h.view_cell_of("ssh config").unwrap().0;
    assert!(h.view_row(stanza).contains("db-01"), "{}", h.view_text());
    assert!(!h.view_text().contains("(no matching entry)"));
}

#[tokio::test]
async fn login_pane_prefills_all_values_from_ssh_config() {
    let mut h = Harness::from_hosts(&["e2e-box"]);
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
                ssh_effective: None,
            },
        )]),
        Default::default(),
    );
    h.sw.apply_host_result(
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
async fn login_and_key_registration_results_are_one_toast_kept_for_the_machine() {
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Ok,
                registration: RegistrationOutcome::Registered,
                output: String::new(),
                saved: None,
            },
        },
        &mut h.state,
    );
    h.draw();
    let toast = &h.state.notify.toasts[0];
    assert_eq!(
        toast.title, "pwbox",
        "the toast names the machine it reports on"
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
async fn a_failed_login_and_a_skipped_key_have_timed_toasts() {
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::state::notify::Level;
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied".into(),
                },
                registration: RegistrationOutcome::NotRequested,
                output: String::new(),
                saved: None,
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
    assert!(toast.until.is_some(), "a failure toast expires");

    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Ok,
                registration: RegistrationOutcome::Skipped("no key to send".into()),
                output: String::new(),
                saved: None,
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
    assert!(h.state.notify.toasts[0].until.is_some());

    // A cancelled login says nothing about the connection the user ended.
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::Cancelled,
                    reason: "cancelled".into(),
                },
                registration: RegistrationOutcome::NotRequested,
                output: String::new(),
                saved: None,
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
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        host: "pwbox".into(),
        address: "100.88.0.0".into(),
        port: "22".into(),
        username: "alice".into(),
        ..Default::default()
    });
    h.draw();
    assert!(
        h.text().contains(" Log in "),
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
        !screen.contains(" Log in "),
        "there is nothing left to submit:\n{screen}"
    );

    // A login running for a DIFFERENT machine is not this pane's: the button stays.
    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("elsewhere"));
    h.draw();
    assert!(
        h.text().contains(" Log in "),
        "another machine's login leaves this pane alone:\n{}",
        h.text()
    );
}

#[tokio::test]
async fn the_verdict_takes_the_login_screen_down() {
    // However the conversation ended, it is over: the PTY goes with it and the pane comes
    // back holding what was typed, so a failure is retried rather than retyped.
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::OpResult;
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.state.login = Some(crate::state::LoginDraft {
        host: "pwbox".into(),
        username: "alice".into(),
        ..Default::default()
    });
    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("pwbox"));
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: crate::ui::ops::LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied (publickey,password).".into(),
                },
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                output: String::new(),
                saved: None,
            },
        },
        &mut h.state,
    );
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    // A successful unlock returns the unlocked machine so the app re-probes ONLY that
    // machine (its reach changed locked→connected), and it does NOT arm a whole-roster
    // re-scan - that would re-probe every machine for one that changed.
    let reprobe = h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login {
                user: Some("alice".into()),
                ..Default::default()
            },
            attempt: 0,
            outcome: crate::ui::ops::LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Ok,
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                output: String::new(),
                saved: None,
            },
        },
        &mut h.state,
    );
    // The values that authenticated come back WITH the machine: the app records them on
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
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    let reprobe = h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: crate::ui::ops::LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied (publickey,password).".into(),
                },
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                output: String::new(),
                saved: None,
            },
        },
        &mut h.state,
    );
    assert_eq!(reprobe, None, "a failure re-probes nothing");
    assert!(
        h.sw.current_machine_blocked(),
        "auth failure keeps the card locked"
    );
}

/// A blocked `pwbox` whose last login ssh refused the password, with ssh's own text
/// carrying a line before the refusal so the folded and unfolded forms differ.
fn refused_login_harness() -> Harness {
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    let raw = "Warning: Permanently added 'pwbox' to the list of known hosts.\nalice@pwbox: Permission denied (publickey,password).";
    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login: crate::transport::Login::default(),
            attempt: 0,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::WrongPassword,
                    reason: format!("the password was refused\n{raw}"),
                },
                output: raw.into(),
                saved: None,
                registration: RegistrationOutcome::NotRequested,
            },
        },
        &mut h.state,
    );
    h.state.notify.dismiss_all();
    h
}

#[test]
fn login_hint_is_visible_on_first_focus_before_any_field_is_edited() {
    let mut h = refused_login_harness();
    assert!(h.state.login.is_none());
    h.draw();
    assert!(
        !h.view_text().contains("Tab next"),
        "the pane states its keys only while it takes them"
    );
    h.state
        .focus
        .set_view_focus(crate::state::ViewFocus::Terminal);
    h.draw_terminal_focused();
    assert_eq!(h.plan.view_screen, Some(crate::model::ViewScreen::Login));
    assert!(
        !h.plan.floating_hint_bar,
        "no bar floats over the window for the pane"
    );
    let out = h.view_text();
    let keys = out
        .lines()
        .position(|l| l.contains("Tab next · Enter next / log in · Space choose · Esc nav"))
        .unwrap_or_else(|| panic!("the keys sit under the form:\n{out}"));
    let details = out
        .lines()
        .position(|l| l.contains("details"))
        .expect("the failure's details choice");
    assert!(keys > details, "under the result block:\n{out}");
    assert!(
        out.contains("[ Log in ]"),
        "the button reads as a button:\n{out}"
    );
}

#[tokio::test]
async fn a_login_failure_reads_verdict_marked_field_dim_ssh_line_then_details() {
    let mut h = refused_login_harness();
    h.draw();
    let out = h.view_text();
    let pal = crate::ui::palette::Palette::default();

    let (verdict_row, verdict_style) = h
        .view_cell_of("✗ the password was refused")
        .unwrap_or_else(|| panic!("the verdict leads with the failure mark:\n{out}"));
    assert_eq!(verdict_style.fg, Some(pal.error));
    let (ssh_row, ssh_style) = h
        .view_cell_of("alice@pwbox: Permission denied (publickey,password).")
        .unwrap_or_else(|| panic!("ssh's own last line follows:\n{out}"));
    assert_eq!(ssh_style.fg, Some(pal.decoration), "ssh's text is dimmed");
    let (details_row, _) = h
        .view_cell_of("[ ] details")
        .unwrap_or_else(|| panic!("the details choice follows:\n{out}"));
    assert!(verdict_row < ssh_row && ssh_row < details_row, "{out}");

    // The field the failure concerns carries the mark; the others do not.
    let (password_row, password_style) = h.view_cell_of("password").unwrap();
    assert_eq!(password_style.fg, Some(pal.error), "{out}");
    assert!(h.view_row(password_row).ends_with('✗'), "{out}");
    let (address_row, address_style) = h.view_cell_of("address*").unwrap();
    assert_eq!(address_style.fg, Some(pal.decoration));
    assert!(!h.view_row(address_row).contains('✗'), "{out}");

    // Folded: ssh's earlier lines and the machine facts wait behind the choice, and the
    // keys stay.
    for folded in ["Warning: Permanently added", "ssh output"] {
        assert!(!out.contains(folded), "{folded:?} is folded:\n{out}");
    }
    assert!(out.contains("rescan all machines"), "{out}");
    assert!(
        !out.contains(" reason "),
        "the verdict replaces the reason row:\n{out}"
    );
}

#[tokio::test]
async fn the_details_choice_unfolds_ssh_text_and_machine_facts() {
    let mut h = refused_login_harness();
    // Back-tab from the first stop wraps to the last one, which is the details choice
    // while the pane states a failure, and Space picks it like any other choice.
    h.state.feed_login("pwbox", b"\x1b[Z");
    assert_eq!(
        h.state.login.as_ref().unwrap().focus,
        crate::state::LoginFocus::Details
    );
    h.state.feed_login("pwbox", b" ");
    assert!(h.state.login.as_ref().unwrap().details);
    h.draw();
    let out = h.view_text();
    assert!(out.contains("[x] details"), "{out}");
    for unfolded in ["ssh output", "Warning: Permanently added", "ssh config"] {
        assert!(out.contains(unfolded), "{unfolded:?} is unfolded:\n{out}");
    }
    h.state.feed_login("pwbox", b" ");
    h.draw();
    assert!(
        !h.view_text().contains("ssh output"),
        "Space folds it again"
    );

    // Without a failure there is nothing to unfold, so the choice is no stop.
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result("pwbox".into(), vec![], None, &mut h.state);
    h.state.feed_login("pwbox", b"\x1b[Z");
    assert_eq!(
        h.state.login.as_ref().unwrap().focus,
        crate::state::LoginFocus::Submit
    );
}

#[tokio::test]
async fn login_steps_show_each_state_as_the_login_reports_it() {
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::ui::ops::{LoginOutcome, OpResult, RegistrationOutcome};
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    let login = crate::transport::Login {
        address: Some("10.0.4.12".into()),
        port: Some(2222),
        user: Some("alice".into()),
    };
    h.state.login_progress.insert(
        "pwbox".into(),
        crate::model::LoginProgress::start(1, &login, true, false, true),
    );
    h.state.login_run = Some(crate::link::unlock::RunningLogin::parked("pwbox"));
    h.draw();
    let spin = crate::ui::spinner_glyph(h.state.chrome.spinner_frame);
    let row = |h: &Harness, text: &str| {
        let (y, _) = h
            .view_cell_of(text)
            .unwrap_or_else(|| panic!("{text:?} is a step:\n{}", h.view_text()));
        h.view_row(y)
    };
    assert_eq!(
        row(&h, "connect 10.0.4.12:2222"),
        format!("{spin} connect 10.0.4.12:2222")
    );
    assert_eq!(
        row(&h, "authenticate as alice"),
        "authenticate as alice with the password",
        "a pending step has a blank mark"
    );
    assert!(
        !h.view_text().contains("✗"),
        "the probe failure the login answers is no failure of its own:\n{}",
        h.view_text()
    );
    assert!(!h.view_text().contains("ssh output"), "{}", h.view_text());

    h.sw.apply_op_result(
        OpResult::LoginProgress {
            host: "pwbox".into(),
            attempt: 1,
            event: crate::model::LoginEvent::PasswordAsked,
        },
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        row(&h, "connect 10.0.4.12:2222"),
        "✓ connect 10.0.4.12:2222"
    );
    assert_eq!(
        row(&h, "authenticate as alice"),
        format!("{spin} authenticate as alice with the password")
    );

    h.sw.apply_op_result(
        OpResult::Login {
            host: "pwbox".into(),
            login,
            attempt: 1,
            outcome: LoginOutcome {
                auth_method: None,
                connect: UnlockOutcome::Failed {
                    kind: FailureKind::WrongPassword,
                    reason: "the password was refused\nalice@pwbox: Permission denied".into(),
                },
                output: "alice@pwbox: Permission denied".into(),
                saved: None,
                registration: RegistrationOutcome::NotRequested,
            },
        },
        &mut h.state,
    );
    h.state.notify.dismiss_all();
    h.draw();
    assert_eq!(
        row(&h, "connect 10.0.4.12:2222"),
        "✓ connect 10.0.4.12:2222"
    );
    assert_eq!(
        row(&h, "authenticate as alice"),
        "✗ authenticate as alice with the password"
    );
    assert_eq!(
        row(&h, "· register my public key"),
        "· register my public key"
    );
    assert_eq!(row(&h, "find mux"), "· find mux");
    let (steps_end, _) = h.view_cell_of("· find mux").unwrap();
    let (verdict, _) = h.view_cell_of("✗ the password was refused").unwrap();
    assert!(steps_end < verdict, "the steps lead to the verdict");
}

#[tokio::test]
async fn a_step_note_over_several_lines_renders_one_line_each() {
    use crate::link::unlock::UnlockOutcome;
    use crate::ui::ops::{LoginOutcome, RegistrationOutcome};
    let mut h = Harness::from_hosts(&["pwbox"]);
    h.sw.apply_host_result(
        "pwbox".into(),
        vec![],
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    let mut progress = crate::model::LoginProgress::start(
        1,
        &crate::transport::Login::default(),
        false,
        false,
        true,
    );
    progress.finish(&LoginOutcome {
        auth_method: None,
        connect: UnlockOutcome::Ok,
        output: String::new(),
        saved: None,
        registration: RegistrationOutcome::Failed(
            "the password was refused\nalice@pwbox: Permission denied".into(),
        ),
    });
    h.state.login_progress.insert("pwbox".into(), progress);
    h.draw();
    let (summary, _) = h
        .view_cell_of("register my public key: the password was refused")
        .unwrap_or_else(|| panic!("the note's first line follows the step:\n{}", h.view_text()));
    let (detail, _) = h
        .view_cell_of("alice@pwbox: Permission denied")
        .unwrap_or_else(|| panic!("the detail is its own line:\n{}", h.view_text()));
    assert_eq!(detail, summary + 1, "{}", h.view_text());
    assert_eq!(h.view_row(detail), "alice@pwbox: Permission denied");
}

#[tokio::test]
async fn login_inputs_are_grouped_and_the_focused_value_is_highlighted() {
    let mut h = refused_login_harness();
    h.state.login = Some(crate::state::LoginDraft {
        host: "pwbox".into(),
        address: "10.0.4.12".into(),
        port: "22".into(),
        username: "alice".into(),
        focus: crate::state::LoginFocus::Username,
        ..Default::default()
    });
    h.draw_terminal_focused();
    let out = h.view_text();
    let at = |text: &str| {
        h.view_cell_of(text)
            .unwrap_or_else(|| panic!("{text:?}:\n{out}"))
            .0
    };
    let connection = at("Connection");
    let after = at("After login");
    assert!(
        connection < at("address*") && at("password") < after,
        "{out}"
    );
    assert!(
        after < at("register my public key") && at("Log in") < at("the password was refused"),
        "{out}"
    );

    let lit =
        |h: &Harness, text: &str| h.view_cell_of(text).unwrap().1.bg == Some(Color::LightGreen);
    assert!(lit(&h, "alice"), "the focused value is highlighted:\n{out}");
    assert!(!lit(&h, "username*"), "the label stays plain");
    assert!(!lit(&h, "address*"), "other names do not");
    assert!(
        !lit(&h, " username*"),
        "the padding before the name stays plain"
    );

    h.state.login.as_mut().unwrap().focus = crate::state::LoginFocus::AfterSshConfig;
    h.draw_terminal_focused();
    assert!(lit(&h, "( ) save connection"));
    assert!(h
        .view_text()
        .contains(" ( ) save connection to ssh config "));

    // A stop without a name is highlighted over its own text.
    h.state.login.as_mut().unwrap().focus = crate::state::LoginFocus::Submit;
    h.draw_terminal_focused();
    assert!(lit(&h, "Log in"));
    assert!(!lit(&h, "username*"));

    // The pane takes keys only while the terminal view is focused, and says so.
    h.draw();
    assert!(!lit(&h, "Log in"));
}

#[tokio::test]
async fn a_card_claims_a_mux_only_when_it_is_confirmed() {
    // A host-state card claims no mux it cannot back with an answer. A bare-id host
    // (its mux is a config assumption, never probed until the enumeration answers)
    // reads the machine alone when unreachable or scanning; a QUALIFIED id names a mux
    // the machine was resolved to serve, which is a confirmed fact even while the host
    // is unreachable; a settled reachable host shows the mux its enumeration answered
    // through.
    let h = Harness::new(Scan {
        groups: vec![
            // bare id: mux only assumed, unreachable - no claim.
            Group {
                host: "dead".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            // qualified id: the mux was resolved on the machine, so it is a fact. The
            // machine's other mux answered, so the machine is up and the failing mux keeps
            // a card of its own.
            Group {
                host: "srv:zellij".into(),
                err: Some("connection refused".into()),
                sessions: vec![],
            },
            Group {
                host: "srv:screen".into(),
                err: None,
                sessions: vec![],
            },
            // settled reachable empty host: the enumeration answered through its mux.
            Group {
                host: "fresh:psmux".into(),
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
async fn unreachable_machine_screen_keeps_a_long_reason_whole() {
    // ssh wraps the failure in its own context and names it LAST, past the width of the
    // screen: a reason cut off at the edge drops the only words that say what went wrong.
    let reason =
        "command failed (exit 255): ssh: connect to host kyla.tail1cbccc.ts.net port 22: Connection timed out";
    let mut h = Harness::from_hosts(&["kyla"]);
    h.sw.apply_host_result("kyla".into(), vec![], Some(reason.into()), &mut h.state);
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_host_result(
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
async fn the_created_toast_names_the_listed_mux_before_the_reach_resolves() {
    // No reach yet, so the host names no mux, but the created session carries one: the
    // toast names the mux the new session's card names.
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_op_result(
        OpResult::Created {
            session: sess_mux("local", "serve", "psmux"),
        },
        &mut h.state,
    );
    let history: Vec<String> = h
        .state
        .notify
        .history
        .iter()
        .map(|e| e.note.text.clone())
        .collect();
    assert_eq!(history, ["local/psmux/serve created"]);
}

#[tokio::test]
async fn the_session_xmux_runs_in_shows_a_screen_instead_of_its_grid() {
    // Refusing silently would leave the last session's grid standing under the wrong
    // card. The screen says whose session it is and why it is not shown.
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_host_result(
        "local".into(),
        vec![sess_mux("local", "xmus", "psmux")],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    // The host names no mux before its reach resolves, but the session's listing does,
    // and the headline names the mux the session's card names.
    assert!(
        out.contains("local/psmux/xmus"),
        "headlined by its path:\n{out}"
    );
    assert!(out.contains("running xmux"), "and by its state:\n{out}");
    assert!(out.contains("refused"), "and says it is refused:\n{out}");
}

#[tokio::test]
async fn the_self_session_screen_headline_carries_the_mux_too() {
    // The screen is reached by an ADDRESS, and an address names three levels: the machine,
    // its mux, the session. The headline states all three, in the cards' own grammar.
    let mut h = Harness::from_hosts(&["local"]);
    h.state.chrome.set_host_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.set_own_session(Some(crate::session::Address::new("local", "xmus")));
    h.sw.apply_host_result(
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
async fn unreachable_machine_screen_names_the_provider_that_offered_the_machine() {
    // A machine that fails is only half an answer while the user cannot tell why it is on
    // the list at all: a tailnet peer nobody wrote down reads as a mystery. The screen
    // names the provider that put it there, which is also the one they would turn off.
    let mut h = Harness::from_hosts(&["kyla"]);
    h.state.chrome.set_roster_providers(
        [("kyla".to_string(), "tailscale".to_string())]
            .into_iter()
            .collect(),
    );
    h.sw.apply_host_result(
        "kyla".into(),
        vec![],
        Some("connection timed out".into()),
        &mut h.state,
    );
    h.key(KeyCode::Char('d')).await;
    h.draw();
    let out = h.view_text();
    assert!(out.contains("provider"), "the row is named:\n{out}");
    assert!(out.contains("tailscale"), "and carries the answer:\n{out}");
}

#[tokio::test]
async fn a_machine_nothing_recorded_gets_no_provider_row() {
    // An empty map is not "offered by nothing": it is nothing recorded. The row is
    // absent rather than blank, so the screen never states an answer it does not have.
    let mut h = Harness::from_hosts(&["kyla"]);
    h.sw.apply_host_result(
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
async fn unreachable_machine_screen_shows_ssh_config_stanza() {
    let mut h = Harness::from_hosts(&["jupiter00"]);
    h.state.chrome.set_login_defaults(
        Default::default(),
        std::collections::HashMap::from([(
            "jupiter00".into(),
            "Host jupiter00\n    HostName 143.248.140.120\n    User hrlee\n".into(),
        )]),
    );
    h.sw.apply_host_result(
        "jupiter00".into(),
        vec![],
        Some("no route".into()),
        &mut h.state,
    );
    h.key(KeyCode::Char('d')).await;
    h.draw();
    let out = h.text();
    assert!(
        out.contains("HostName 143.248.140.120"),
        "shows the machine's ssh config:\n{out}"
    );
    assert!(out.contains("hrlee"), "shows the configured user:\n{out}");
    assert!(
        !out.contains("1.2.3.4"),
        "does NOT leak an unrelated machine's config:\n{out}"
    );
}

#[tokio::test]
async fn streaming_keeps_local_preselect_when_untouched() {
    // An untouched selection sits on the top SESSION card (the local host's first
    // session, row 1 - row 0 is its section title), and a later REMOTE
    // session streaming in must NOT steal it: the selection must not leap to a remote
    // on first launch (#1).
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    h.sw.apply_host_result(
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
    h.sw.apply_host_result(
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
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "local"),
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
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    // The remote answers first.
    h.sw.apply_host_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.host == "jupiter00"
        ),
        "the first session to answer takes the cursor"
    );
    // The local host answers second, and the order puts it ABOVE the remote - the case a
    // top-card preselect would move the cursor for.
    h.sw.apply_host_result(
        "local".into(),
        vec![sess("local", "editor", 1, false)],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.host == "jupiter00" && sess.name == "infer"
        ),
        "a host answering later does not take the cursor off the session already on screen"
    );
}

#[tokio::test]
async fn request_rescan_arms_a_display_reattach() {
    // The `R` re-scan also arms an explicit re-attach of the current display, so a
    // detached / dead display client is re-created on demand (the loop consumes it).
    let mut state = crate::state::State::from_hosts(vec!["h".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    assert!(
        !sw.take_reattach_kick(),
        "no re-attach armed before a re-scan"
    );
    sw.request_rescan(&mut state);
    assert!(
        sw.take_reattach_kick(),
        "an R re-scan arms a display re-attach"
    );
    assert!(!sw.take_reattach_kick(), "the kick is consumed once");
}

#[tokio::test]
async fn rebuild_holds_a_user_moved_session_against_the_preselect() {
    // The selection thrash: once the user has moved the selection onto a session, a bare
    // rebuild (a frequent poll / %-event)
    // must keep it there, not snap it back to the preferred preselect.
    let mut state = crate::state::State::from_hosts(vec!["h".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    sw.apply_host_result(
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
    sw.set_selected(idx);
    sw.interest = super::Interest::Selected;
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
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    h.sw.apply_host_result(
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
    h.sw.apply_host_result(
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
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    let hint_bar = h.hint_bar_text();
    assert!(
        hint_bar.contains("scanning"),
        "hint_bar shows a global scanning indicator:\n{hint_bar:?}"
    );
    assert!(
        hint_bar.contains("/2"),
        "hint_bar shows the host progress fraction:\n{hint_bar:?}"
    );
    h.sw.apply_host_result("local".into(), vec![], None, &mut h.state);
    h.sw.apply_host_result("jupiter00".into(), vec![], None, &mut h.state);
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
async fn the_armed_prefix_indicator_fits_a_narrow_nav() {
    // A live prefix names its keys in the key list beside the nav, so the indicator in
    // the nav column keeps the prefix alone and never clips.
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
        "the armed indicator fits the nav column:\n{hint_bar:?}"
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
async fn the_selected_card_is_painted_in_the_themes_accent() {
    // With no `[ui] selection-style` set, the selected card is the theme's on-accent text
    // on its accent, over every cell: the dim number and the level-coloured name alike,
    // so no colour of the card survives inside the highlight.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await; // step onto local/editor's card
    let sel = h.nav_row_of("editor").expect("editor row");
    let other = h.nav_row_of("inference").expect("inference row");
    for x in [CARD_INDENT, 4] {
        let cell = h.buf()[(x, sel)].clone();
        assert!(
            on_accent(&cell),
            "the selected row is on the accent: {cell:?}"
        );
        assert_eq!(cell.fg, Color::Black, "in the on-accent text: {cell:?}");
        assert!(
            !cell.modifier.intersects(Modifier::REVERSED | Modifier::DIM),
            "neither swapped nor dimmed: {cell:?}"
        );
    }
    assert!(
        !on_accent(&h.buf()[(4, other)]),
        "and only that row is: {other}"
    );
    assert_eq!(
        h.buf()[(CARD_INDENT, sel)].symbol(),
        "2",
        "the selected card keeps its number; the highlight alone marks it"
    );
}

#[tokio::test]
async fn selected_card_stays_highlighted_with_terminal_focus() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await;
    h.sw.sync_view_focus(true);
    h.draw_terminal_focused();
    let selected = h.nav_row_of("editor").expect("editor row");
    let other = h.nav_row_of("inference").expect("inference row");
    assert!(on_accent(&h.buf()[(4, selected)]));
    assert!(!on_accent(&h.buf()[(4, other)]));
}

#[tokio::test]
async fn selected_host_card_stays_highlighted_with_terminal_focus() {
    let mut h = Harness::new_sized(scan_with_a_host_band(), 60, 70);
    h.key(KeyCode::Right).await;
    h.key(KeyCode::Right).await;
    assert!(matches!(
        h.sw.current_ref(),
        Some(RowRef::Host { .. } | RowRef::Machine { .. })
    ));
    h.sw.sync_view_focus(true);
    h.draw_terminal_focused();
    let (_, rect) = h
        .plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == h.sw.selected)
        .expect("selected host card");
    assert!(
        (rect.x..rect.right()).all(|x| on_accent(&h.buf()[(x, rect.y)])),
        "rect={rect:?} row={:?} backgrounds={:?}",
        nav_line(&h, rect.y),
        (rect.x..rect.right())
            .map(|x| h.buf()[(x, rect.y)].bg)
            .collect::<Vec<_>>()
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
        "the applied filter shows on the hint bar:\n{out}"
    );
}

#[tokio::test]
async fn create_adds_and_selects() {
    // A reachable empty host shows a host card; n on it creates a session, then selects it.
    let scan = Scan {
        groups: vec![Group {
            host: "local".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let mut h = Harness::new(scan);
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { host, .. }) if host == "local"),
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
            host: "local".into(),
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
    // `n` starts a new SESSION on the selected card's machine/mux. A session card
    // names its host, so `n` there opens the create input seeded with it rather
    // than refusing - you can add a session to a host that already has sessions.
    let mut h = Harness::new(sample());
    assert!(h
        .sw
        .select_address(&crate::session::Address::new("local", "editor")));
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
                i.host.as_deref(),
                Some("local"),
                "seeded with the selected card's host"
            );
        }
        _ => panic!("expected a New input modal"),
    }
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "nothing is created yet"
    );
}

/// Asserts that the newest toast is the refusal `reason` under `title`, a warning that
/// leaves by itself, and that the hint bar still says `bar`, what it said before the key.
fn assert_refused(h: &Harness, bar: &str, title: &str, reason: &str) {
    use crate::state::notify::{Level, Note};
    let toast = h.state.notify.toasts.last().expect("a refusal is a toast");
    assert_eq!(toast.title, title);
    assert_eq!(toast.notes, vec![Note::new(Level::Warning, reason)]);
    assert!(toast.until.is_some(), "a refusal leaves by itself");
    assert_eq!(h.hint_bar_text(), bar, "the hint bar keeps its advice");
}

/// What an action did or why it did nothing is a notification, never hint-bar text:
/// every refused key reports a toast and leaves the hint bar on its contextual text.
#[tokio::test]
async fn a_refused_key_is_a_notification_and_the_hint_bar_keeps_its_advice() {
    // The local machine is not reached over SSH, so it has no login to log out of.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await;
    let bar = h.hint_bar_text();
    h.ch('L').await;
    assert!(h.state.modal.is_none(), "no logout confirm opens");
    assert_refused(&h, &bar, "logout local", "this machine does not use SSH");

    // A session lives in a host, and the unreachable machine has none to create it in.
    h.key(KeyCode::End).await;
    let bar = h.hint_bar_text();
    h.ch('n').await;
    assert!(!h.state.is_inputting(), "no new-session input opens");
    assert_refused(
        &h,
        &bar,
        "new session",
        "machine unreachable, cannot create here",
    );

    // A machine names no host to create a session in.
    h.key(KeyCode::Home).await;
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    let bar = h.hint_bar_text();
    h.ch('n').await;
    assert!(!h.state.is_inputting(), "no new-session input opens");
    assert_refused(
        &h,
        &bar,
        "new session",
        "select a host of local to start a session",
    );

    // A machine is asked one thing at a time.
    h.key(KeyCode::Home).await;
    h.state.scanning.insert("local".into());
    h.draw();
    let bar = h.hint_bar_text();
    h.ch('r').await;
    assert_refused(
        &h,
        &bar,
        "rescan machine local",
        "local is still being scanned",
    );
    h.state.scanning.clear();
}

/// The open popup's row holding `text`, its top border being row 0, and the colour of
/// the cell `text` starts at.
fn popup_line_of(h: &Harness, text: &str) -> Option<(String, Option<Color>)> {
    let r = h.plan.popup_rect;
    (0..r.height).find_map(|i| {
        let row = h.popup_row(i);
        let at = row.find(text)?;
        let col = r.x + row[..at].chars().count() as u16;
        Some((row, Some(h.buf()[(col, r.y + i)].fg)))
    })
}

/// Asserts that the open popup states `error` in the error colour, that the popup is
/// still open on `mode`, and that nothing reached the notifications.
fn assert_popup_error(h: &Harness, mode: crate::state::InputMode, error: &str) {
    assert!(
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.mode == mode),
        "the popup stays open"
    );
    let (row, fg) = popup_line_of(h, &format!("✗ {error}"))
        .unwrap_or_else(|| panic!("the popup states {error:?}"));
    assert_eq!(
        fg,
        Some(h.sw.palette().error),
        "in the error colour: {row:?}"
    );
    assert!(h.state.notify.toasts.is_empty(), "no toast");
    assert!(h.state.notify.history.is_empty(), "nothing in the history");
}

/// Feedback on what the user typed into a popup stays inside that popup, beside its
/// field, with the popup open for the correction: notifications carry only the results
/// of actions.
#[tokio::test]
async fn a_wrong_confirm_word_is_stated_inside_the_confirm() {
    use crate::state::InputMode;
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result(
        "box".into(),
        vec![sess("box", "api", 1, true)],
        None,
        &mut h.state,
    );
    h.ch('L').await;
    for c in "nope".chars() {
        h.ch(c).await;
    }
    h.key(KeyCode::Enter).await;
    assert_popup_error(&h, InputMode::Logout, "type logout to confirm");
    assert_eq!(h.input_buffer(), "", "the field is empty for a new word");

    // The next key takes the error down, and the right word confirms.
    for c in "logout".chars() {
        h.ch(c).await;
    }
    assert!(popup_line_of(&h, "✗").is_none(), "a key clears the error");
    let commands = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(h.state.modal.is_none(), "the confirm closes");
    assert!(
        matches!(commands.as_slice(), [Command::Logout(machine)] if machine == "box"),
        "the logout runs: {commands:?}"
    );

    // The second confirm, for the key lines xmux did not add, answers the same way.
    h.state.modal = Some(Modal::Input(Box::new(crate::state::Input::new(
        InputMode::LogoutKeys,
        String::new(),
        Some("box".into()),
    ))));
    h.draw();
    h.ch('x').await;
    h.key(KeyCode::Enter).await;
    assert_popup_error(&h, InputMode::LogoutKeys, "type remove to confirm");

    // The jump popup states a number no card carries the same way.
    h.key(KeyCode::Esc).await;
    h.ch('9').await;
    h.key(KeyCode::Enter).await;
    assert_popup_error(&h, InputMode::Jump, "no card 9");
}

#[test]
fn logout_confirms_the_machine_of_the_selected_session() {
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result(
        "box".into(),
        vec![sess("box", "api", 1, true)],
        None,
        &mut h.state,
    );
    h.state
        .display_auth_methods
        .insert("box".into(), crate::model::AuthMethod::Password);
    h.draw();
    assert!(h
        .sw
        .handle_key(
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
            &mut h.state
        )
        .is_empty());
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    // Opened from a session card, the first fact is still the machine: the logout acts
    // on the machine, and the session is only where the user stood.
    assert_eq!(
        input.facts,
        vec![
            ("machine", "box".to_string()),
            ("SSH login", "username and password".to_string()),
            ("password", "held password is cleared".to_string()),
            (
                "key",
                "removed from box; asks first if xmux did not add it".to_string()
            ),
            (
                "ssh config",
                "removes the entry xmux saved; asks first for others naming it".to_string()
            ),
            ("connections", "closes box connections".to_string()),
        ]
    );
    assert!(h
        .sw
        .handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut h.state
        )
        .is_empty());
    h.sw.set_input_text("logout", &mut h.state);
    let commands = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(matches!(commands.as_slice(), [Command::Logout(machine)] if machine == "box"));
}

#[test]
fn logout_from_a_session_riding_the_shared_connection_states_its_login() {
    // The display attachment rode the machine's shared SSH connection, so it reported no
    // method of its own; the login its session uses is the one that connection reported.
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result(
        "box".into(),
        vec![sess("box", "api", 1, true)],
        None,
        &mut h.state,
    );
    h.state
        .auth_methods
        .insert("box".into(), crate::model::AuthMethod::PublicKey);
    h.draw();
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
        &mut h.state,
    );
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert_eq!(input.facts[1], ("SSH login", "public key".to_string()));
    assert!(
        !input.facts.iter().any(|(name, _)| *name == "password"),
        "a key login holds no password to clear: {:?}",
        input.facts
    );
}

#[test]
fn logout_states_the_login_an_earlier_run_recorded_for_the_shared_connection() {
    // Every connection of this run rode a shared connection an earlier run opened, so
    // none reported a method; that run's record is the login the session uses.
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result(
        "box".into(),
        vec![sess("box", "api", 1, true)],
        None,
        &mut h.state,
    );
    h.state.recorded_logins.insert(
        "box".into(),
        crate::model::RecordedLogin {
            method: crate::model::AuthMethod::PublicKey,
            connection: Some("7:100.000000001".into()),
        },
    );
    h.state.shared_connections.insert(
        "box".into(),
        ("7:100.000000001".into(), std::time::Instant::now()),
    );
    h.draw();
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
        &mut h.state,
    );
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert_eq!(input.facts[1], ("SSH login", "public key".to_string()));
}

#[test]
fn logout_from_a_card_with_no_session_names_the_machine() {
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result("box".into(), Vec::new(), None, &mut h.state);
    h.draw();
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { .. })));
    assert!(h
        .sw
        .handle_key(
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
            &mut h.state
        )
        .is_empty());
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert_eq!(
        input.facts.first(),
        Some(&("machine", "box".to_string())),
        "the machine is not a session"
    );
}

/// The second confirmation states which lines xmux did not add, what removing them costs
/// outside xmux, and what keeping them leaves, and only the typed word confirms it.
#[test]
fn the_logout_key_confirmation_states_the_risk_and_needs_remove_typed() {
    let mut h = Harness::from_hosts(&["box"]);
    h.sw.open_logout_keys("box", &["authorized_keys"], 1, &[], &mut h.state);
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout key confirmation")
    };
    assert!(input.mode == crate::state::InputMode::LogoutKeys);
    assert_eq!(
        input.facts,
        vec![
            (
                "key",
                "1 line of this PC's key not added by xmux".to_string()
            ),
            ("file", "authorized_keys".to_string()),
            ("remove", "ssh outside xmux loses this key too".to_string()),
            ("keep", "only the 1 line xmux added go".to_string()),
            ("logout", "goes on either way".to_string()),
        ]
    );
    h.sw.set_input_text("logout", &mut h.state);
    assert!(h
        .sw
        .handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut h.state
        )
        .is_empty());
    assert!(h.state.modal.is_some(), "another word does not confirm");
    h.sw.set_input_text("remove", &mut h.state);
    let commands = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(matches!(commands.as_slice(), [Command::RemoveUnmarked(machine)] if machine == "box"));
    assert!(h.state.modal.is_none());
}

#[test]
fn the_logout_key_confirmation_says_the_key_stays_when_xmux_added_none() {
    let mut h = Harness::from_hosts(&["box"]);
    h.sw.open_logout_keys(
        "box",
        &["authorized_keys", "administrators_authorized_keys"],
        0,
        &[],
        &mut h.state,
    );
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout key confirmation")
    };
    assert_eq!(
        input.facts[0].1,
        "2 lines of this PC's key not added by xmux"
    );
    assert_eq!(
        input.facts[1].1,
        "authorized_keys, administrators_authorized_keys"
    );
    assert_eq!(input.facts[3], ("keep", "the key stays on box".to_string()));
}

/// The ssh config entries xmux did not write are listed with the line each one leaves,
/// in the same confirmation as the key lines when both need an answer.
#[test]
fn the_logout_confirmation_lists_the_ssh_config_lines_that_change() {
    let entries = [
        crate::provision::config::RemovedEntry {
            header: "Host gpu-01 web-01 box".into(),
            after: Some("Host gpu-01 web-01".into()),
        },
        crate::provision::config::RemovedEntry {
            header: "Host box".into(),
            after: None,
        },
    ];
    let mut h = Harness::from_hosts(&["box"]);
    h.sw.open_logout_keys("box", &[], 1, &entries, &mut h.state);
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert!(input.mode == crate::state::InputMode::LogoutKeys);
    assert_eq!(
        input.facts,
        vec![
            (
                "ssh config",
                "Host gpu-01 web-01 box becomes Host gpu-01 web-01".to_string()
            ),
            ("ssh config", "Host box goes with its options".to_string()),
            (
                "remove",
                "ssh outside xmux loses these entries too".to_string()
            ),
            ("keep", "the entries stay in ssh config".to_string()),
            ("logout", "goes on either way".to_string()),
        ]
    );
    h.sw.open_logout_keys("box", &["authorized_keys"], 0, &entries[1..], &mut h.state);
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert_eq!(
        input.facts,
        vec![
            (
                "key",
                "1 line of this PC's key not added by xmux".to_string()
            ),
            ("file", "authorized_keys".to_string()),
            ("ssh config", "Host box goes with its options".to_string()),
            (
                "remove",
                "ssh outside xmux loses this key and these entries too".to_string()
            ),
            (
                "keep",
                "the key stays on box; the entries stay in ssh config".to_string()
            ),
            ("logout", "goes on either way".to_string()),
        ]
    );
    h.sw.set_input_text("remove", &mut h.state);
    let commands = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(matches!(commands.as_slice(), [Command::RemoveUnmarked(machine)] if machine == "box"));
}

#[tokio::test]
async fn filter_leaves_cursor_on_visible_session() {
    // The filter hides the selected session, so the selection moves along that
    // session's lineage to its section title, which stays visible because another of its
    // sessions matches. The title shows its information screen, never another session's
    // grid; the next step down reaches the visible session. The filter applies live
    // while the input is open (set_input_text applies it as a real edit would), so Enter
    // only closes it.
    let mut h = Harness::from_hosts(&["local"]);
    h.sw.apply_host_result(
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
    assert!(matches!(
        h.sw.selected_card(),
        Some(RowRef::Section { host }) if host == "local"
    ));
    assert!(h.sw.current_attach_target(&h.state).is_none());
    h.key(KeyCode::Down).await;
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
    let mut h = Harness::from_hosts(&["alpha"]);
    h.sw.apply_host_result(
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
async fn filter_keeps_the_selection_while_its_card_survives_and_names_nothing_once_hidden() {
    // As the live filter shrinks the list, the selection holds its session while that
    // survives. Once nothing of its machine is listed it names nothing rather than
    // landing on another card, and the edit that lists the session again returns the
    // selection to it. The selection starts on build (the first card in name-sorted
    // order).
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
    assert_eq!(
        h.sw.selected_node(),
        None,
        "a hidden machine leaves nothing selected"
    );
    assert_eq!(h.sw.hard_row(), None);
    h.key(KeyCode::Backspace).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.name == "build"
        ),
        "the edit that lists it again returns the selection"
    );
}

#[tokio::test]
async fn create_on_unreachable_machine_refused() {
    let mut h = Harness::new(sample());
    // jump to the last card - the unreachable db-2, one card for the machine.
    h.key(KeyCode::End).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { .. })),
        "expected to reach the unreachable db-2 machine"
    );
    h.ch('n').await;
    assert_eq!(
        h.state.notify.last_report(),
        Some((
            "new session",
            crate::state::notify::Level::Warning,
            "machine unreachable, cannot create here"
        ))
    );
    assert!(h.ops.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_reachable_host_shows_its_host_screen() {
    // A reachable host with no sessions renders its host screen (the name, the state, the
    // keys that apply) in the terminal view, not a blank grid.
    let scan = Scan {
        groups: vec![Group {
            host: "fresh".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let h = Harness::new(scan);
    // The lone selectable row is that empty host, so it is auto-selected.
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { host, .. }) if host == "fresh"),
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
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "local"),
        "the top card is a session of a reachable host with sessions"
    );
    assert!(
        !h.view_text().contains("start a new session"),
        "a host with sessions must not show a host screen"
    );
}

#[tokio::test]
async fn a_section_opens_host_freshness_by_key_and_click_without_numbering_it() {
    let mut h = Harness::new(sample());
    h.state.chrome.host_reach.insert(
        "local".into(),
        reach("tmux", "local", "", "tmux list-sessions"),
    );
    h.state.live_hosts.insert("local".into());
    h.key(KeyCode::Char('i')).await;
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Section { host }) if host == "local"));
    let screen = h.view_text();
    assert!(screen.contains("sessions"), "{screen}");
    assert!(screen.contains("live updates"), "{screen}");
    assert!(h.sw.current_attach_target(&h.state).is_none());
    let section = h.sw.selected;
    assert_eq!(h.sw.card_number(section), 0);

    h.key(KeyCode::Down).await;
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Session { .. })));
    let rect = h
        .plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == section)
        .unwrap()
        .1;
    h.sw.mouse_select(&h.plan.clone(), rect.x, rect.y);
    h.draw();
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Section { host }) if host == "local"));
    h.sw.rebuild(&mut h.state);
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Section { host }) if host == "local"));
    h.state.live_hosts.remove("local");
    h.draw();
    assert!(h.view_text().contains("last observed (channel closed)"));
}

#[tokio::test]
async fn the_machine_screen_shows_the_observed_login_method() {
    let mut h = Harness::from_hosts(&["box"]);
    h.state.chrome.host_reach.insert(
        "box".into(),
        crate::state::HostReach {
            ssh: true,
            ..Default::default()
        },
    );
    h.sw.apply_host_result(
        "box".into(),
        vec![sess("box", "api", 1, false)],
        None,
        &mut h.state,
    );
    h.state
        .auth_methods
        .insert("box".into(), crate::model::AuthMethod::Password);
    h.state
        .display_auth_methods
        .insert("box".into(), crate::model::AuthMethod::PublicKey);
    // The login is the machine's: a session's host does not state it.
    h.ctrl(KeyCode::Up);
    assert!(h.view_cell_of("SSH login").is_none(), "{}", h.view_text());
    h.ctrl(KeyCode::Up);
    let row = h.view_cell_of("SSH login").unwrap().0;
    assert!(h.view_row(row).contains("username and password"));
    h.state.auth_methods.remove("box");
    h.draw();
    assert!(
        h.view_row(row).contains("public key"),
        "the display's login stands in for a probe that observed none"
    );
    h.state.display_auth_methods.remove("box");
    h.draw();
    assert!(h.view_row(row).contains("not observed"));
}

#[tokio::test]
async fn both_host_screens_share_one_grammar() {
    // The unreachable screen and the empty screen are ONE screen in two states, so what
    // is pinned here is the SHAPE both hold to, not either one's words: the name as the
    // headline, the state word under it, one rule column for every row that carries a
    // cell, and the rescan key both offer. A state added later has this to answer to.
    let mut dead = Harness::from_hosts(&["prod"]);
    dead.state.chrome.set_login_defaults(
        Default::default(),
        std::collections::HashMap::from([(
            "prod".into(),
            "Host prod\n    HostName 10.0.0.1\n".into(),
        )]),
    );
    dead.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut dead.state,
    );
    dead.draw();
    let empty = Harness::new(Scan {
        groups: vec![Group {
            host: "fresh".into(),
            err: None,
            sessions: vec![],
        }],
    });
    for (label, view, name, word) in [
        (
            "unreachable",
            dead.view_text(),
            "machine prod",
            "unreachable",
        ),
        ("empty", empty.view_text(), "host fresh", "no sessions"),
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
        assert!(
            !view.contains('│'),
            "{label}: the rows use whitespace: {view}"
        );
        assert!(
            view.contains("rescan all machines"),
            "{label}: both screens offer the rescan key:\n{view}"
        );
    }
    let empty_lines = empty.view_text();
    let lines: Vec<_> = empty_lines.lines().collect();
    let action = lines
        .iter()
        .position(|line| line.contains("start a new session"));
    let fact = lines
        .iter()
        .position(|line| line.trim_start().starts_with("sessions"));
    assert!(
        action < fact,
        "empty-host actions precede facts: {empty_lines}"
    );
}

#[test]
fn an_empty_host_animates_only_below_its_screen_content_when_it_fits() {
    let scan = Scan {
        groups: vec![Group {
            host: "fresh".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let mut tall = Harness::new_sized(scan.clone(), 180, 60);
    assert_eq!(tall.plan.view_screen, Some(crate::model::ViewScreen::Empty));
    let view = tall.plan.regions.terminal;
    let last_content = (view.y..view.bottom())
        .find(|&y| {
            (view.x..view.right())
                .map(|x| tall.buf()[(x, y)].symbol())
                .collect::<String>()
                .contains("rescan all machines")
        })
        .expect("the screen actions");
    let braille_rows = |h: &Harness| -> Vec<u16> {
        let view = h.plan.regions.terminal;
        (view.y..view.bottom())
            .filter(|&y| {
                (view.x..view.right()).any(|x| {
                    h.buf()[(x, y)]
                        .symbol()
                        .chars()
                        .next()
                        .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
                })
            })
            .collect()
    };
    let rows = braille_rows(&tall);
    assert_eq!(rows.len(), 16);
    assert!(rows[0] > last_content);
    let before = tall.view_text();
    tall.state.chrome.animation_ms = 1_033;
    tall.draw();
    assert_ne!(tall.view_text(), before, "the frame advances");

    tall.state.chrome.braille_animation = false;
    tall.draw();
    assert!(braille_rows(&tall).is_empty());
    assert!(tall.view_text().contains("rescan all machines"));

    let short = Harness::new_sized(scan, 100, 20);
    assert!(
        braille_rows(&short).is_empty(),
        "the remaining rows do not fit"
    );
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
    assert!(h
        .sw
        .select_address(&crate::session::Address::new("jupiter00", "inference")));
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
async fn card_text_has_a_fixed_attribute_hierarchy() {
    let h = Harness::new(sample());
    assert!(
        !h.nav_mod_of("editor").unwrap().contains(Modifier::BOLD),
        "the session reads at normal weight"
    );
    assert!(
        h.nav_mod_of("local").unwrap().contains(Modifier::BOLD),
        "the section title reads bold"
    );
    assert!(h.nav_mod_of("2").unwrap().contains(Modifier::DIM));
}

/// A session stamped with its mux kind, for the context-line tests.
fn sess_mux(host: &str, name: &str, mux: &str) -> Session {
    Session {
        host: host.into(),
        name: name.into(),
        mux: mux.into(),
        id: String::new(),
        windows: 1,
        attached: false,
        stopped: false,
    }
}

/// One host carrying `sessions`.
fn one_host_scan(host: &str, sessions: Vec<Session>) -> Scan {
    Scan {
        groups: vec![Group {
            host: host.into(),
            err: None,
            sessions,
        }],
    }
}

/// Several hosts, each carrying `sessions`.
fn hosts_scan(hosts: Vec<(&str, Vec<Session>)>) -> Scan {
    let groups = hosts
        .into_iter()
        .map(|(host, sessions)| Group {
            host: host.into(),
            err: None,
            sessions,
        })
        .collect();
    Scan { groups }
}

#[tokio::test]
async fn a_hosts_cards_are_contiguous_and_the_order_is_deterministic() {
    // A host's cards sit together under their host's one section title: alpha's
    // before beta's, never interleaved with another host's, and inside each host the
    // name order holds (a-new, then a-old).
    let mut h = Harness::new(hosts_scan(vec![
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
    // The app resolves every host's reach before its first frame; set it here so the
    // section titles name their mux exactly as the live app's do.
    h.state.chrome.set_host_reach(
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
    // One section title per host: the title names the whole group, the cards below
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
async fn a_session_found_later_lands_inside_its_own_host() {
    // The order is frozen once the hosts settle, so a session that appears afterwards
    // cannot be placed by re-sorting. It is inserted after the last card of its own
    // host - never appended to the bottom, which would strand it under another host's
    // context line and split the host it belongs to.
    let mut h = Harness::new(hosts_scan(vec![
        ("alpha", vec![sess_mux("alpha", "a-one", "tmux")]),
        ("beta", vec![sess_mux("beta", "b-one", "tmux")]),
    ]));
    h.sw.apply_host_result(
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
async fn the_section_title_shows_machine_mux_and_the_session_takes_the_accent() {
    // The `{machine}/{mux}` label lives on the SECTION TITLE, both halves in the quiet
    // header role; the session card under it is the name alone, the accent target.
    // The mux comes from the resolved reach, exactly as the app resolves every host
    // before its first frame.
    let mut h = Harness::new(selection_parked_elsewhere(one_host_scan(
        "srv",
        vec![sess_mux("srv", "alpha", "tmux")],
    )));
    h.state.chrome.set_host_reach(
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
        "the section title is dim, machine half"
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
            painted.starts_with(' ') && !painted.starts_with("  "),
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
async fn the_selections_highlight_pads_the_card_into_its_indent() {
    // The indent stays blank text, so the highlight's left padding takes it: the card
    // rect starts past the indent, and the indent cell beside it carries the accent
    // without moving the card's number.
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
        on_accent(&buf[(rect.x, rect.y)]),
        "the card itself is painted on the accent"
    );
    let strip = &buf[(rect.x - CARD_INDENT, rect.y)];
    assert_eq!(strip.symbol(), " ", "the indent is blank");
    assert!(on_accent(strip), "and it is the highlight's left padding");
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
    // under the `{machine}/{mux}` above them: the title is the only row saying where the
    // cards are, and a host cut in half names a machine that does not exist. It holds
    // its one row while doing it - the fix is the column's width, never a second row.
    let mut h = Harness::new_sized(
        hosts_scan(vec![
            (
                "build-runner-eu-west",
                vec![sess_mux("build-runner-eu-west", "z", "zellij")],
            ),
            ("jupiter00", vec![sess_mux("jupiter00", "a", "tmux")]),
        ]),
        60,
        12,
    );
    h.state.chrome.set_host_reach(
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
    // level it displays - stays with the machine half; both read in the secondary role.
    // The separator keeps its own furniture role.
    let scan = Scan {
        groups: vec![
            Group {
                host: "srv:zellij".into(),
                err: None,
                sessions: vec![sess_mux("srv:zellij", "alpha", "zellij")],
            },
            // A reachable machine with no session left: the host-state card.
            Group {
                host: "srv:psmux".into(),
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
        "the mux shares the machine half's secondary role"
    );
    // The separator is furniture on both card kinds, and the machine half is secondary.
    let (x, y) = locate(h.buf(), "srv/psmux", NAV_WIDTH).expect("the host card");
    assert_eq!(
        h.buf()[(x, y)].fg,
        crate::ui::palette::Palette::default().secondary,
        "the machine half"
    );
    assert_eq!(
        h.buf()[(x + 3, y)].fg,
        crate::ui::palette::Palette::default().decoration,
        "the separator is its own role"
    );
}

/// A scan holding `n` sessions on one reachable host plus one unreachable host, so
/// the nav has both of its bands: session cards over a host-state card.
fn scan_with_bands(n: usize) -> Scan {
    let mut scan = scan_with_sessions(n);
    scan.groups.push(Group {
        host: "db-2".into(),
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
    // The dead machine is FIRST in group order, and its card still lands last: a host with
    // no session to show is the tail of the list, whatever order the hosts were scanned in.
    let mut groups = vec![Group {
        host: "db-2".into(),
        err: Some("connection timed out".into()),
        sessions: vec![],
    }];
    groups.extend(sample().groups.into_iter().filter(|g| g.err.is_none()));
    let h = Harness::new(Scan { groups });
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    assert!(boundary > 0, "the session cards come first");
    for (i, row) in h.sw.rows.iter().enumerate() {
        let host_card = matches!(row.reference, RowRef::Host { .. } | RowRef::Machine { .. });
        assert_eq!(
            host_card,
            i >= boundary,
            "card {i} is on the wrong side of the boundary"
        );
    }
}

#[tokio::test]
async fn the_bands_part_with_the_rows_left_over() {
    // Both bands start at the top with one blank row between them.
    let h = Harness::new(sample());
    let boundary = h.sw.band_boundary().expect("the list has a host card");
    let host = card_rect(&h, boundary);
    let last_session = card_rect(&h, boundary - 1);
    assert_eq!(
        host.y,
        last_session.y + last_session.height + 1,
        "the host-state band follows one blank row"
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
    let mut h = Harness::from_hosts(&["local", "db-2"]);
    let sessions: Vec<crate::session::Session> = (0..27)
        .map(|i| sess("local", &format!("s{i}"), 1, false))
        .collect();
    h.sw.apply_host_result("local".into(), sessions, None, &mut h.state);
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
async fn scanning_hosts_start_at_the_top_until_found() {
    // Host cards use the first available rows even before sessions are found.
    let h = Harness::from_hosts(&["local", "jupiter00"]);
    let txt = h.nav_cards_text();
    let rows: Vec<&str> = txt.lines().collect();
    let card_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        card_rows,
        vec![0, 1],
        "both scanning hosts start at the top:\n{card_rows:?}"
    );
    // One host resolves: its section and cards lead, and the other follows.
    let mut h = Harness::from_hosts(&["local", "jupiter00"]);
    h.sw.apply_host_result(
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
            .expect("the resolved host gains a section title");
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
    h.sw.mouse_select(&h.plan, h.plan.nav_inner.x, gap_y);
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
async fn a_hosts_sessions_are_each_a_single_row_under_one_section_title() {
    // Every session of one host is a single-row card (the number + the name), stacked
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
    h.state.chrome.set_host_reach(
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
    // row count and its content untouched, and its number stays in the address column.
    // This test keeps the selection on cards.
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
    // The number stays in the address column, on the same row that carries the session.
    assert_eq!(
        h.buf()[(CARD_INDENT, beta_row)].symbol(),
        "2",
        "the selected card keeps its number in the address column"
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
    h.key(KeyCode::End).await; // last card = db-2 machine
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2")
    );
    h.key(KeyCode::Down).await; // wrap bottom → first SESSION card (row 1, under its title)
    assert_eq!(
        h.sw.selected, 1,
        "↓ from the last card wraps to the first session card"
    );
    h.key(KeyCode::Up).await; // wrap top → bottom
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2")
    );
}

#[tokio::test]
async fn horizontal_steps_one_host_and_lands_on_its_first_card() {
    // ↑/↓ and ←/→ name the two things the list is made of. ←/→ cross a whole category
    // at a time: from a session of one host the selection lands on the FIRST card of
    // the next, so a list of many hosts is crossed without stepping over every session
    // between them. The host band is the last category, entered at its first card.
    // (`sample`: local holds two sessions, jupiter00 one, db-2 is unreachable.)
    let mut h = Harness::new(sample());
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "local"),
        "the launch cursor is a local session card"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.host == "jupiter00" && sess.name == "inference"
        ),
        "→ lands on the next host's first session"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2"),
        "the host band is entered at its first card"
    );
}

#[tokio::test]
async fn the_host_band_is_one_stop_however_many_cards_it_holds() {
    // The hosts with nothing to show are ONE category to ←/→, not one each: a
    // list of machines with nothing running on them is a single thing to reach past
    // rather than a run of places to be carried into one at a time. Every one of them is
    // still a card, so ↑/↓ reach each.
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await; // local → jupiter00
    h.key(KeyCode::Right).await; // jupiter00 → the band
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2"),
        "→ enters the band at its first card"
    );
    h.key(KeyCode::Down).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-3"),
        "↓ still walks the band card by card"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "local"),
        "→ crosses the whole band in one step, from any card in it"
    );
}

#[tokio::test]
async fn leaving_the_host_band_backwards_lands_on_the_last_host_with_sessions() {
    // The band is left the same way in either direction, and from any card in it: ← from
    // its second card returns to the host before it, not to its own first card.
    let mut h = Harness::new(scan_with_a_host_band());
    h.key(KeyCode::Right).await; // local → jupiter00
    h.key(KeyCode::Right).await; // jupiter00 → the band
    h.key(KeyCode::Down).await; // the band's second card
    h.key(KeyCode::Left).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.host == "jupiter00"
        ),
        "← leaves the band for the host before it"
    );
}

#[tokio::test]
async fn horizontal_leaves_the_host_from_any_of_its_cards() {
    // The step is by HOST, not by card: it leaves the host the selection is on
    // wherever inside that host the selection sits. Stepping to the next CARD from the
    // last session of a host would look the same from that one card alone, so the
    // selection is moved off the first card of a two-session host first.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Down).await;
    assert!(
        matches!(
            h.sw.current_ref(),
            Some(RowRef::Session { sess }) if sess.host == "local" && sess.name == "editor"
        ),
        "the second local session"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "jupiter00"),
        "→ leaves the host from a card that is not its last"
    );
}

#[tokio::test]
async fn horizontal_wraps_at_both_ends() {
    // The category step wraps exactly as the card step does, so neither end of the list
    // is a dead stop.
    let mut h = Harness::new(sample());
    h.key(KeyCode::Left).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2"),
        "← from the first host wraps to the last"
    );
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.host == "local"),
        "→ from the last host wraps to the first"
    );
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
    h.sw.mouse_select(&h.plan, x, y);
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
    h.sw.set_selected(0);

    h.sw.mouse_select(&RenderPlan::default(), rect.x, rect.y);
    assert_ne!(h.sw.selected, target, "a different plan has no card there");

    h.sw.mouse_select(&frame_plan, rect.x, rect.y);
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
async fn help_overlay_renders_takes_q_as_search_and_closes_on_esc_or_prefix_help() {
    let mut h = Harness::new(sample());
    assert!(!h.text().contains("fuzzy filter"), "help hidden initially");
    h.sw.show_help(&mut h.state); // driven by the app's `prefix ?`
    h.draw();
    let out = h.text();
    assert!(
        out.contains("╭ help "),
        "show_help opens the help modal:\n{out}"
    );
    assert!(out.contains("fuzzy filter"), "help should list keybindings");
    // q is a search character: it narrows the rows and keeps the help open.
    assert!(h
        .sw
        .feed_reader_key(b"q", 0x07, &mut false, (80, 200), &mut h.state));
    h.draw();
    let out = h.text();
    assert!(out.contains("│ / q"), "q types into the search:\n{out}");
    assert!(out.contains("quit xmux"), "the match stays:\n{out}");
    assert!(
        !out.contains("fuzzy filter"),
        "the rest is filtered out:\n{out}"
    );
    // Esc closes it.
    assert!(h
        .sw
        .feed_reader_key(b"\x1b", 0x07, &mut false, (80, 200), &mut h.state));
    h.draw();
    assert!(!h.text().contains("quit xmux"), "Esc closes the help");
    // prefix ? closes it too.
    h.sw.show_help(&mut h.state);
    assert!(h
        .sw
        .feed_reader_key(b"\x07?", 0x07, &mut false, (80, 200), &mut h.state));
    assert!(
        !matches!(h.state.modal, Some(Modal::Help { .. })),
        "prefix ? closes the help"
    );
}

#[test]
fn the_help_scrolls_back_up_at_once_from_its_end() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let visible = (80u16, 11u16);
    let max = modal::help_map(
        &state.chrome.ui_prefix,
        state.chrome.nav_position,
        "",
        visible.0,
        visible.1,
    )
    .max_scroll;
    let scroll = |state: &crate::state::State| match &state.modal {
        Some(Modal::Help { scroll, .. }) => *scroll,
        _ => panic!("help closed"),
    };
    sw.feed_reader_key(b"\x1b[F", 0x07, &mut false, visible, &mut state);
    assert_eq!(
        scroll(&state),
        max,
        "End lands on the last page the paint shows"
    );
    sw.feed_reader_key(b"\x1b[A", 0x07, &mut false, visible, &mut state);
    assert_eq!(scroll(&state), max - 1, "one ↑ moves the view at once");
    sw.feed_reader_key(
        b"\x1b[6~\x1b[6~\x1b[6~",
        0x07,
        &mut false,
        visible,
        &mut state,
    );
    assert_eq!(
        scroll(&state),
        max,
        "a PgDn past the end stops at the last page"
    );
}

#[tokio::test]
async fn terminal_view_target_follows_cursor() {
    let mut h = Harness::new(sample());
    // On a session card, the target is that session (its active window follows).
    assert!(h
        .sw
        .select_address(&crate::session::Address::new("local", "editor")));
    let t = h.sw.terminal_view_target();
    assert_eq!((t.host.as_str(), t.target.as_str()), ("local", "editor"));
    // Step to the next card (the next session) - the target follows the cursor.
    h.key(KeyCode::Down).await;
    let t = h.sw.terminal_view_target();
    assert_eq!(
        (t.host.as_str(), t.target.as_str()),
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
    let mut state = crate::state::State::from_hosts(vec!["local".into(), "jupiter06".into()]);
    let sw = Switcher::from_hosts(&mut state);
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
            RowRef::Host { host, .. } | RowRef::Section { host, .. } => host.clone(),
            RowRef::Machine { machine, .. } => machine.clone(),
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
    assert_eq!((t.host.as_str(), t.target.as_str()), ("local", "build"));
    h.key(KeyCode::Down).await; // ↓ to the next local session's card
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("still a target");
    assert_eq!((t.host.as_str(), t.target.as_str()), ("local", "editor"));
}

#[tokio::test]
async fn current_host_tracks_cursor_host() {
    // The app ensures this host on every move; every card yields its host, so the
    // host's tree can be fetched.
    let mut h = Harness::new(sample()); // launch on the first local session's card
    assert_eq!(h.sw.current_host().as_deref(), Some("local"));
    h.key(KeyCode::End).await; // jump to the last card (the db-2 machine card)
    assert_eq!(h.sw.current_host().as_deref(), Some("db-2"));
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
    a.sw.mouse_scroll(true);
    let by_wheel = a.sw.selected;
    let mut b = Harness::new(sample());
    b.key(KeyCode::Down).await;
    assert_eq!(
        by_wheel, b.sw.selected,
        "wheel down lands where ↓ does (column)"
    );

    let mut c = Harness::new_sized(sample(), 60, 70);
    c.sw.mouse_scroll(true);
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
    // title carries no number at all. The selection in this test stays on a card.
    let selected = sw.selected;
    let num_w = sw.highest_number().to_string().len().max(1) as u16;
    let read =
        |x: u16, y: u16, w: u16| -> String { (x..x + w).map(|c| buf[(c, y)].symbol()).collect() };
    // Read each card where the PLAN put it: the side list parts its two bands, so a card
    // is not always the sum of the heights above it.
    assert_eq!(plan.nav_cells.len(), sw.rows.len(), "every row was drawn");
    for (i, rect) in plan.nav_cells.iter().copied() {
        if matches!(sw.rows[i].reference, RowRef::Section { .. }) {
            assert_ne!(i, selected, "this test selects a session card");
            // The section title is flush left - its machine name occupies the address
            // column - so it must simply never carry a number.
            let first = read(rect.x, rect.y, num_w).trim().to_string();
            assert!(
                first.parse::<usize>().is_err(),
                "row {i} (a section title) carries no number, got {first:?}"
            );
            continue;
        }
        let want = sw.card_number(i).to_string();
        // Every card is one row, so the number sits on that single row.
        assert_eq!(
            read(rect.x, rect.y, num_w).trim(),
            want,
            "card {i} address on its row (selected={selected})"
        );
    }
}

#[test]
fn the_armed_key_list_covers_the_grid_beside_the_nav_and_moves_no_card() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    // A grid packed edge to edge, so any cell the box fails to cover shows an `X`.
    let mut grid = crate::display::grid::Grid::new(30, 140);
    let mut fill = Vec::new();
    for r in 0..30u16 {
        fill.extend(format!("\x1b[{};1H", r + 1).bytes());
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
    let row = |term: &Terminal<TestBackend>, y: u16, x0: u16, x1: u16| -> String {
        let buf = term.backend().buffer();
        (x0..x1).map(|x| buf[(x, y)].symbol()).collect()
    };
    // Where the cards sit is what must not move when the prefix is armed.
    let cards_before = row(&term, 0, 0, NAV_WIDTH);
    state.chrome.set_armed(true);
    draw(&mut term, &mut sw, &state);
    let plan = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::visible(NAV_WIDTH),
        &state,
        &RenderPlan::default(),
    );
    let (list, _) = plan.key_list.clone().expect("the key list is open");
    assert!(list.x > NAV_WIDTH, "it opens past the nav: {list:?}");
    assert_eq!(list.bottom(), 30, "against the indicator's row: {list:?}");
    // Covering, not just recolouring: the grid's own characters would otherwise show
    // through the cells the keys do not reach.
    let text: String = (list.y..list.bottom())
        .map(|y| row(&term, y, list.x, list.right()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !text.contains('X'),
        "the box covers the grid it opens over:\n{text}"
    );
    assert!(text.contains("quit"), "{text}");
    assert_eq!(
        row(&term, 0, 0, NAV_WIDTH),
        cards_before,
        "arming the prefix only adds paint, so no card moves"
    );
}

#[tokio::test]
async fn with_the_nav_hidden_the_filter_opens_at_the_window_bottom_left() {
    // An open input must be seen even with the nav hidden (auto-hide + terminal
    // focus): its box opens where the key list does there, the window's bottom left,
    // over the grid.
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
        row.starts_with("╰") && row.contains("Enter apply · Esc cancel ╯"),
        "with the nav hidden the filter box opens at the window's bottom left: {row:?}"
    );
}

#[tokio::test]
async fn a_jump_holds_out_of_range_numbers_and_vets_at_enter() {
    // The number goes into the buffer whatever it addresses; the existence check is
    // Enter-time. While the number names no card the selection stays put, and Enter on
    // it shows the range and keeps the popup open.
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
    // Enter on a dead number refuses it in the popup and keeps the popup open.
    h.key(KeyCode::Enter).await;
    assert!(h.state.is_inputting(), "the popup stays open");
    assert!(
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.error.is_some()),
        "the popup refuses the dead number"
    );
    let row = h.popup_row(1);
    assert!(
        row.contains(&format!("✗ no card {n}")),
        "the refused number shows in the jump box: {row:?}"
    );
    assert!(h.popup_row(0).contains(" 1-"), "the meta keeps the range");
    assert_eq!(h.hint_bar_text(), " C-g", "the bar keeps resting");
    // A fresh edit clears the refusal and the input line returns.
    h.key(KeyCode::Backspace).await;
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
    // selection stays put; Enter shows the 1-based range and keeps the popup.
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
    let row = h.popup_row(1);
    assert!(
        row.split("card 0")
            .nth(1)
            .is_some_and(|rest| rest.trim_end_matches('│').trim().is_empty()),
        "the box holds the 0 and names no card: {row:?}"
    );
    h.key(KeyCode::Enter).await;
    assert!(h.state.is_inputting(), "the popup stays open");
    assert!(
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.error.as_deref() == Some("no card 0")),
        "the popup refuses the dead number"
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
        " 1 ",
        "a one-digit number sits right-aligned in the two-wide column"
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
    // selection on the seeded card, and Enter shows the range and keeps the
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
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.error.as_deref() == Some("no card 11")),
        "the popup refuses the dead number"
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

    // Armed: the prefix must still answer, so its key list opens over the window's
    // bottom left even with the nav hidden.
    state.chrome.set_armed(true);
    draw(&mut term, &mut sw, &state);
    let armed = row(&term);
    assert!(
        armed.starts_with('╰') && armed.contains("╯X"),
        "an armed prefix opens its key list over a hidden nav: {armed:?}"
    );
    state.chrome.set_armed(false);

    // Another host's scan does NOT float over a selected session. The user asked
    // for the whole live grid and the hidden nav contributes no persistent line.
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

    // Armed, the key list beside it names exactly those keys, and the bar keeps the
    // prefix.
    h.state.chrome.set_armed(true);
    h.draw();
    assert_eq!(h.state.chrome.hint_bar_text(200, &h.state).trim(), "C-g");
    let armed = h.text();
    assert!(
        armed.contains("quit") && armed.contains("help"),
        "the key list names the chords the prefix unlocks:\n{armed}"
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
    // The view tab scrolls its section, the collapse key among it, to the top.
    let r = h.plan.popup_rect;
    let inner = (r.width - 2, r.height - 2);
    h.sw.feed_reader_key(b"\x1b[C\x1b[C\x1b[C", 0x07, &mut false, inner, &mut h.state);
    h.draw();
    let view = h.text();
    assert!(
        view.contains("collapse / expand the nav"),
        "help explains the collapse key:\n{view}"
    );
    assert!(
        help.contains("previous / next section (the machine cards as one)"),
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
    assert_eq!(width, 3, "exactly the prefix wide");
    assert_eq!(left.tree, Rect::default());
    assert_eq!(left.hint_bar, Rect::new(0, 29, width, 1));
    assert_eq!(
        left.view_border,
        Rect::new(width - 1, 0, 1, 29),
        "on the prefix's last column, above the prefix row"
    );
    assert_eq!(left.terminal, Rect::new(width, 0, 140 - width, 30));

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
    assert_eq!(
        right.view_border,
        Rect::new(140 - width, 0, 1, 29),
        "on the prefix's terminal-side column, above the prefix row"
    );
    assert_eq!(right.terminal, Rect::new(0, 0, 140 - width, 30));

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
    // A side column opens across the whole row of its indicator.
    let left = hint_bar_rect(Rect::new(0, 7, 7, 1), area, true);
    assert_eq!(left, Rect::new(0, 7, 24, 1));
    let right = hint_bar_rect(Rect::new(17, 7, 7, 1), area, true);
    assert_eq!(right, Rect::new(0, 7, 24, 1));
    // A hidden nav has no indicator: the bar borrows the window's bottom row.
    let hidden = hint_bar_rect(Rect::default(), area, true);
    assert_eq!(hidden, Rect::new(0, 7, 24, 1));
    // At rest the bar is the indicator itself.
    let rest = hint_bar_rect(Rect::new(0, 7, 7, 1), area, false);
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
    h.sw.set_selected(build);
    h.sw.interest = super::Interest::Selected;
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
                              // A window taller than the help, so the popup has room to move up.
    let mut term = Terminal::new(TestBackend::new(140, 80)).unwrap();
    let before_plan = sw.layout(
        Rect::new(0, 0, 140, 80),
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
    sw.drag_popup(bx + 5, by - 1);
    let after_plan = sw.layout(
        Rect::new(0, 0, 140, 80),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &before_plan,
    );
    term.draw(|f| sw.render(f, None, false, &state, &after_plan))
        .unwrap();
    assert_eq!(after_plan.popup_rect.x, before.x + 5, "moved right by 5");
    assert_eq!(after_plan.popup_rect.y, before.y - 1, "moved up by 1");
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
        matches!(state.modal, Some(Modal::Help { .. })) && !state.is_inputting(),
        "help closes the input"
    );
    sw.open_input(InputMode::Filter, &mut state);
    assert!(
        state.is_inputting() && !matches!(state.modal, Some(Modal::Help { .. })),
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
fn a_press_anywhere_on_a_popup_grabs_it_and_outside_does_not() {
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
        !sw.begin_popup_drag_in_plan(&plan, r.right() + 1, r.y, &state),
        "a press beside the popup does not grab it"
    );
    assert!(
        sw.begin_popup_drag_in_plan(&plan, r.x + 2, r.y + 4, &state),
        "an interior press grabs the popup"
    );
    sw.drag_popup(r.x + 12, r.y + 2);
    sw.end_popup_drag();
    let moved = sw.layout(
        Rect::new(0, 0, 140, 30),
        NavSize::hidden(NAV_WIDTH),
        &state,
        &plan,
    );
    assert_eq!(
        moved.popup_rect.x,
        r.x + 10,
        "the popup follows the pointer"
    );
}

/// The help opened on a window, 140x30 unless given, laid out and painted, with its
/// tab-finding helpers.
struct HelpOnScreen {
    state: crate::state::State,
    sw: Switcher,
    plan: RenderPlan,
    term: Terminal<TestBackend>,
    size: (u16, u16),
}

impl HelpOnScreen {
    fn open() -> Self {
        Self::open_at(140, 30)
    }

    fn open_at(w: u16, h: u16) -> Self {
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        sw.show_help(&mut state);
        let mut me = HelpOnScreen {
            state,
            sw,
            plan: RenderPlan::default(),
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
            size: (w, h),
        };
        me.paint();
        me
    }

    fn paint(&mut self) {
        self.plan = self.sw.layout(
            Rect::new(0, 0, self.size.0, self.size.1),
            NavSize::hidden(NAV_WIDTH),
            &self.state,
            &self.plan,
        );
        let (sw, state, plan) = (&self.sw, &self.state, &self.plan);
        self.term
            .draw(|f| sw.render(f, None, false, state, plan))
            .unwrap();
    }

    fn inner(&self) -> (u16, u16) {
        let r = self.plan.popup_rect;
        (r.width - 2, r.height - 2)
    }

    /// The screen cell of the tab row that names `section`'s tab, or the gap after `section`.
    fn tab_cell(&self, section: usize, gap: bool) -> (u16, u16) {
        let r = self.plan.popup_rect;
        let (inner, visible) = self.inner();
        let (prefix, pos) = (&self.state.chrome.ui_prefix, self.state.chrome.nav_position);
        let (scroll, tab) = match &self.state.modal {
            Some(Modal::Help { scroll, tab, .. }) => (*scroll, *tab),
            _ => panic!("the help is open"),
        };
        let at = |x| modal::help_tab_at(prefix, pos, "", scroll, tab, inner, visible, x);
        let x = if gap {
            (1..inner).find(|&x| at(x - 1) == Some(section) && at(x).is_none())
        } else {
            (0..inner).find(|&x| at(x) == Some(section))
        }
        .expect("the tab is on the row");
        (r.x + 1 + x, r.y + 1 + modal::HELP_TAB_ROW)
    }

    /// `(scroll, tab, hover)` of the help.
    fn help(&self) -> (usize, Option<usize>, Option<usize>) {
        match &self.state.modal {
            Some(Modal::Help {
                scroll, tab, hover, ..
            }) => (*scroll, *tab, *hover),
            _ => panic!("the help is open"),
        }
    }

    /// The text of the help's first body row as painted.
    fn top_body_row(&self) -> String {
        self.inner_row(modal::HELP_LEAD as u16)
    }

    /// The text of the popup's inner row `y`, counted from its inner top, as painted.
    fn inner_row(&self, y: u16) -> String {
        let r = self.plan.popup_rect;
        let buf = self.term.backend().buffer();
        (r.x + 1..r.right() - 1)
            .map(|x| buf[(x, r.y + 1 + y)].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
    }

    fn section_title(&self, section: usize) -> String {
        modal::help_rows(&self.state.chrome.ui_prefix, self.state.chrome.nav_position)
            .into_iter()
            .filter_map(|r| match r {
                modal::HelpRow::Head(h) => Some(h),
                _ => None,
            })
            .nth(section)
            .expect("the section")
    }
}

#[test]
fn a_rule_parts_the_help_tabs_from_the_body_at_every_size() {
    for (w, h) in [(140, 30), (40, 12)] {
        let mut on = HelpOnScreen::open_at(w, h);
        let (inner, visible) = on.inner();
        assert_eq!(
            on.inner_row(modal::HELP_TAB_ROW + 1),
            "─".repeat(inner as usize),
            "{w}x{h}: a rule spans the row under the tabs"
        );
        let r = on.plan.popup_rect;
        let y = r.y + 1 + modal::HELP_TAB_ROW + 1;
        let buf = on.term.backend().buffer();
        assert_eq!(
            buf[(r.x + 1, y)].fg,
            buf[(r.x, y)].fg,
            "{w}x{h}: the rule is the border's colour"
        );
        assert_eq!(
            on.top_body_row(),
            on.section_title(0),
            "{w}x{h}: the body starts under the rule"
        );
        // A click on a tab the row shows, the second where it fits, names that tab and
        // brings its section up under the rule.
        let (prefix, pos) = (&on.state.chrome.ui_prefix, on.state.chrome.nav_position);
        let shown = (0..inner)
            .filter_map(|x| modal::help_tab_at(prefix, pos, "", 0, None, inner, visible, x))
            .max()
            .expect("a tab on the row")
            .min(1);
        let (col, row) = on.tab_cell(shown, false);
        assert!(on
            .sw
            .begin_popup_drag_in_plan(&on.plan, col, row, &on.state));
        on.sw.end_popup_drag_in_plan(&on.plan, &mut on.state);
        assert_eq!(on.help().1, Some(shown), "{w}x{h}: the click chose its tab");
        on.paint();
        assert_eq!(on.top_body_row(), on.section_title(shown), "{w}x{h}");
        // End scrolls to the last display row, and it is painted on the last inner row.
        on.sw
            .feed_reader_key(b"\x1b[F", 0x07, &mut false, (inner, visible), &mut on.state);
        on.paint();
        let (_, all) = modal::help_lines(
            &on.state.chrome.ui_prefix,
            on.state.chrome.nav_position,
            &crate::ui::palette::Palette::default(),
            "",
            0,
            None,
            None,
            u16::MAX,
            inner,
        );
        let tail = all.last().expect("a body row").to_string();
        assert_eq!(
            on.inner_row(visible - 1),
            tail.trim(),
            "{w}x{h}: the last body row is reachable"
        );
    }
}

#[test]
fn a_click_on_a_help_tab_executes_it_and_a_drag_from_it_moves_the_popup() {
    let mut h = HelpOnScreen::open();
    let (col, row) = h.tab_cell(2, false);
    assert!(
        h.sw.begin_popup_drag_in_plan(&h.plan, col, row, &h.state),
        "a press on a tab grabs the popup until it is released"
    );
    h.sw.end_popup_drag_in_plan(&h.plan, &mut h.state);
    assert!(!h.sw.popup_drag_active());
    let (inner, visible) = h.inner();
    let map = modal::help_map(
        &h.state.chrome.ui_prefix,
        h.state.chrome.nav_position,
        "",
        inner,
        visible,
    );
    assert_eq!(
        h.help(),
        (map.scroll_to(2), Some(2), None),
        "the click made the tab the hard selection and scrolled its section up"
    );
    h.paint();
    // A press on a tab that moves before its release is a drag: the popup follows, and
    // the hard selection stays.
    let before = h.plan.popup_rect;
    let (col, row) = h.tab_cell(0, false);
    assert!(h.sw.begin_popup_drag_in_plan(&h.plan, col, row, &h.state));
    h.sw.drag_popup(col + 4, row);
    h.sw.end_popup_drag_in_plan(&h.plan, &mut h.state);
    assert_eq!(h.help().1, Some(2), "the drag executed nothing");
    h.paint();
    assert_eq!(h.plan.popup_rect.x, before.x + 4, "the popup moved");
    // A click between tabs names no tab and executes nothing.
    let (col, row) = h.tab_cell(0, true);
    assert!(h.sw.begin_popup_drag_in_plan(&h.plan, col, row, &h.state));
    h.sw.end_popup_drag_in_plan(&h.plan, &mut h.state);
    assert_eq!(h.help().1, Some(2));
}

#[test]
fn hovering_a_help_tab_shows_its_section_until_the_pointer_leaves() {
    let mut h = HelpOnScreen::open();
    let first = h.section_title(0);
    assert_eq!(h.top_body_row(), first);
    let (col, row) = h.tab_cell(2, false);
    h.sw.hover_popup(&h.plan, col, row, &mut h.state);
    assert_eq!(
        h.help(),
        (0, None, Some(2)),
        "the hover leaves the hard selection where it was"
    );
    h.paint();
    assert_eq!(
        h.top_body_row(),
        h.section_title(2),
        "the body shows its section"
    );
    let buf = h.term.backend().buffer();
    assert!(
        buf[(col, row)].modifier.contains(Modifier::UNDERLINED),
        "the hovered tab is drawn apart"
    );
    let (lit_col, _) = h.tab_cell(0, false);
    assert!(
        !buf[(lit_col, row)].modifier.contains(Modifier::UNDERLINED)
            && on_accent(&buf[(lit_col, row)]),
        "the hard-selected tab keeps its own look"
    );
    // The pointer moves down onto the body: the body returns to the hard selection.
    h.sw.hover_popup(&h.plan, col, row + 2, &mut h.state);
    assert_eq!(h.help(), (0, None, None));
    h.paint();
    assert_eq!(h.top_body_row(), first);
}

/// The text inside the open popup's border, row by row.
fn popup_rows(h: &Harness) -> Vec<String> {
    let r = h.plan.popup_rect;
    let buf = h.buf();
    (r.y + 1..r.bottom() - 1)
        .map(|y| {
            (r.x + 1..r.right() - 1)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn a_small_window_shows_the_whole_help_by_scrolling() {
    let mut h = Harness::new_sized(sample(), 40, 12);
    h.sw.show_help(&mut h.state);
    h.draw();
    let r = h.plan.popup_rect;
    assert!(r.width <= 40 && r.height <= 12 && !r.is_empty(), "{r:?}");
    let inner = (r.width - 2, r.height - 2);
    let mut seen = String::new();
    for _ in 0..200 {
        let rows = popup_rows(&h);
        seen.push_str(&rows[modal::help_lead(inner.1)..].concat());
        h.sw.feed_reader_key(b"\x1b[B", 0x07, &mut false, inner, &mut h.state);
        h.draw();
    }
    let seen: String = seen.split_whitespace().collect();
    let squeeze = |t: &str| t.split_whitespace().collect::<String>();
    for row in modal::help_rows(&h.state.chrome.ui_prefix, h.state.chrome.nav_position) {
        let (modal::HelpRow::Head(t) | modal::HelpRow::Key(t, _)) = &row;
        assert!(seen.contains(&squeeze(t)), "{t:?} shown");
        if let modal::HelpRow::Key(_, d) = &row {
            assert!(seen.contains(&squeeze(d)), "{d:?} shown whole");
        }
    }
}

#[tokio::test]
async fn a_narrow_window_wraps_the_palette_descriptions() {
    let mut h = Harness::new_sized(sample(), 44, 30);
    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let rows = popup_rows(&h);
    let squeezed: String = rows.concat().split_whitespace().collect();
    let first =
        h.sw.palette_entries(&h.state, "")
            .into_iter()
            .next()
            .expect("a command")
            .0;
    let desc = first.split_once("  ").map_or(first.as_str(), |(d, _)| d);
    let desc: String = desc.split_whitespace().collect();
    assert!(squeezed.contains(&desc), "{desc} in {rows:#?}");
}

#[test]
fn the_key_list_drags_and_the_popup_its_key_opens_keeps_the_place() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let area = Rect::new(0, 0, 140, 30);
    let nav = NavSize::visible(NAV_WIDTH);
    state.chrome.set_armed(true);
    let plan = sw.layout(area, nav, &state, &RenderPlan::default());
    let (list, _) = plan
        .key_list
        .clone()
        .expect("a live prefix opens the key list");
    assert!(sw.begin_popup_drag_in_plan(&plan, list.x + 3, list.y + 1, &state));
    sw.drag_popup(list.x - 7, list.y - 4);
    sw.end_popup_drag();
    let moved = sw.layout(area, nav, &state, &plan);
    let (dragged, _) = moved.key_list.clone().unwrap();
    assert_eq!((dragged.x + 10, dragged.y + 5), (list.x, list.y));
    // The popup a prefix key opens takes the place the key list was dragged to.
    sw.show_help(&mut state);
    let undragged = {
        let mut fresh = Switcher::new(&mut state);
        fresh.show_help(&mut state);
        fresh.layout(area, nav, &state, &moved).popup_rect
    };
    let help = sw.layout(area, nav, &state, &moved).popup_rect;
    assert_eq!(
        help.x + 10,
        undragged.x,
        "the help is as tall as the window, so only x moves"
    );
    // Once neither is on screen, the next prefix starts where the key list opens.
    state.modal = None;
    state.chrome.set_armed(false);
    sw.settle_popup_position(&state);
    state.chrome.set_armed(true);
    let again = sw.layout(area, nav, &state, &moved);
    assert_eq!(again.key_list.unwrap().0, list);
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
    assert!(!matches!(state.modal, Some(Modal::Help { .. })));
    sw.toggle_help(&mut state);
    assert!(matches!(state.modal, Some(Modal::Help { .. })));
    sw.toggle_help(&mut state);
    assert!(!matches!(state.modal, Some(Modal::Help { .. })));
}

#[test]
fn feed_reader_key_is_modal_searches_and_closes_on_esc() {
    // tmux view-mode style: while open, every key is consumed; the help takes typing as
    // its search, Esc closes it; while closed, nothing is consumed (falls through).
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    assert!(
        !sw.feed_reader_key(b"q", 0x07, &mut false, (80, 200), &mut state),
        "closed → not consumed, routes normally"
    );

    sw.toggle_help(&mut state);
    assert!(
        sw.feed_reader_key(b"q", 0x07, &mut false, (80, 200), &mut state),
        "open → consumed"
    );
    assert!(
        matches!(&state.modal, Some(Modal::Help { query, .. }) if query == "q"),
        "q types into the search and keeps help open"
    );
    assert!(
        sw.feed_reader_key(
            b"\x1b[6~\x1b[6~\x1b[6~",
            0x07,
            &mut false,
            (80, 200),
            &mut state
        ),
        "a scroll (ESC [) is swallowed, not a close"
    );
    assert!(
        matches!(state.modal, Some(Modal::Help { scroll, .. }) if scroll < 2),
        "a search with one match (under its head) scrolls no further than its last row"
    );

    assert!(
        sw.feed_reader_key(b"\x1b", 0x07, &mut false, (80, 200), &mut state),
        "lone Esc → consumed"
    );
    assert!(
        !matches!(state.modal, Some(Modal::Help { .. })),
        "Esc closes help"
    );
}

#[tokio::test]
async fn the_filter_opens_as_a_box_where_the_key_list_opens() {
    let mut h = Harness::new(sample());
    let cards_before = h.plan.nav_cells.clone();
    h.ch('/').await;
    assert!(h.state.is_inputting(), "input open");
    let pop = h.plan.popup_rect;
    let term = h.plan.regions.terminal;
    assert_eq!(pop.x, term.x, "beside the column");
    assert_eq!(
        pop.bottom(),
        h.plan.hint_bar_rect.bottom(),
        "against the indicator row"
    );
    assert!(h.popup_row(0).contains("╭ filter "), "{}", h.popup_row(0));
    assert!(
        h.popup_row(2).contains("Enter apply · Esc cancel ╯"),
        "{}",
        h.popup_row(2)
    );
    assert_eq!(h.plan.nav_cells, cards_before, "the nav keeps its rows");
    h.ch('b').await;
    h.ch('u').await;
    assert!(h.popup_row(1).contains("│ / bu"), "{}", h.popup_row(1));
}

#[tokio::test]
async fn input_esc_cancels_without_acting() {
    // `n` starts a session on a REACHABLE host card, so the fixture is one empty host.
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            host: "local".into(),
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

/// `name` on `jup`, listed under the mux identity `id`.
fn sess_id(name: &str, id: &str) -> Session {
    Session {
        id: id.into(),
        ..sess("jup", name, 1, false)
    }
}

/// A switcher whose selected, displayed card is `jup/api`, listed beside `jup/zeta`.
fn on_api(api: Session, zeta: Session) -> (crate::state::State, Switcher) {
    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![api, zeta],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    sw.interest = super::Interest::Selected;
    assert!(matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "api"));
    // The loop syncs the selection off the switcher; stand in for it.
    state.selection = crate::model::Selection {
        host: "jup".into(),
        session: "api".into(),
    };
    state.displayed = state.selection.clone();
    // Stable numbering, after the full scan: each card keeps its number.
    sw.set_renumbering(false, &mut state);
    sw.hold_numbers(false, &state);
    (state, sw)
}

#[test]
fn a_renamed_session_keeps_the_selection_and_the_displayed_record() {
    // The selected, displayed session is renamed under the user: the mux lists it under
    // its own identity with a new name. The card they are on is still the card they are
    // on, under its new name, and the selection and the displayed record follow it, so
    // nothing reads the rename as a move elsewhere.
    let (mut state, mut sw) = on_api(sess_id("api", "7$0"), sess_id("zeta", "7$1"));
    let number = sw
        .numbers
        .get(&CardId::Session("jup".into(), "api".into()))
        .copied();
    assert!(number.is_some(), "the card has a number");

    let renamed = sw.apply_host_result(
        "jup".into(),
        vec![sess_id("web", "7$0"), sess_id("zeta", "7$1")],
        None,
        &mut state,
    );
    assert_eq!(renamed, vec![("api".into(), "web".into())]);
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "web"),
        "the selection stays on the renamed card"
    );
    assert_eq!(state.selection.session, "web");
    assert_eq!(state.displayed.session, "web");
    assert_eq!(
        sw.numbers
            .get(&CardId::Session("jup".into(), "web".into()))
            .copied(),
        number,
        "the renamed card keeps its number"
    );
}

/// The selected, displayed session is killed and an unrelated one is created between
/// two listings. By name alone that is a rename; the identities say it is not, so
/// neither the selection, the displayed record, nor the card number moves to the new
/// session.
#[test]
fn a_kill_plus_a_create_does_not_move_the_selection_to_the_new_session() {
    let (mut state, mut sw) = on_api(sess_id("api", "7$0"), sess_id("zeta", "7$1"));
    let number = sw
        .numbers
        .get(&CardId::Session("jup".into(), "api".into()))
        .copied();

    let renamed = sw.apply_host_result(
        "jup".into(),
        vec![sess_id("web", "7$2"), sess_id("zeta", "7$1")],
        None,
        &mut state,
    );
    assert!(renamed.is_empty(), "no rename: {renamed:?}");
    assert!(
        !matches!(sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "web"),
        "the selection does not land on the new session"
    );
    assert!(
        matches!(sw.selected_node(), Some(Node::Host(host)) if host == "jup"),
        "the lost session's selection moves up to its host"
    );
    assert_ne!(state.selection.session, "web");
    assert_ne!(state.displayed.session, "web");
    let web = sw
        .numbers
        .get(&CardId::Session("jup".into(), "web".into()))
        .copied();
    assert!(
        web.is_none() || web != number,
        "the new card does not take the lost one's number"
    );
}

/// A mux whose listing carries no identity cannot tell a rename from a kill plus a
/// create, so a name gone and a name new is never followed.
#[test]
fn a_listing_without_identities_never_moves_the_selection_by_shape() {
    let (mut state, mut sw) = on_api(sess_id("api", ""), sess_id("zeta", ""));

    let renamed = sw.apply_host_result(
        "jup".into(),
        vec![sess_id("web", ""), sess_id("zeta", "")],
        None,
        &mut state,
    );
    assert!(renamed.is_empty(), "no rename: {renamed:?}");
    assert!(
        matches!(sw.selected_node(), Some(Node::Host(host)) if host == "jup"),
        "the lost session's selection moves up to its host"
    );
    assert_ne!(state.selection.session, "web");
    assert_ne!(state.displayed.session, "web");
}

#[test]
fn selection_survives_a_rebuild() {
    // Selection on jup/api's card survives a bare rebuild (the same node, so the
    // selection stays put).
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state); // launch preselects the api card
    sw.interest = super::Interest::Selected;
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
    // A settled selection is enough. With nav_width == 0 the tree column and
    // its view border are gone, so the terminal view owns the left edge (x=0): the
    // live grid's content begins at column 0.
    let mut state = crate::state::State::from_scan(sample());
    let sw = Switcher::new(&mut state);
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
        "",
        0,
        None,
        None,
        200,
        u16::MAX,
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
        "",
        0,
        None,
        None,
        200,
        u16::MAX,
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
            host: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    host: "jup".into(),
                    name: "api".into(),
                    mux: String::new(),
                    id: String::new(),
                    windows: 1,
                    attached: false,
                    stopped: false,
                },
                Session {
                    host: "jup".into(),
                    name: "db".into(),
                    mux: String::new(),
                    id: String::new(),
                    windows: 1,
                    attached: false,
                    stopped: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    // Selection starts on the first session row (api). Jump to db by address.
    assert!(
        sw.select_address(&crate::session::Address::new("jup", "db")),
        "moved to jup/db"
    );
    assert_eq!(sw.terminal_view_target().target, "db");
    // Already-there → no move; unknown address → no move, selection unchanged.
    assert!(
        !sw.select_address(&crate::session::Address::new("jup", "db")),
        "already on jup/db"
    );
    assert!(
        !sw.select_address(&crate::session::Address::new("jup", "ghost")),
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

/// `n` hosts of two sessions each, named so every card is the same width.
fn column_flow_scan(hosts: &[&str], name_len: usize) -> Scan {
    let counts: Vec<(&str, usize)> = hosts.iter().map(|s| (*s, 2)).collect();
    column_flow_scan_sized(&counts, name_len)
}

/// Hosts carrying the given session counts, every card the same width. The counts
/// set each host/mux RUN's height (one expanded card over the rest collapsed), which
/// is what the column flow packs.
fn column_flow_scan_sized(hosts: &[(&str, usize)], name_len: usize) -> Scan {
    let pad = "x".repeat(name_len.saturating_sub(2));
    let mut out: Vec<(&str, Vec<Session>)> = Vec::new();
    for (host, n) in hosts {
        let mut sessions = Vec::new();
        for k in 0..*n {
            sessions.push(sess(host, &format!("{host}{pad}{k}"), 1, false));
        }
        out.push((*host, sessions));
    }
    hosts_scan(out)
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
    // A three-row band: each host's section (a title over its two sessions) fills a
    // column exactly, so the next host opens the column to its right. Reading order is
    // the fill order - down a column, then right - which is what the numbers count in.
    let (_sw, plan, _t) = portrait(column_flow_scan(&["aa", "bb", "cc"], 2), 60, 12);
    let cells = cells_of(&plan);
    assert_eq!(cells.len(), 9, "every row is placed: {cells:?}");
    for base in [0usize, 3, 6] {
        let (title, a, b) = (cells[&base], cells[&(base + 1)], cells[&(base + 2)]);
        // One column, but a session card starts at the group indent while the
        // title it hangs under holds the column's left edge.
        assert_eq!(a.x, title.x + CARD_INDENT, "a host's rows share a column");
        assert_eq!(b.x, a.x, "and the session cards line up with each other");
        assert_eq!(title.y, 0, "the section title starts its column");
        assert_eq!(title.height, 1, "a title is one row");
        assert_eq!(a.y, 1, "the first session hangs directly under it");
        assert_eq!(a.height, 1, "a session card is one row");
        assert_eq!(b.y, 2, "the second session under that");
    }
    assert!(
        cells[&0].x < cells[&3].x && cells[&3].x < cells[&6].x,
        "later hosts open columns to the right: {cells:?}"
    );
}

#[tokio::test]
async fn moving_selection_does_not_reflow_machine_cards_in_a_band() {
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
        .map(|(host, error)| Group {
            host: host.into(),
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
    // than leaving a card behind at the foot of the column: a host's rows stay
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
                host: "aa".into(),
                err: None,
                sessions: vec![sess("aa", "a0", 1, false), sess("aa", "a1", 1, false)],
            },
            Group {
                host: "bb".into(),
                err: None,
                sessions: vec![sess("bb", "b0", 1, false), sess("bb", "b1", 1, false)],
            },
            Group {
                host: "dead".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ],
    };
    let (_sw, plan, term) = portrait(scan, 60, 12);
    let cells = cells_of(&plan);
    // Two sections (6 rows) + one machine card.
    assert_eq!(cells.len(), 7, "every row is placed: {cells:?}");
    let host = cells[&6];
    let sess = cells[&0];
    assert!(
        host.x > sess.x,
        "the machine card is in a column of its own, right of the sessions"
    );
    // The machine card follows the session columns with one blank column between them.
    let band_w = term.backend().buffer().area.width;
    assert!(host.right() < band_w, "unused room remains on the right");
    assert!(host.x > sess.x + sess.width, "blank columns part the bands");
}

#[test]
fn portrait_scanning_hosts_start_at_the_left_until_found() {
    // Host cards begin at the left edge before any session is found.
    let scan = Scan {
        groups: vec![
            Group {
                host: "local".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                host: "jupiter00".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                host: "prod".into(),
                err: None,
                sessions: vec![],
            },
        ],
    };
    let (_sw, plan, term) = portrait(scan, 60, 12);
    let cells = cells_of(&plan);
    let band_w = term.backend().buffer().area.width;
    let x0 = cells[&0].x;
    assert!(x0 == 0, "the host band begins at the left:\n{cells:?}");
    for i in 1..3 {
        assert_eq!(cells[&i].x, x0, "every scanning host shares that column");
    }
    assert!(
        cells[&0].width < 20,
        "the status word takes no column width"
    );
    let row = (0..band_w)
        .map(|x| term.backend().buffer()[(x, cells[&0].y)].symbol())
        .collect::<String>();
    assert!(
        row.contains("no sessions"),
        "the selected status floats over the row: {row}"
    );
}

#[test]
fn floating_host_status_has_highlighted_padding_on_both_sides() {
    let scan = Scan {
        groups: vec![Group {
            host: "local".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let (_sw, plan, term) = portrait(scan, 60, 12);
    let card = plan.nav_cells[0].1;
    let buf = term.backend().buffer();
    let label = " no sessions ";
    let start = (0..=buf.area.width - label.len() as u16)
        .find(|&x| {
            (x..x + label.len() as u16)
                .map(|cell_x| buf[(cell_x, card.y)].symbol())
                .collect::<String>()
                == label
        })
        .expect("the status has one space on each side");
    assert!(
        (start..start + label.len() as u16).all(|x| on_accent(&buf[(x, card.y)])),
        "both spaces belong to the highlighted status"
    );
    assert!(card.width < 20, "the status does not widen the card");
}

#[test]
fn floating_host_status_preserves_the_selected_card_in_a_narrow_band() {
    let scan = Scan {
        groups: vec![Group {
            host: "very-long-host-name".into(),
            err: Some("refused".into()),
            sessions: vec![],
        }],
    };
    let (_sw, plan, term) = portrait(scan, 24, 12);
    let card = plan.nav_cells[0].1;
    let row = (0..24)
        .map(|x| term.backend().buffer()[(x, card.y)].symbol())
        .collect::<String>();
    assert!(
        (0..24).any(|x| on_accent(&term.backend().buffer()[(x, card.y)])),
        "the selected card remains identifiable: {row}"
    );
    assert!(
        row.contains("unreachable"),
        "the status stays visible: {row}"
    );
    assert!(
        row.contains(" unreachable "),
        "the narrow status still has both spaces: {row}"
    );
    let buf = term.backend().buffer();
    let label = " unreachable ";
    let start = (0..=buf.area.width - label.len() as u16)
        .find(|&x| {
            (x..x + label.len() as u16)
                .map(|cell_x| buf[(cell_x, card.y)].symbol())
                .collect::<String>()
                == label
        })
        .expect("the narrow status fits inside the nav");
    assert!(
        (start..start + label.len() as u16).all(|x| on_accent(&buf[(x, card.y)])),
        "both spaces stay inside the highlighted label"
    );
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
    sw.move_to(-1);
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
    // Arming the prefix opens the key list below the seam at its right end, where the
    // indicator is, and the seam keeps the prefix.
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
        text(seam_y + 1).trim_end().ends_with('╮') && text(seam_y + 1).contains("C-g"),
        "the box's titled top border runs under the seam to its right end: {:?}",
        text(seam_y + 1)
    );
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
        on_accent(&buf[(sel_rect.x, sel_rect.y)]),
        "the selected card itself is still highlighted"
    );
}

#[tokio::test]
async fn a_host_card_names_its_mux_even_where_the_id_does_not() {
    // A machine serving ONE mux carries no mux in its host id. The card still names it:
    // a machine that reads `local/psmux` on one card and `local` on the next reads as two
    // machines.
    let mut h = Harness::from_hosts(&["local"]);
    h.state.chrome.set_host_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_host_result("local".into(), vec![], None, &mut h.state);
    h.draw();
    let out = h.text();
    assert!(
        out.contains("local/psmux"),
        "the host card names the machine and its mux:\n{out}"
    );
}

#[tokio::test]
async fn a_session_with_no_stamped_mux_takes_its_host_mux() {
    // A session created since the last enumeration carries no mux of its own. Its card
    // takes the host's, so it does not stand out from the cards beside it.
    let mut h = Harness::from_hosts(&["local"]);
    h.state.chrome.set_host_reach(
        [(
            "local".to_string(),
            reach("psmux", "this box", "", "psmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_host_result(
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
async fn a_host_screen_headline_reads_as_machine_over_mux() {
    let mut h = Harness::from_hosts(&["prod:zellij"]);
    h.state.chrome.set_host_reach(
        [(
            "prod:zellij".to_string(),
            reach("zellij", "ssh to prod", "", "ssh -- prod zellij ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_host_result(
        "prod:zellij".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    // The machine's one card opens the machine's screen; a step down opens its host's.
    h.ctrl(KeyCode::Down);
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

/// A reach entry for `host`, so a screen test states what the app would have resolved.
/// A host xmux never reached is offered under the mux it WOULD have tried. That guess
/// must not reach the screen wearing the grammar every confirmed pair wears: the screen
/// for such a host reads the machine alone.
#[tokio::test]
async fn a_host_that_answered_nothing_headlines_without_a_mux() {
    let mut h = Harness::from_hosts(&["prod"]);
    // The reach record carries the mux that was ASKED FOR, which is what the diagnostic
    // rows state; it is not an answer, and the headline must not read it as one.
    h.state.chrome.set_host_reach(
        [(
            "prod".to_string(),
            reach("tmux", "ssh to prod", "", "ssh -- prod tmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    h.key(KeyCode::Char('d')).await;
    h.draw();
    let out = h.view_text();
    assert!(
        out.lines().any(|l| l.trim() == "machine prod"),
        "the headline is the machine alone:\n{out}"
    );
    assert!(
        !out.contains("prod/tmux"),
        "no mux is claimed for a host that answered nothing:\n{out}"
    );
    // What was ASKED is still stated, in the probe the machine was sent.
    assert!(
        out.contains("prod tmux ls"),
        "the diagnostic still says what it tried:\n{out}"
    );
}

/// A host that answered enumerated THROUGH its mux, so the pair is a fact and the screen
/// reads it. An empty host answered - having no session is an answer.
#[tokio::test]
async fn a_host_that_answered_headlines_with_its_mux() {
    let mut h = Harness::from_hosts(&["fresh"]);
    h.state.chrome.set_host_reach(
        [(
            "fresh".to_string(),
            reach("tmux", "ssh to fresh", "", "ssh -- fresh tmux ls"),
        )]
        .into_iter()
        .collect(),
    );
    h.sw.apply_host_result("fresh".into(), vec![], None, &mut h.state);
    h.draw();
    let out = h.view_text();
    assert!(
        out.contains("fresh/tmux"),
        "an answered host's screen names the pair:\n{out}"
    );
}

fn reach(mux: &str, machine: &str, socket: &str, probe: &str) -> crate::ui::chrome::HostReach {
    crate::ui::chrome::HostReach {
        ssh: false,
        probe: probe.into(),
        machine: machine.into(),
        mux: mux.into(),
        // The binary a test names IS its kind: no test reaches a mux through an alias.
        kind: mux.into(),
        socket: socket.into(),
        refresh: "live updates".into(),
    }
}

/// Selects the first card of an unreachable host or machine, whatever else the nav holds.
async fn select_unreachable_host(h: &mut Harness) {
    h.key(KeyCode::End).await;
    for _ in 0..64 {
        if matches!(
            h.sw.current_ref(),
            Some(
                RowRef::Host {
                    unreachable: true,
                    ..
                } | RowRef::Machine { .. }
            )
        ) {
            return;
        }
        h.key(KeyCode::Up).await;
    }
    panic!("no unreachable host card in the nav");
}

#[tokio::test]
async fn unreachable_machine_screen_states_what_was_asked_and_over_what() {
    // The message alone says a host failed, not what xmux asked of it. The mux and the
    // machine are separate rows because they are the two things that can be wrong
    // independently, and the probe is the command itself, so the user can run it by hand
    // instead of taking the app's word for the failure.
    let mut h = Harness::from_hosts(&["prod"]);
    h.state.chrome.set_host_reach(
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
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.key(KeyCode::Char('d')).await;
    h.draw();
    // An unresolved machine has no host screen to link to, so its own screen states
    // everything that was asked of it.
    let out = h.view_text();
    for want in [
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
async fn unreachable_screen_keeps_last_success_and_folds_diagnostics() {
    let mut h = Harness::from_hosts(&["prod"]);
    h.state.chrome.host_reach.insert(
        "prod".into(),
        reach(
            "tmux",
            "ssh to prod",
            "/tmp/cm-prod.sock",
            "ssh prod tmux ls",
        ),
    );
    h.sw.apply_host_result("prod".into(), vec![], None, &mut h.state);
    let last = h.state.last_reached["prod"];
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let folded = h.view_text();
    assert!(
        folded.contains("verdict") && folded.contains("connection refused"),
        "{folded}"
    );
    assert!(
        folded.contains("last reached") && folded.contains("UTC"),
        "{folded}"
    );
    assert!(folded.contains("rescan this machine"), "{folded}");
    assert!(!folded.contains("ssh to prod"), "{folded}");
    assert_eq!(h.state.last_reached["prod"], last);
    h.key(KeyCode::Char('d')).await;
    assert!(h.view_text().contains("ssh to prod"));
}

#[tokio::test]
async fn a_host_nothing_was_resolved_for_gets_no_reach_rows() {
    // An empty map is not "reached by nothing": it is nothing resolved. Those rows are
    // absent rather than blank, the provider row's own rule, so the screen never names a
    // datum it does not have.
    let mut h = Harness::from_hosts(&["prod"]);
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    let out = h.view_text();
    for absent in ["probe", "socket", "machine"] {
        assert!(
            !out.lines()
                .skip(2)
                .any(|l| l.trim_start().starts_with(absent)),
            "no {absent:?} row:\n{out}"
        );
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
    let mut h = Harness::from_hosts(&["prod:tmux", "prod:zellij", "local"]);
    h.sw.apply_host_result(
        "prod:zellij".into(),
        vec![sess_mux("prod:zellij", "infer", "zellij")],
        None,
        &mut h.state,
    );
    h.sw.apply_host_result("local".into(), vec![], None, &mut h.state);
    h.sw.apply_host_result(
        "prod:tmux".into(),
        vec![],
        Some("no server running".into()),
        &mut h.state,
    );
    select_unreachable_host(&mut h).await;
    h.key(KeyCode::Char('d')).await;
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
        "a host on ANOTHER machine is not a sibling:\n{out}"
    );
}

#[tokio::test]
async fn unreachable_machine_screen_separates_a_standing_failure_from_a_blip() {
    // One failed sweep and a host that has not answered since launch read identically in
    // the message. The run length is what parts them, and it clears the moment the host
    // answers - a stale count would keep calling a live host a standing failure.
    let mut h = Harness::from_hosts(&["prod"]);
    for _ in 0..3 {
        h.sw.apply_host_result(
            "prod".into(),
            vec![],
            Some("connection refused".into()),
            &mut h.state,
        );
    }
    h.key(KeyCode::Char('d')).await;
    h.draw();
    let out = h.view_text();
    assert!(out.contains("failures"), "the row is named:\n{out}");
    assert!(out.contains("3 in a row"), "and counts them:\n{out}");

    h.sw.apply_host_result(
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
async fn unreachable_machine_screen_names_the_log_file() {
    // Everything xmux dispatched and what came back is written down. The screen names the
    // file, so the full history is findable rather than being something the user has to
    // already know about.
    let mut h = Harness::from_hosts(&["prod"]);
    h.state
        .chrome
        .set_log_path("/home/h/.xmux/xmux.log.<date>".into());
    h.sw.apply_host_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.key(KeyCode::Char('d')).await;
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
    assert!(matches!(
        h.sw.current_ref(),
        Some(RowRef::Host { .. } | RowRef::Machine { .. })
    ));
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
async fn a_live_prefix_keeps_the_host_band_hidden_after_leaving_nav_from_a_session() {
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    h.draw();
    assert!(!h.nav_cards_text().contains("db-2"));
    h.state.chrome.armed = true;
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        !nav.contains("db-2") && !nav.contains("db-3"),
        "the prefix keeps the host band hidden:
{nav}"
    );
    h.state.chrome.armed = false;
    h.draw();
    assert!(
        !h.nav_cards_text().contains("db-2"),
        "the band is hidden again once the prefix ends"
    );
}

#[tokio::test]
async fn a_selected_host_card_is_painted_while_the_band_is_hidden() {
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    h.draw();
    assert!(!h.nav_cards_text().contains("db-2"));
    h.key(KeyCode::Right).await;
    h.key(KeyCode::Right).await; // the selection reaches the band
    assert!(matches!(
        h.sw.current_ref(),
        Some(RowRef::Host { .. } | RowRef::Machine { .. })
    ));
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        nav.contains("db-2"),
        "the selected card is painted:
{nav}"
    );
    h.key(KeyCode::Left).await; // back onto a session card
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        !nav.contains("db-2"),
        "the focus decision holds:
{nav}"
    );
}

#[tokio::test]
async fn a_selection_that_falls_to_its_host_card_is_painted() {
    // Logging out of the machines, or the machines losing their sessions, leaves the
    // selection on a host card while the terminal view keeps the focus.
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    for host in ["local", "jupiter00"] {
        h.sw.apply_host_result(
            host.into(),
            vec![],
            Some(crate::model::LOGGED_OUT.into()),
            &mut h.state,
        );
    }
    assert!(matches!(
        h.sw.current_ref(),
        Some(RowRef::Host { .. } | RowRef::Machine { .. })
    ));
    assert!(
        h.sw.selected < h.sw.painted_rows(),
        "the selected card is painted"
    );
    h.draw();
    assert!(
        h.plan.nav_cells.iter().any(|(i, _)| *i == h.sw.selected),
        "the selected card takes a click"
    );
}

#[tokio::test]
async fn a_jump_to_a_hidden_host_card_paints_it() {
    let mut h = Harness::new(scan_with_a_host_band());
    h.sw.sync_view_focus(true);
    let number = number_of(&h.sw, "db-2").expect("the host card is numbered");
    let digit = char::from_digit(number as u32, 10).expect("a one-digit number");
    h.sw.open_jump(digit, &mut h.state);
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Machine { machine, .. }) if machine == "db-2")
    );
    h.draw();
    let nav = h.nav_cards_text();
    assert!(
        nav.contains("db-2"),
        "the jump lands on a painted card:
{nav}"
    );
}

/// One host serving `names`, every one a session.
fn host_with(host: &str, names: &[&str]) -> Vec<Session> {
    names.iter().map(|n| sess_mux(host, n, "tmux")).collect()
}

/// The number the card naming `name` carries.
fn number_of(sw: &Switcher, name: &str) -> Option<usize> {
    (0..sw.rows.len())
        .find(|&i| match &sw.rows[i].reference {
            RowRef::Session { sess } => sess.name == name,
            RowRef::Host { host, .. } => host == name,
            RowRef::Machine { machine, .. } => machine == name,
            RowRef::Section { .. } => false,
        })
        .map(|i| sw.card_number(i))
}

#[tokio::test]
async fn default_numbers_follow_the_sorted_current_list_and_jump() {
    let mut h = Harness::new(one_host_scan("h", host_with("h", &["a", "b", "c"])));
    h.sw.apply_host_result("h".into(), host_with("h", &["a", "c"]), None, &mut h.state);
    assert_eq!(
        [number_of(&h.sw, "a"), number_of(&h.sw, "c")],
        [Some(1), Some(2)]
    );
    h.sw.apply_host_result(
        "h".into(),
        host_with("h", &["a", "aa", "c"]),
        None,
        &mut h.state,
    );
    assert_eq!(
        [
            number_of(&h.sw, "a"),
            number_of(&h.sw, "aa"),
            number_of(&h.sw, "c")
        ],
        [Some(1), Some(2), Some(3)]
    );
    h.state.filter = "c".into();
    h.sw.rebuild(&mut h.state);
    assert_eq!(number_of(&h.sw, "c"), Some(1));
    h.key(KeyCode::Char('1')).await;
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "c"));
    h.state.filter.clear();
    h.sw.rebuild(&mut h.state);
    assert_eq!(number_of(&h.sw, "c"), Some(3));
}

#[tokio::test]
async fn a_card_keeps_its_number_and_an_ended_cards_number_stays_vacant() {
    let mut h = Harness::new(one_host_scan("h", host_with("h", &["a", "b", "c"])));
    h.sw.set_renumbering(false, &mut h.state);
    assert_eq!(
        [
            number_of(&h.sw, "a"),
            number_of(&h.sw, "b"),
            number_of(&h.sw, "c")
        ],
        [Some(1), Some(2), Some(3)]
    );
    // b ends: c keeps 3 and 2 stays vacant; the screen writes the same number.
    h.sw.apply_host_result("h".into(), host_with("h", &["a", "c"]), None, &mut h.state);
    h.draw();
    assert_eq!(number_of(&h.sw, "c"), Some(3), "no card shifts");
    assert!(
        h.nav_cards_text()
            .lines()
            .any(|l| l.trim_start().starts_with("3 c")),
        "c is still drawn as 3:\n{}",
        h.nav_cards_text()
    );
    // A new card takes the next number, never the vacant one.
    h.sw.apply_host_result(
        "h".into(),
        host_with("h", &["a", "c", "d"]),
        None,
        &mut h.state,
    );
    assert_eq!(number_of(&h.sw, "d"), Some(4));
    // The same session returning under its name takes its number back.
    h.sw.apply_host_result(
        "h".into(),
        host_with("h", &["a", "b", "c", "d"]),
        None,
        &mut h.state,
    );
    assert_eq!(number_of(&h.sw, "b"), Some(2));
}

#[tokio::test]
async fn a_jump_lands_by_the_fixed_number_and_refuses_a_vacant_one() {
    let mut h = Harness::new(one_host_scan("h", host_with("h", &["a", "b", "c"])));
    h.sw.set_renumbering(false, &mut h.state);
    h.sw.apply_host_result("h".into(), host_with("h", &["a", "c"]), None, &mut h.state);
    h.draw();
    let start = h.sw.selected;
    h.key(KeyCode::Char('2')).await;
    assert_eq!(h.sw.selected, start, "no card carries 2, so nothing moves");
    h.key(KeyCode::Enter).await;
    assert!(
        h.state.is_inputting(),
        "Enter on a vacant number keeps the input"
    );
    assert!(
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.error.as_deref() == Some("no card 2")),
        "the popup refuses the dead number"
    );
    h.key(KeyCode::Esc).await;
    h.key(KeyCode::Char('3')).await;
    h.key(KeyCode::Enter).await;
    assert!(!h.state.is_inputting());
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session { sess }) if sess.name == "c"),
        "3 is c, as it was before b ended"
    );
}

#[tokio::test]
async fn a_full_rescan_deals_the_numbers_again_in_list_order() {
    let mut h = Harness::new(one_host_scan("h", host_with("h", &["a", "b", "c"])));
    h.sw.set_renumbering(false, &mut h.state);
    h.sw.apply_host_result("h".into(), host_with("h", &["a", "c"]), None, &mut h.state);
    assert_eq!(number_of(&h.sw, "c"), Some(3));
    h.sw.request_rescan(&mut h.state);
    h.sw.apply_host_result("h".into(), host_with("h", &["a", "c"]), None, &mut h.state);
    assert_eq!(
        [number_of(&h.sw, "a"), number_of(&h.sw, "c")],
        [Some(1), Some(2)],
        "the re-scan closes the vacancy"
    );
    // Once that scan has heard from every host the numbers are fixed again.
    h.sw.apply_host_result("h".into(), host_with("h", &["c"]), None, &mut h.state);
    assert_eq!(number_of(&h.sw, "c"), Some(2));
}

#[tokio::test]
async fn numbers_are_dealt_in_list_order_while_the_launch_scan_runs() {
    let mut h = Harness::from_hosts(&["alpha", "beta"]);
    h.sw.set_renumbering(false, &mut h.state);
    h.sw.apply_host_result("beta".into(), host_with("beta", &["x"]), None, &mut h.state);
    h.sw.apply_host_result(
        "alpha".into(),
        host_with("alpha", &["y"]),
        None,
        &mut h.state,
    );
    assert_eq!(
        [number_of(&h.sw, "y"), number_of(&h.sw, "x")],
        [Some(1), Some(2)],
        "the launch scan ends with the numbers in list order"
    );
    h.sw.apply_host_result(
        "beta".into(),
        host_with("beta", &["w", "x"]),
        None,
        &mut h.state,
    );
    assert_eq!(number_of(&h.sw, "x"), Some(2), "and from then on they hold");
    assert_eq!(number_of(&h.sw, "w"), Some(3));
}

/// A host with a session, a blocked machine, two unreachable ones, and a host whose listing
/// failed.
fn problem_scan() -> Scan {
    let failed = |host: &str, err: &str| Group {
        host: host.into(),
        err: Some(err.into()),
        sessions: vec![],
    };
    Scan {
        groups: vec![
            Group {
                host: "aaa".into(),
                err: None,
                sessions: vec![sess_mux("aaa", "work", "tmux")],
            },
            failed(
                "login-box",
                "alice@login-box: Permission denied (publickey,password).",
            ),
            failed("dead-1", "connection refused"),
            failed("dead-2", "connection timed out"),
            failed("list-box", "invalid tuios session listing: expected value"),
        ],
    }
}

#[tokio::test]
async fn the_check_table_groups_problem_machines_by_cause() {
    use crate::model::FailureKind;
    let mut h = Harness::new(problem_scan());
    let entries = h.sw.check_entries(&h.state);
    let rows: Vec<(&str, FailureKind)> =
        entries.iter().map(|e| (e.host.as_str(), e.kind)).collect();
    assert_eq!(
        rows,
        [
            ("login-box", FailureKind::Blocked),
            ("dead-1", FailureKind::Unreachable),
            ("dead-2", FailureKind::Unreachable),
            ("list-box", FailureKind::ListFailed),
        ]
    );
    assert_eq!(entries[1].reason, "connection refused");
    h.sw.toggle_check(&mut h.state);
    h.draw();
    let text = h.text();
    assert!(text.contains("╭ machine problems "), "{text}");
    assert!(
        text.contains(" 4 ╮"),
        "the count is the top border's meta: {text}"
    );
    assert!(text.contains("? login needed"), "{text}");
    assert!(text.contains("▲ unreachable"), "{text}");
    assert!(text.contains("✗ list failed"), "{text}");
    assert!(
        text.contains("dead-1     connection refused"),
        "a machine and its reason share one row: {text}"
    );
    assert!(text.contains("Enter open · Esc close ╯"), "{text}");
    assert!(!text.contains("hidden"), "{text}");
}

#[tokio::test]
async fn enter_on_a_blocked_machine_selects_it_and_hands_the_focus_to_its_login_pane() {
    let mut h = Harness::new(problem_scan());
    h.sw.toggle_check(&mut h.state);
    let mut armed = false;
    h.sw.feed_reader_key(b"\r", 0x07, &mut armed, (80, 20), &mut h.state);
    assert!(
        h.sw.open_checked_host(&mut h.state),
        "the login pane takes the keys"
    );
    assert!(h.state.modal.is_none(), "the table closes");
    assert!(h.sw.current_machine_blocked());
    assert_eq!(h.sw.current_host().as_deref(), Some("login-box"));
}

#[tokio::test]
async fn enter_on_a_disconnected_machine_opens_login() {
    let mut h = Harness::new(problem_scan());
    h.sw.toggle_check(&mut h.state);
    let mut armed = false;
    h.sw.feed_reader_key(b"j", 0x07, &mut armed, (80, 20), &mut h.state);
    h.sw.feed_reader_key(b"j", 0x07, &mut armed, (80, 20), &mut h.state);
    h.sw.feed_reader_key(b"\r", 0x07, &mut armed, (80, 20), &mut h.state);
    assert!(
        h.sw.open_checked_host(&mut h.state),
        "the login pane takes focus"
    );
    assert!(h.state.filter.is_empty());
    assert_eq!(h.sw.current_host().as_deref(), Some("dead-2"));
    assert_eq!(
        h.sw.selected_node(),
        Some(crate::model::Node::Machine("dead-2".into())),
        "an unreachable machine is one card, and its login is on its screen"
    );
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(crate::model::ViewScreen::Login)
    );
}

#[tokio::test]
async fn command_palette_searches_commands_and_machine_login() {
    let mut h = Harness::new(problem_scan());
    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let text = h.text();
    assert!(text.contains("commands"), "{text}");
    let mut armed = false;
    h.sw.feed_reader_key(b"rescan", 0x07, &mut armed, (80, 20), &mut h.state);
    let crate::state::Modal::Palette { query, .. } = h.state.modal.as_ref().unwrap() else {
        panic!("palette");
    };
    assert_eq!(query, "rescan");
    assert!(h
        .sw
        .palette_entries(&h.state, query)
        .iter()
        .any(|(name, _)| name.contains("rescan")));
    h.sw.feed_reader_key(
        b"\x15login dead-2",
        0x07,
        &mut armed,
        (80, 20),
        &mut h.state,
    );
    h.draw();
    assert!(h.text().contains("log in to dead-2"));
    h.sw.feed_reader_key(b"\r", 0x07, &mut armed, (80, 20), &mut h.state);
    assert_eq!(
        h.sw.take_palette_choice(&mut h.state),
        Some(crate::state::PaletteChoice::Login("dead-2".into()))
    );
    assert!(h.sw.open_host("dead-2", &mut h.state));
    assert!(h.state.filter.is_empty());
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(crate::model::ViewScreen::Login)
    );
}

#[tokio::test]
async fn the_check_table_closes_on_esc_and_its_selection_stays_on_a_row() {
    let mut h = Harness::new(problem_scan());
    h.sw.toggle_check(&mut h.state);
    let mut armed = false;
    for _ in 0..10 {
        h.sw.feed_reader_key(b"j", 0x07, &mut armed, (80, 20), &mut h.state);
    }
    assert!(matches!(
        h.state.modal,
        Some(crate::state::Modal::Check { selected: 3, .. })
    ));
    h.sw.feed_reader_key(b"\x1b", 0x07, &mut armed, (80, 20), &mut h.state);
    assert!(h.state.modal.is_none());
    // prefix h opens it and prefix h closes it again.
    h.sw.toggle_check(&mut h.state);
    h.sw.feed_reader_key(b"\x07h", 0x07, &mut armed, (80, 20), &mut h.state);
    assert!(h.state.modal.is_none());
}

#[tokio::test]
async fn prefix_r_asks_for_the_selected_machine_alone_unless_it_is_scanning() {
    let mut h = Harness::new(hosts_scan(vec![
        ("alpha", host_with("alpha", &["a"])),
        ("beta", host_with("beta", &["b"])),
    ]));
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut h.state,
    );
    let machine = crate::session::machine_of(&h.sw.current_host().unwrap()).to_string();
    assert!(matches!(&cmds[..], [Command::RescanMachine(m)] if *m == machine));
    h.sw.mark_machine_scanning(&machine, &mut h.state);
    assert_eq!(
        h.state.scanning.len(),
        1,
        "only that machine's host is in flight"
    );
    assert!(
        number_of(&h.sw, "a").is_some() && number_of(&h.sw, "b").is_some(),
        "and the cards stay on the list"
    );
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(cmds.is_empty(), "a machine is asked one thing at a time");
    assert_eq!(
        h.state.notify.last_report(),
        Some((
            format!("rescan machine {machine}").as_str(),
            crate::state::notify::Level::Warning,
            format!("{machine} is still being scanned").as_str()
        ))
    );
}

#[tokio::test]
async fn the_key_list_carries_no_scope_or_hidden_host_status() {
    let mut h = Harness::new(sample());
    h.state.chrome.armed = true;
    h.draw();
    assert!(!h.text().contains("hidden"), "{}", h.text());
    assert!(!h.text().contains("scope"), "{}", h.text());
}

#[tokio::test]
async fn numbers_stay_open_until_a_held_roster_answers() {
    // The launch roster names the remote machines after the first host already answered:
    // the numbers are dealt in list order until that roster is in.
    let mut h = Harness::from_hosts(&["beta"]);
    h.sw.set_renumbering(false, &mut h.state);
    h.sw.hold_numbers(true, &h.state);
    h.sw.apply_host_result("beta".into(), host_with("beta", &["x"]), None, &mut h.state);
    h.sw.add_host("alpha".into(), &mut h.state);
    h.sw.apply_host_result(
        "alpha".into(),
        host_with("alpha", &["y"]),
        None,
        &mut h.state,
    );
    assert_eq!(
        [number_of(&h.sw, "y"), number_of(&h.sw, "x")],
        [Some(1), Some(2)]
    );
    h.sw.hold_numbers(false, &h.state);
    h.sw.apply_host_result(
        "beta".into(),
        host_with("beta", &["w", "x"]),
        None,
        &mut h.state,
    );
    assert_eq!(number_of(&h.sw, "x"), Some(2), "released, the numbers hold");
    assert_eq!(number_of(&h.sw, "w"), Some(3));
}

/// The terminal's own cursor after the last draw, and the cell it is on.
fn hardware_cursor(h: &mut Harness) -> (u16, u16, Modifier) {
    use ratatui::backend::Backend;
    let at = h.term.backend_mut().get_cursor_position().unwrap();
    (at.x, at.y, h.buf()[(at.x, at.y)].modifier)
}

#[tokio::test]
async fn every_text_field_puts_the_hardware_cursor_on_its_caret() {
    // An input method draws what it is composing at the terminal's cursor, so each field
    // taking keys owns the cursor at its reversed caret cell.
    let mut h = Harness::new(sample());
    h.ch('/').await;
    h.ch('b').await;
    h.ch('u').await;
    let pop = h.plan.popup_rect;
    let (x, y, m) = hardware_cursor(&mut h);
    assert_eq!((x, y), (pop.x + 6, pop.y + 1), "after `/ bu`");
    assert!(m.contains(Modifier::REVERSED));
    h.key(KeyCode::Esc).await;

    h.key(KeyCode::Char('3')).await;
    let pop = h.plan.popup_rect;
    let (x, y, m) = hardware_cursor(&mut h);
    assert_eq!((x, y), (pop.x + 8, pop.y + 1), "after `card 3`");
    assert!(m.contains(Modifier::REVERSED));
    h.key(KeyCode::Esc).await;

    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let pop = h.plan.popup_rect;
    let (x, y, m) = hardware_cursor(&mut h);
    assert_eq!((x, y), (pop.x + 4, pop.y + 1), "after the palette's `: `");
    assert!(m.contains(Modifier::REVERSED));
    h.sw.toggle_palette(&mut h.state);

    h.sw.show_help(&mut h.state);
    h.draw();
    let pop = h.plan.popup_rect;
    let (x, y, m) = hardware_cursor(&mut h);
    assert_eq!((x, y), (pop.x + 4, pop.y + 1), "after the help's `/ `");
    assert!(m.contains(Modifier::REVERSED));
}

#[tokio::test]
async fn a_popover_field_puts_the_hardware_cursor_on_its_caret() {
    let mut h = Harness::new(Scan {
        groups: vec![Group {
            host: "local".into(),
            err: None,
            sessions: vec![],
        }],
    });
    h.ch('n').await;
    h.ch('x').await;
    let pop = h.plan.popup_rect;
    let (x, y, m) = hardware_cursor(&mut h);
    assert_eq!(y, pop.y + 2, "the name row");
    assert_eq!(
        h.buf()[(x - 1, y)].symbol(),
        "x",
        "just after what was typed"
    );
    assert!(m.contains(Modifier::REVERSED));
}

#[test]
fn the_login_pane_field_puts_the_hardware_cursor_on_its_caret() {
    let mut h = refused_login_harness();
    h.state
        .focus
        .set_view_focus(crate::state::ViewFocus::Terminal);
    h.draw_terminal_focused();
    let term = h.plan.regions.terminal;
    let (x, y, _) = hardware_cursor(&mut h);
    assert!(term.contains(ratatui::layout::Position { x, y }), "{x},{y}");
    assert!(
        on_accent(&h.buf()[(x, y)]) && !on_accent(&h.buf()[(x + 1, y)]),
        "on the focused field's caret, the highlight's last cell"
    );
    let row: String = (term.x..term.right())
        .map(|c| h.buf()[(c, y)].symbol().to_string())
        .collect();
    assert!(row.contains("address*"), "the focused field's row: {row:?}");
}

/// A surface that names a session apart from its host's section writes the session's
/// whole path: the machine, the mux, and the session. Each such surface is rendered and
/// its text scanned, on a machine serving one mux, where the host id is the machine
/// alone and a surface that joined the id and the session would read `gpu-01/train-llm`.
#[test]
fn every_rendered_surface_names_a_session_by_its_three_level_path() {
    use crate::ui::ops::OpResult;
    let mut h = Harness::from_hosts(&["gpu-01"]);
    h.state.chrome.set_host_reach(
        [(
            "gpu-01".to_string(),
            crate::state::HostReach {
                ssh: true,
                kind: "tmux".into(),
                ..Default::default()
            },
        )]
        .into(),
    );
    h.sw.apply_host_result(
        "gpu-01".into(),
        vec![sess_mux("gpu-01", "train-llm", "tmux")],
        None,
        &mut h.state,
    );
    let screen = |h: &Harness| {
        let buf = h.buf();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    };
    let mut frames = Vec::new();

    // The landing lists every card as its path.
    h.sw.open_landing();
    h.draw();
    frames.push(("landing", screen(&h)));

    // The jump popup names the card its number reaches.
    h.sw.open_jump('1', &mut h.state);
    h.draw();
    frames.push(("jump", screen(&h)));
    h.state.modal = None;

    // The logout confirmation opened from the session card names the machine it acts on.
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
        &mut h.state,
    );
    h.draw();
    let Some(Modal::Input(input)) = &h.state.modal else {
        panic!("logout confirmation")
    };
    assert_eq!(input.facts[0], ("machine", "gpu-01".to_string()));
    frames.push(("logout", screen(&h)));
    h.state.modal = None;

    // A re-scan that finds a new session, and a session the user created, each toast it.
    let before =
        crate::state::notify::ScanSnapshot::of(&h.state, &std::collections::HashSet::new());
    h.sw.apply_host_result(
        "gpu-01".into(),
        vec![
            sess_mux("gpu-01", "eval", "tmux"),
            sess_mux("gpu-01", "train-llm", "tmux"),
        ],
        None,
        &mut h.state,
    );
    let after = crate::state::notify::ScanSnapshot::of(&h.state, &std::collections::HashSet::new());
    let notes = before.summary(&after, |host| {
        h.state.chrome.named_mux(host, true).to_string()
    });
    h.state.notify.toast("rescan machine gpu-01", notes);
    h.sw.apply_op_result(
        OpResult::Created {
            session: sess("gpu-01", "serve", 1, false),
        },
        &mut h.state,
    );
    h.draw();
    frames.push(("toasts", screen(&h)));
    let history: Vec<String> = h
        .state
        .notify
        .history
        .iter()
        .map(|e| e.note.text.clone())
        .collect();
    frames.push(("history", history.join("\n")));

    for (surface, text) in &frames {
        for name in ["train-llm", "eval", "serve"] {
            assert!(
                !text.contains(&format!("gpu-01/{name}")),
                "{surface} names {name} without its mux:\n{text}"
            );
        }
    }
    let named = |surface: &str, path: &str| {
        let (_, text) = frames.iter().find(|(s, _)| *s == surface).unwrap();
        assert!(text.contains(path), "{surface} names {path}:\n{text}");
    };
    named("landing", "gpu-01/tmux/train-llm");
    named("jump", "gpu-01/tmux/train-llm");
    named("toasts", "gpu-01/tmux/eval");
    named("toasts", "gpu-01/tmux/serve");
    named("history", "gpu-01/tmux/serve");
}

#[test]
fn a_session_that_asked_for_attention_wears_the_alert_mark_until_cleared() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let nav_w = 30u16;
    let card_row = |sw: &Switcher, state: &crate::state::State| -> String {
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| sw.render_test(f, None, false, NavSize::visible(nav_w), state))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..nav_w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .find(|row| row.contains("build"))
            .expect("the build card is on screen")
    };
    assert!(!card_row(&sw, &state).contains('!'), "no mark at rest");
    assert!(sw.mark_alert(Address::new("local", "build")));
    assert!(
        !sw.mark_alert(Address::new("local", "build")),
        "marked once"
    );
    let row = card_row(&sw, &state);
    assert!(
        row.trim_end().ends_with("build !"),
        "the mark follows the session name:\n{row:?}"
    );
    sw.clear_alert("local", "build");
    assert!(!card_row(&sw, &state).contains('!'), "cleared once shown");
}
