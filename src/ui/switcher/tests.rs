use super::*;
use crate::session::Pane;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use std::sync::Mutex;

// --- mock ops -----------------------------------------------------------

#[derive(Default)]
struct RecordOps {
    killed: Mutex<Vec<Session>>,
    created: Mutex<Vec<String>>,
    renamed: Mutex<Vec<String>>,
    windowed: Mutex<Vec<String>>,
    split: Mutex<Vec<String>>,
    killed_windows: Mutex<Vec<String>>,
    renamed_windows: Mutex<Vec<String>>,
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
    async fn new_window(&self, source: &str, session: &str, name: &str) -> anyhow::Result<()> {
        self.windowed
            .lock()
            .unwrap()
            .push(format!("{source}/{session}:{name}"));
        Ok(())
    }
    async fn split_window(&self, source: &str, target: &str, vertical: bool) -> anyhow::Result<()> {
        self.split.lock().unwrap().push(format!(
            "{source}/{target}:{}",
            if vertical { "v" } else { "h" }
        ));
        Ok(())
    }
    async fn kill(&self, s: &Session) -> anyhow::Result<()> {
        self.killed.lock().unwrap().push(s.clone());
        Ok(())
    }
    async fn rename(&self, s: &Session, nn: &str) -> anyhow::Result<()> {
        self.renamed
            .lock()
            .unwrap()
            .push(format!("{}->{}", s.address(), nn));
        Ok(())
    }
    async fn panes(&self, _s: &Session) -> anyhow::Result<Vec<WindowPanes>> {
        Ok(Vec::new())
    }
    async fn kill_window(&self, _source: &str, target: &str) -> anyhow::Result<()> {
        self.killed_windows.lock().unwrap().push(target.to_string());
        Ok(())
    }
    async fn rename_window(
        &self,
        _source: &str,
        target: &str,
        new_name: &str,
    ) -> anyhow::Result<()> {
        self.renamed_windows
            .lock()
            .unwrap()
            .push(format!("{target}->{new_name}"));
        Ok(())
    }
}

// --- headless harness ---------------------------------------------------

struct Harness {
    sw: Switcher,
    state: crate::state::State,
    term: Terminal<TestBackend>,
    ops: RecordOps,
}

impl Harness {
    fn new(scan: Scan) -> Self {
        let backend = TestBackend::new(100, 30);
        let term = Terminal::new(backend).unwrap();
        let mut state = crate::state::State::from_scan(scan);
        let mut h = Harness {
            sw: Switcher::new(&mut state),
            state,
            term,
            ops: RecordOps::default(),
        };
        h.draw();
        h
    }

    fn from_sources(aliases: &[&str]) -> Self {
        let backend = TestBackend::new(100, 30);
        let term = Terminal::new(backend).unwrap();
        let aliases = aliases.iter().map(|s| s.to_string()).collect();
        let mut state = crate::state::State::from_sources(aliases);
        let mut h = Harness {
            sw: Switcher::from_sources(&mut state),
            state,
            term,
            ops: RecordOps::default(),
        };
        h.draw();
        h
    }

    fn hint_bar_text(&self) -> String {
        let buf = self.buf();
        let y = buf.area.height - 1;
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        line.trim_end().to_string()
    }

    /// Only the tree pane (first `TREE_WIDTH` columns) — so a hint assertion
    /// is not satisfied by the preview pane's own loading/reconnecting dialog.
    fn tree_text(&self) -> String {
        let buf = self.buf();
        let limit = TREE_WIDTH.min(buf.area.width);
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..limit {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn draw(&mut self) {
        let sw = &mut self.sw;
        let state = &self.state;
        self.term
            .draw(|f| sw.render(f, None, false, TREE_WIDTH, state))
            .unwrap();
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

    fn tree_row_of(&self, text: &str) -> Option<u16> {
        row_of(self.buf(), text, TREE_WIDTH)
    }

    fn tree_row_reversed(&self, y: u16) -> bool {
        let buf = self.buf();
        (0..TREE_WIDTH.min(buf.area.width))
            .any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
    }

    fn tree_fg_of(&self, text: &str) -> Option<Color> {
        fg_of(self.buf(), text, TREE_WIDTH)
    }

    fn tree_modifier_of(&self, text: &str) -> Option<Modifier> {
        locate(self.buf(), text, TREE_WIDTH).map(|(x, y)| self.buf()[(x, y)].modifier)
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

// --- sample data --------------------------------------------------------

fn sess(source: &str, name: &str, windows: i64, attached: bool, last: i64) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        windows,
        attached,
        last_attached: last,
    }
}

fn win(index: i64, name: &str, active: bool, panes: Vec<Pane>) -> WindowPanes {
    WindowPanes {
        index,
        name: name.into(),
        active,
        panes,
    }
}

fn pane(index: i64, active: bool, command: &str) -> Pane {
    Pane {
        index,
        active,
        command: command.into(),
    }
}

fn sample() -> Scan {
    let groups = vec![
        Group {
            source: "local".into(),
            err: None,
            sessions: vec![
                sess("local", "editor", 2, true, 200),
                sess("local", "build", 1, false, 100),
            ],
        },
        Group {
            source: "jupiter00".into(),
            err: None,
            sessions: vec![sess("jupiter00", "inference", 1, false, 300)],
        },
        Group {
            source: "db-2".into(),
            err: Some("connection timed out".into()),
            sessions: vec![],
        },
    ];
    let mut panes = HashMap::new();
    panes.insert(
        "local/editor".to_string(),
        vec![
            win(1, "shell", true, vec![pane(1, true, "bash")]),
            win(2, "logs", false, vec![pane(1, false, "tail")]),
        ],
    );
    panes.insert(
        "local/build".to_string(),
        vec![win(1, "make", true, vec![pane(1, true, "make")])],
    );
    panes.insert(
        "jupiter00/inference".to_string(),
        vec![win(1, "train", true, vec![pane(1, true, "python")])],
    );
    Scan { groups, panes }
}

fn cur_session_name(h: &Harness) -> Option<String> {
    match h.sw.current_ref()? {
        RowRef::Session(s) => Some(s.name.clone()),
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
/// out of the [`Command`]s `handle_key` returned — the off-loop op the run loop
/// would spawn. `None` when no op was queued (validation refused / cancelled).
fn only_run_op(cmds: Vec<Command>) -> Option<crate::model::MuxOp> {
    cmds.into_iter().find_map(|c| match c {
        Command::RunOp(op) => Some(op),
        _ => None,
    })
}

fn two_window_scan() -> Scan {
    let mut panes = HashMap::new();
    panes.insert(
        "jup/api".to_string(),
        vec![
            win(0, "w0", true, vec![pane(0, true, "bash")]),
            win(1, "w1", false, vec![pane(0, true, "bash")]),
        ],
    );
    Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 2, false, 100)],
        }],
        panes,
    }
}

#[test]
fn active_window_is_bold_italic() {
    // The active window of a session (the one whose live terminal is shown) reads
    // BOLD+ITALIC; an inactive window has neither.
    let h = Harness::new(two_window_scan());
    let m0 = h
        .tree_modifier_of("window 0")
        .expect("window 0 row present");
    assert!(
        m0.contains(Modifier::BOLD) && m0.contains(Modifier::ITALIC),
        "the active window is bold+italic: {m0:?}"
    );
    let m1 = h
        .tree_modifier_of("window 1")
        .expect("window 1 row present");
    assert!(
        !m1.contains(Modifier::BOLD) && !m1.contains(Modifier::ITALIC),
        "an inactive window is neither bold nor italic: {m1:?}"
    );
}

#[test]
fn set_active_window_moves_the_marker() {
    // An external active-window change (resolved via the control-client probe)
    // moves the bold+italic marker, without a full inventory refetch.
    let mut h = Harness::new(two_window_scan());
    assert!(
        h.sw.set_active_window("jup", "api", 1, &mut h.state),
        "active window moved 0 -> 1"
    );
    h.draw();
    let m1 = h
        .tree_modifier_of("window 1")
        .expect("window 1 row present");
    assert!(
        m1.contains(Modifier::BOLD) && m1.contains(Modifier::ITALIC),
        "window 1 is now the active window: {m1:?}"
    );
    let m0 = h
        .tree_modifier_of("window 0")
        .expect("window 0 row present");
    assert!(
        !m0.contains(Modifier::ITALIC),
        "window 0 no longer active: {m0:?}"
    );
    // Idempotent: re-applying the same active window reports no change.
    assert!(
        !h.sw.set_active_window("jup", "api", 1, &mut h.state),
        "no-op when already active"
    );
}

#[test]
fn select_window_follows_external_change_on_a_window_row() {
    // Selection on window 1's row; an external client switches the session's
    // active window to 0. The tree selection must follow to window 0's row.
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session)
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → window 0
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state); // ↓ → window 1
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 1, .. })
    ));
    assert!(
        sw.select_window("jup", "api", 0, &state),
        "moved to the new active window"
    );
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 0, .. })
    ));
}

#[test]
fn right_descends_left_ascends_tree_levels() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &mut state); // local host
    assert!(matches!(sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"));
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → first session
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
        "→ descends host → first session"
    );
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → first window
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })),
        "→ descends to a window"
    );
    sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state); // ← parent session
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
        "← ascends window → session"
    );
    sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state); // ← parent host
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"),
        "← ascends session → host"
    );
}

#[test]
fn up_down_move_within_level_and_hjkl_match_arrows() {
    // ↑/↓ (and k/j) move between SIBLINGS at the current tree level — they do NOT
    // descend into children. →/← (and l/h) change level (descend/ascend). (#1,#2)
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state); // local host preselected
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → editor (local's first session)
    assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"));
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "build"),
        "↓ moves to the next session sibling, not into a window"
    );
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Window { .. })),
        "→ descends into a window"
    );

    // hjkl mirror the arrows exactly.
    let mut state2 = crate::state::State::from_scan(sample());
    let mut sw2 = Switcher::new(&mut state2);
    sw2.handle_key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        &mut state2,
    ); // l == → : descend local host → editor
    assert!(
        matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
        "l descends to the first session"
    );
    sw2.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &mut state2,
    );
    assert!(
        matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "build"),
        "j == ↓"
    );
    sw2.handle_key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        &mut state2,
    );
    assert!(
        matches!(sw2.current_ref(), Some(RowRef::Window { .. })),
        "l == →"
    );
    sw2.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        &mut state2,
    );
    assert!(
        matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "build"),
        "h == ←"
    );
    sw2.handle_key(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        &mut state2,
    );
    assert!(
        matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
        "k == ↑"
    );
}

#[test]
fn active_window_pane_have_no_text_marker() {
    // The active window/pane is shown bold+italic (Row::active), not with "(active)" text.
    let w = win(2, "logs", true, vec![pane(1, true, "tail")]);
    assert_eq!(
        tree::window_label(&w),
        "window 2: logs",
        "no (active) text on the window label"
    );
    assert_eq!(
        tree::pane_label(&w.panes[0]),
        "pane 1  tail",
        "no (active) text on the pane label"
    );
}

#[test]
fn select_window_follows_from_a_session_row() {
    // When the terminal view has focus the user is no longer driving the tree
    // selection (stdin goes to the PTY), so the app only calls select_window
    // then. An active-window change must move the selection to that window even from
    // the SESSION row — this is how focus→terminal and in-mux window navigation keep
    // the tree view mirroring the displayed window (#3).
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session): launch preselects the host row
    assert!(matches!(sw.current_ref(), Some(RowRef::Session(_))));
    assert!(
        sw.select_window("jup", "api", 1, &state),
        "follows from the session row to window 1"
    );
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 1, .. })
    ));
}

#[test]
fn select_active_window_moves_to_cached_active_window() {
    // focus→terminal: with the selection on the session row, select_active_window moves
    // it to the session's currently-active window (from cached panes) so the
    // tree view mirrors the window the mux is displaying (#3). Window 0 is active.
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session): launch preselects the host row
    assert!(matches!(sw.current_ref(), Some(RowRef::Session(_))));
    assert!(
        sw.select_active_window(&mut state),
        "moved to the cached active window"
    );
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 0, .. })
    ));
    // Idempotent: re-applying when already on the active window reports no move.
    assert!(
        !sw.select_active_window(&mut state),
        "no-op when already on the active window"
    );
}

#[test]
fn select_active_window_descends_from_a_host_row() {
    // focus→terminal from a HOST row must descend into the host's recent session's active
    // window (the window the mux displays), not leave the selection stuck on the host.
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state); // launch preselects the host row
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Host { .. })),
        "selection on the host row"
    );
    assert!(
        sw.select_active_window(&mut state),
        "descends from host to the active window"
    );
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })),
        "landed on the recent session's active window (0)"
    );
}

#[test]
fn select_window_no_move_for_another_session() {
    // A window change on a session the selection is NOT on must not move it.
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session): launch preselects the host row
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → window 0
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state); // ↓ → window 1
    assert!(!sw.select_window("jup", "other", 0, &state));
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 1, .. })
    ));
}

// --- tests --------------------------------------------------------------

#[tokio::test]
async fn renders_four_level_tree() {
    let h = Harness::new(sample());
    let out = h.text();
    for want in [
        "local",
        "editor",
        "window 1: shell",
        "pane 1  bash",
        "window 2: logs",
        "build",
        "jupiter00",
        "inference",
        "window 1: train",
        "pane 1  python",
        "db-2",
        "⚠", // unreachable host marker (the reason now lives in the info pane)
    ] {
        assert!(out.contains(want), "tree missing {want:?}\n{out}");
    }
}

#[tokio::test]
async fn launch_preselects_top_local_host_row() {
    // #G: on launch the highlight sits on the very top row — the local host
    // (index 0) — regardless of recency; no persisted last_session is consulted.
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false, 100)],
        None,
        &mut h.state,
    );
    // A more-recent remote streams in and must NOT pull the cursor down.
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(h.sw.selected, 0, "the launch cursor is the very top row");
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"),
        "the top row is the local host"
    );
}

#[tokio::test]
async fn panes_are_not_selectable() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Window { .. })),
        "→ reaches a window"
    );
    // → on a window does NOT descend onto a pane (panes are not selectable).
    h.key(KeyCode::Right).await;
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Window { .. })),
        "→ on a window is a no-op (its panes are not selectable)"
    );
    // ↓/↑ cycle window siblings; the selection must never land on a pane.
    let mut saw_window = false;
    for _ in 0..8 {
        let r = h.sw.current_ref();
        assert!(r.is_some(), "selection landed on a node");
        assert!(
            !matches!(r, Some(RowRef::Pane)),
            "selection must never land on a pane"
        );
        if matches!(r, Some(RowRef::Window { .. })) {
            saw_window = true;
        }
        h.key(KeyCode::Down).await;
    }
    assert!(saw_window, "navigation reaches window nodes");
}

#[tokio::test]
async fn rescan_resets_to_scanning_skeleton() {
    // `r` resets every host to its scanning state and signals the loop to
    // re-kick the probes — the tree returns to skeletons until results land.
    let mut h = Harness::new(sample());
    assert!(h.text().contains("inference"), "sessions before rescan");
    h.ch('r').await;
    assert!(
        h.sw.take_rescan_kick(),
        "rescan must signal the loop to re-probe"
    );
    let tree = h.tree_text();
    assert!(
        tree.contains("scanning"),
        "hosts return to scanning after rescan:\n{tree}"
    );
    assert!(
        !tree.contains("inference"),
        "stale sessions clear until the re-probe lands:\n{tree}"
    );
}

// --- streaming model (render-first, per-element) ------------------------

#[tokio::test]
async fn from_sources_renders_scanning_skeletons() {
    // The first frame: one host-skeleton row per source, each in a scanning
    // state, before ANY probe result lands. Structure first, data later.
    let h = Harness::from_sources(&["local", "jupiter00"]);
    let out = h.text();
    assert!(out.contains("local"), "host skeleton present:\n{out}");
    assert!(out.contains("jupiter00"), "host skeleton present:\n{out}");
    assert!(
        out.contains("scanning"),
        "each host shows a scanning status:\n{out}"
    );
    assert!(
        !out.contains("window"),
        "no pane detail before any probe:\n{out}"
    );
}

#[tokio::test]
async fn apply_source_result_turns_scanning_into_sessions() {
    let mut h = Harness::from_sources(&["local"]);
    assert!(h.text().contains("scanning"), "scanning before the result");
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 2, false, 100)],
        None,
        &mut h.state,
    );
    h.draw();
    let out = h.tree_text();
    assert!(
        out.contains("editor"),
        "session appears after result:\n{out}"
    );
    assert!(
        !out.contains("scanning"),
        "scanning status clears once the only host resolves:\n{out}"
    );
    assert!(
        out.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
        "the session shows a progress spinner until its panes arrive:\n{out}"
    );
}

#[tokio::test]
async fn poll_preserves_session_order_after_scan() {
    // Scan establishes recency order db(200), web(100). A later poll reports web
    // freshly attach-bumped (999) — live recency would float it to the top, but a
    // routine poll must hold the established order.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false, 100),
            sess("local", "db", 1, false, 200),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "the scan applies recency order"
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false, 200),
            sess("local", "web", 1, false, 999),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "a routine poll must not re-sort the tree under the user"
    );
}

#[tokio::test]
async fn poll_appends_new_session_at_group_end() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false, 100),
            sess("local", "db", 1, false, 200),
        ],
        None,
        &mut h.state,
    ); // → db, web
       // A poll surfaces a brand-new session `api` (most recent). It appends last,
       // never displacing the frozen db, web order.
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false, 200),
            sess("local", "web", 1, false, 100),
            sess("local", "api", 1, false, 999),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web", "api"],
        "a session new since the scan appends at the group's end"
    );
}

#[tokio::test]
async fn poll_preserves_host_group_order_after_scan() {
    // Scan settles the host order: local pinned first, then jupiter00 (recency 300)
    // above jupiter06 (recency 100).
    let mut h = Harness::from_sources(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "w", 1, false, 50)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "b", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "a", 1, false, 300)],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_order(&h),
        vec!["local", "jupiter00", "jupiter06"],
        "the scan orders hosts local-first then by recency"
    );
    // A poll reports jupiter06's session freshly bumped (999) — live recency would
    // lift jupiter06 above jupiter00, but the group order is frozen after the scan.
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "b", 1, false, 999)],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_order(&h),
        vec!["local", "jupiter00", "jupiter06"],
        "a routine poll must not reorder host groups"
    );
}

#[tokio::test]
async fn rescan_reapplies_recency_order() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "web", 1, false, 100),
            sess("local", "db", 1, false, 200),
        ],
        None,
        &mut h.state,
    ); // → db, web
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false, 200),
            sess("local", "web", 1, false, 999),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["db", "web"],
        "the poll held the order"
    );
    // The `r` re-scan clears sessions + re-seeds scanning; the next result is a
    // scan-driven one, so recency (web=999 now leads) is re-applied.
    h.sw.request_rescan(&mut h.state);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "db", 1, false, 200),
            sess("local", "web", 1, false, 999),
        ],
        None,
        &mut h.state,
    );
    assert_eq!(
        group_session_names(&h, "local"),
        vec!["web", "db"],
        "a re-scan re-applies recency order"
    );
}

/// Streams the sample three-host tree (local/jupiter00/jupiter06), each with one
/// session, and leaves the selection on the MIDDLE host's session.
async fn three_hosts_cursor_on_middle() -> Harness {
    let mut h = Harness::from_sources(&["local", "jupiter00", "jupiter06"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter06".into(),
        vec![sess("jupiter06", "build", 1, false, 50)],
        None,
        &mut h.state,
    );
    assert!(h.sw.select_address("jupiter00/infer", &h.state));
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
        vec![sess("jupiter06", "build", 1, false, 50)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
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
    // Sessions re-stream — the selection must NOT get yanked back to infer.
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "web", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
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
    let out = h.text();
    assert!(
        out.contains("empty"),
        "a reachable host with no sessions reads (empty):\n{out}"
    );
    assert!(!out.contains("scanning"), "no longer scanning:\n{out}");
}

#[tokio::test]
async fn apply_source_result_unreachable_marks_tree_and_reason_in_info_pane() {
    let mut h = Harness::from_sources(&["prod"]);
    h.sw.apply_source_result(
        "prod".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    h.draw();
    // Tree: only the ⚠ marker beside the name — not the verbose reason.
    let tree = h.tree_text();
    assert!(tree.contains('⚠'), "the host row is marked with ⚠:\n{tree}");
    assert!(
        !tree.contains("connection refused"),
        "the reason does NOT clutter the tree:\n{tree}"
    );
    // The lone unreachable host is auto-selected → its right-pane info panel
    // states it is unreachable and shows why.
    let out = h.text();
    assert!(
        out.contains("unreachable"),
        "info pane states unreachable:\n{out}"
    );
    assert!(
        out.contains("connection refused"),
        "info pane shows the failure reason:\n{out}"
    );
}

#[tokio::test]
async fn unreachable_info_pane_shows_ssh_config_stanza() {
    let mut h = Harness::from_sources(&["jupiter00"]);
    h.state.chrome.set_ssh_config_text(
            "Host jupiter00\n    HostName 143.248.140.120\n    User hrlee\n\nHost other\n    HostName 1.2.3.4\n".into(),
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
async fn apply_panes_attaches_and_clears_loading() {
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.draw();
    assert!(
        h.tree_text()
            .chars()
            .any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
        "a progress spinner stands in before panes land"
    );
    h.sw.apply_panes(
        "local/editor".into(),
        vec![win(1, "shell", true, vec![pane(1, true, "bash")])],
        &mut h.state,
    );
    h.draw();
    let out = h.tree_text();
    assert!(
        out.contains("window 1: shell"),
        "panes attach under the session:\n{out}"
    );
    assert!(
        !out.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
        "the progress spinner clears once panes arrive:\n{out}"
    );
}

#[tokio::test]
async fn streaming_keeps_local_preselect_when_untouched() {
    // An untouched selection sits on the top row (the local host, index 0), and a
    // later more-recent REMOTE session streaming in must NOT steal it: the selection
    // must not leap to a remote on first launch (#1).
    let mut h = Harness::from_sources(&["local", "jupiter00"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![sess("local", "editor", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        h.sw.selected, 0,
        "the selection stays on the local host row"
    );
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        h.sw.selected, 0,
        "an untouched selection stays on the local host row (index 0); a recent remote must not steal it"
    );
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"),
        "the untouched selection is the local host row"
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
    // must keep it there, not snap it back to the recency/preferred preselect.
    let mut state = crate::state::State::from_sources(vec!["h".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    sw.apply_source_result(
        "h".into(),
        vec![sess("h", "a", 1, false, 200), sess("h", "b", 1, false, 100)],
        None,
        &mut state,
    );
    let names: Vec<String> = sw
        .rows
        .iter()
        .filter_map(|r| match &r.reference {
            RowRef::Session(s) => Some(s.name.clone()),
            _ => None,
        })
        .collect();
    // Pick the session that is NOT the preselect target (the recency-first one), so a
    // bare rebuild's preselect would move the selection here if the fix were absent.
    let other = names[1].clone();
    let idx = sw
        .rows
        .iter()
        .position(|r| matches!(&r.reference, RowRef::Session(s) if s.name == other))
        .expect("other session row");
    sw.set_selected(idx, &state);
    sw.user_moved = true;
    sw.rebuild(&mut state);
    let got = match sw.current_ref() {
        Some(RowRef::Session(s)) => s.name.clone(),
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
            sess("local", "editor", 1, false, 100),
            sess("local", "build", 1, false, 50),
        ],
        None,
        &mut h.state,
    );
    h.draw();
    // local host preselected; descend to editor, then move down to build.
    h.key(KeyCode::Right).await;
    h.key(KeyCode::Down).await;
    assert_eq!(cur_session_name(&h).as_deref(), Some("build"));
    // A more-recent remote session streams in; the selection must NOT jump.
    h.sw.apply_source_result(
        "jupiter00".into(),
        vec![sess("jupiter00", "infer", 1, false, 300)],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(
        cur_session_name(&h).as_deref(),
        Some("build"),
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
    assert!(
        hint_bar.contains("filter") || hint_bar.contains("help") || hint_bar.contains("quit"),
        "the hint_bar returns to the help line:\n{hint_bar:?}"
    );
}

#[tokio::test]
async fn hint_bar_fits_narrow_width() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(30, 30)).unwrap();
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer();
    let y = buf.area.height - 1;
    let mut hint_bar = String::new();
    for x in 0..buf.area.width {
        hint_bar.push_str(buf[(x, y)].symbol());
    }
    let hint_bar = hint_bar.trim_end().to_string();
    assert!(
        hint_bar.chars().count() <= 30,
        "hint_bar fits a 30-column terminal:\n{hint_bar:?}"
    );
    assert!(
        hint_bar.contains("help") || hint_bar.contains("quit"),
        "hint_bar still offers help/quit hints:\n{hint_bar:?}"
    );
}

#[test]
fn hint_bar_has_status_bar_background() {
    // The hint bar is a solid tmux-style status bar (green bg / black fg) spanning the
    // full width — including cells past the text — so it reads as chrome, clearly set
    // off from the tree rows above.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer();
    let y = buf.area.height - 1; // the one-line hint bar sits on the last row
                                 // tmux's default status-style themegreen/themeblack = yellowgreen / gray5 on truecolor.
    let bg = Color::Rgb(0x9a, 0xcd, 0x32);
    let fg = Color::Rgb(0x0d, 0x0d, 0x0d);
    assert_eq!(
        buf[(1, y)].bg,
        bg,
        "a text cell has the bar bg (yellowgreen)"
    );
    assert_eq!(buf[(1, y)].fg, fg, "the hint text is near-black (gray5)");
    assert_eq!(
        buf[(buf.area.width - 1, y)].bg,
        bg,
        "a trailing cell past the text is also the bar bg (fills full width)"
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
async fn selected_node_renders_reverse_video() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Right).await; // launch preselects the host row; descend to editor
    let sel = h.tree_row_of("editor").expect("editor row");
    let other = h.tree_row_of("inference").expect("inference row");
    assert!(h.tree_row_reversed(sel), "selected row must be reversed");
    assert!(
        !h.tree_row_reversed(other),
        "non-selected row must not be reversed"
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
async fn kill_confirm_esc_cancels() {
    let mut h = Harness::new(sample()); // launch preselects the local host row
    h.key(KeyCode::Right).await; // → editor (a local session)
    h.ch('x').await; // arm the kill y/n confirm
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(_))),
        "x arms the y/n confirm"
    );
    h.key(KeyCode::Esc).await; // Esc cancels it, like the input prompts
    assert!(
        !matches!(h.state.modal, Some(Modal::Kill(_))),
        "Esc clears the confirm"
    );
    assert!(
        h.ops.killed.lock().unwrap().is_empty(),
        "Esc must not kill anything"
    );
}

#[tokio::test]
async fn kill_removes_session_and_cache() {
    let mut h = Harness::new(sample());
    // launch preselects the local host row; descend to the editor session, then kill it.
    h.key(KeyCode::Right).await; // → editor (local)
    assert!(h.state.panes.contains_key("local/editor"));
    h.ch('x').await; // arm
    assert!(
        h.text().contains("kill local/editor?"),
        "expected inline kill confirm:\n{}",
        h.text()
    );
    h.ch('y').await;
    assert_eq!(h.ops.killed.lock().unwrap().len(), 1);
    assert_eq!(h.ops.killed.lock().unwrap()[0].name, "editor");
    assert!(
        !h.state.panes.contains_key("local/editor"),
        "kill must invalidate cache"
    );
    assert!(
        !h.text().contains("editor"),
        "killed session must disappear"
    );
}

#[tokio::test]
async fn create_adds_and_selects() {
    let mut h = Harness::new(sample());
    // launch preselects the local HOST row; n on a host row ⇒ create a session.
    h.ch('n').await; // n on a host row ⇒ create a session
    h.sw.set_input_text("scratch", &mut h.state);
    h.key(KeyCode::Enter).await;
    assert_eq!(*h.ops.created.lock().unwrap(), vec!["local/scratch"]);
    assert_eq!(cur_session_name(&h).as_deref(), Some("scratch"));
}

#[tokio::test]
async fn slow_op_is_deferred_off_the_key_path() {
    // The key-handling path must NOT perform the network create (which would
    // freeze the UI on a slow remote); it only queues the op for the loop.
    let mut h = Harness::new(sample());
    // launch preselects the local HOST row.
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
        h.sw.row_of_session("local/scratch").is_some(),
        "applying the result folds the new session into the tree"
    );
}

#[tokio::test]
async fn n_on_session_row_creates_a_window() {
    // The `n` action is level-aware: on a SESSION row it creates a window.
    let mut h = Harness::new(sample());
    // launch preselects the local HOST row; descend to the editor session row.
    h.key(KeyCode::Right).await; // → local/editor (session)
    h.ch('n').await;
    h.sw.set_input_text("logs", &mut h.state);
    h.key(KeyCode::Enter).await;
    assert_eq!(
        *h.ops.windowed.lock().unwrap(),
        vec!["local/editor:logs"],
        "n on a session row queues a new window"
    );
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "not a session create"
    );
}

#[tokio::test]
async fn n_on_window_row_splits_a_pane() {
    // On a WINDOW row, `n` splits the pane (direction from the prompt).
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host row
    h.key(KeyCode::Right).await; // → local/editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (a window row)
    h.ch('n').await;
    h.sw.set_input_text("h", &mut h.state); // horizontal split
    h.key(KeyCode::Enter).await;
    assert_eq!(
        *h.ops.split.lock().unwrap(),
        vec!["local/editor:1:h"],
        "n on a window row queues a horizontal split of that window"
    );
}

#[tokio::test]
async fn rename_targets_node_captured_at_open_not_enter() {
    // Open rename on alpha/a-sess, then let a more-recent session stream in on
    // another host. The rename must still target the session captured when the
    // input opened.
    let mut h = Harness::from_sources(&["alpha", "beta"]);
    h.sw.apply_source_result(
        "alpha".into(),
        vec![sess("alpha", "a-sess", 1, false, 100)],
        None,
        &mut h.state,
    );
    h.key(KeyCode::Right).await; // launch preselects the alpha host row; descend to a-sess
    h.ch('R').await; // capture alpha/a-sess
    h.sw.apply_source_result(
        "beta".into(),
        vec![sess("beta", "b-sess", 1, false, 999)],
        None,
        &mut h.state,
    );
    h.sw.set_input_text("renamed", &mut h.state);
    h.key(KeyCode::Enter).await;
    let renamed = h.ops.renamed.lock().unwrap();
    assert_eq!(
        *renamed,
        vec!["alpha/a-sess->renamed".to_string()],
        "rename must target the captured node, not where streaming moved the selection"
    );
}

#[tokio::test]
async fn rename_rejects_leading_dash() {
    let mut h = Harness::new(sample());
    h.ch('R').await;
    h.sw.set_input_text("-bad", &mut h.state);
    h.key(KeyCode::Enter).await;
    assert!(
        h.ops.renamed.lock().unwrap().is_empty(),
        "leading-dash rename must be refused"
    );
}

#[tokio::test]
async fn filter_leaves_cursor_on_visible_session() {
    // Filter to a session — selection must land on it after the filter completes.
    let mut h = Harness::from_sources(&["local"]);
    h.sw.apply_source_result(
        "local".into(),
        vec![
            sess("local", "live", 2, true, 999),
            sess("local", "xmux-probeL", 1, false, 50),
        ],
        None,
        &mut h.state,
    );
    h.ch('/').await;
    h.sw.set_input_text("probeL", &mut h.state);
    h.key(KeyCode::Enter).await; // apply filter
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
    // After filtering, current_attach_target on the host row yields the
    // first visible session, not a filtered-out one.
    let mut h = Harness::from_sources(&["alpha"]);
    h.sw.apply_source_result(
        "alpha".into(),
        vec![
            sess("alpha", "keep-me", 1, false, 50),
            sess("alpha", "other", 1, false, 999),
        ],
        None,
        &mut h.state,
    );
    h.ch('/').await;
    h.sw.set_input_text("keep", &mut h.state);
    h.key(KeyCode::Enter).await; // apply filter
    h.key(KeyCode::Home).await; // host row
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("host row has a visible session");
    assert_eq!(
        t.target.as_str(),
        "keep-me",
        "current_attach_target on host row under filter yields the visible session"
    );
}

#[tokio::test]
async fn create_on_unreachable_host_refused() {
    let mut h = Harness::new(sample());
    // jump to the last host row — the unreachable db-2.
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
async fn levels_have_distinct_colors() {
    let h = Harness::new(sample());
    assert_eq!(h.tree_fg_of("local"), Some(COLOR_HOST));
    assert_eq!(h.tree_fg_of("editor"), Some(COLOR_SESSION));
    assert_eq!(h.tree_fg_of("window 1: shell"), Some(COLOR_WINDOW));
    assert_eq!(h.tree_fg_of("pane 1  bash"), Some(COLOR_PANE));
}

#[tokio::test]
async fn navigation_wraps_around() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::End).await; // last node = db-2
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"));
    h.key(KeyCode::Down).await; // wrap bottom → top
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"));
    h.key(KeyCode::Up).await; // wrap top → bottom
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2"));
}

#[tokio::test]
async fn double_click_selects_node() {
    let mut h = Harness::new(sample());
    // inference preselected; double-click inside the tree moves the selection.
    let before = h.sw.selected;
    h.sw.mouse_attach(5, 4, &h.state);
    // selection moved (or stayed on the same selectable row — just check no panic
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
    h.sw.mouse_select(5, 4, &h.state);
    // After a single click the selection is on a selectable row (not pane/loading).
    let selectable = h.sw.rows.get(h.sw.selected).is_some_and(Row::selectable);
    assert!(selectable, "single click must land on a selectable row");
}

#[test]
fn menu_items_by_row_type() {
    use super::MenuItem::*;
    let host = RowRef::Host {
        source: "h".into(),
        unreachable: false,
    };
    assert_eq!(modal::menu_items(&host), vec![NewSession]);

    let s = sess("h", "api", 1, false, 0);
    assert_eq!(
        modal::menu_items(&RowRef::Session(s.clone())),
        vec![Focus, NewWindow, Rename, Kill]
    );
    assert_eq!(
        modal::menu_items(&RowRef::Window { sess: s, window: 1 }),
        vec![Focus, Rename, Kill]
    );
    assert!(modal::menu_items(&RowRef::Pane).is_empty());
    assert!(modal::menu_items(&RowRef::Loading).is_empty());
}

/// The screen (col,row) of the tree row at `idx`, given the current layout.
fn row_screen_pos(h: &Harness, idx: usize) -> (u16, u16) {
    let y = h.sw.tree_inner.y + (idx - h.sw.list_state.offset()) as u16;
    (h.sw.tree_inner.x, y)
}

fn row_index<F: Fn(&RowRef) -> bool>(h: &Harness, pred: F) -> usize {
    h.sw.rows
        .iter()
        .position(|r| pred(&r.reference))
        .expect("row exists")
}

#[tokio::test]
async fn menu_open_on_session_does_not_move_cursor() {
    let mut h = Harness::new(sample());
    let before = h.sw.selected;
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(
        h.sw.menu_open(x, y, &mut h.state),
        "menu opens over a session row"
    );
    assert!(h.state.menu_active());
    assert_eq!(
        h.sw.selected, before,
        "opening the menu must not move the tree selection"
    );
}

#[tokio::test]
async fn menu_does_not_open_on_pane_row() {
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Pane));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(
        !h.sw.menu_open(x, y, &mut h.state),
        "no menu over a pane row"
    );
    assert!(!h.state.menu_active());
}

#[tokio::test]
async fn menu_release_off_menu_cancels() {
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(_)));
    let (x, y) = row_screen_pos(&h, idx);
    h.sw.menu_open(x, y, &mut h.state);
    // Drag fully outside the box → highlight clears → release cancels.
    h.sw.menu_hover(99, 29, &mut h.state);
    assert!(matches!(h.sw.menu_release(&mut h.state), MenuOutcome::None));
    assert!(!h.state.menu_active(), "menu closes on release");
    assert!(
        !h.state.is_inputting() && !matches!(h.state.modal, Some(Modal::Kill(_))),
        "nothing happened"
    );
}

#[tokio::test]
async fn menu_release_in_place_cancels() {
    // Accidental-click safety: open then release WITHOUT dragging onto an item does
    // nothing. The pointer lands on the title row with no item pre-selected, so the
    // release lands off every item.
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(h.sw.menu_open(x, y, &mut h.state));
    assert!(
        matches!(h.sw.menu_release(&mut h.state), MenuOutcome::None),
        "no drag → no action"
    );
    assert!(!h.state.menu_active());
    assert!(
        !h.state.is_inputting() && !matches!(h.state.modal, Some(Modal::Kill(_))),
        "nothing happened"
    );
}

#[tokio::test]
async fn menu_title_row_sits_on_the_pointer() {
    // tmux-style: the title row (top border) lands on the click row, and no item is
    // pre-selected under the pointer — an accidental right-click releases off every item.
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(h.sw.menu_open(x, y, &mut h.state));
    let Some(Modal::Menu(menu)) = &h.state.modal else {
        unreachable!()
    };
    assert_eq!(menu.rect.y, y, "the title row sits on the click row");
    assert_eq!(menu.item_at(x, y), None, "no item under the pointer");
}

#[tokio::test]
async fn menu_release_focus_focuses_terminal_and_selects_target() {
    let mut h = Harness::new(sample());
    let s = sess("local", "build", 1, false, 100);
    let target = RowRef::Session(s);
    let items = modal::menu_items(&target);
    let focus_at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(focus_at),
    }));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::FocusTerminal
    ));
    assert_eq!(
        cur_session_name(&h).as_deref(),
        Some("build"),
        "selection moved to target"
    );
}

#[tokio::test]
async fn menu_release_rename_opens_input() {
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = modal::menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Rename).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(at),
    }));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::Handled
    ));
    assert!(h.state.is_inputting(), "rename opens the inline input");
}

#[tokio::test]
async fn menu_release_kill_arms_confirm() {
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = modal::menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Kill).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(at),
    }));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::Handled
    ));
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(_))),
        "kill arms the y/n confirm"
    );
}

#[tokio::test]
async fn menu_kill_keeps_the_cursor_so_the_confirm_survives() {
    // Regression: the y/n confirm flashed and vanished because moving the selection to
    // the target changed the selection → attach → events → rebuild, which clears
    // pending_kill. Acting on a row must NOT move the selection.
    let mut h = Harness::new(sample());
    let editor = row_index(
        &h,
        |r| matches!(r, RowRef::Session(s) if s.name == "editor"),
    );
    h.sw.set_selected(editor, &h.state);
    h.sw.user_moved = true;
    // Kill a DIFFERENT session ('build') via the menu.
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = modal::menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Kill).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(at),
    }));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::Handled
    ));
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(_))),
        "kill is armed against the clicked row"
    );
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
        "the selection stayed put → no selection change to rebuild away the confirm"
    );
}

#[tokio::test]
async fn kill_confirm_survives_a_rebuild_until_the_target_vanishes() {
    // The confirm must NOT have a time limit: a routine rebuild (the 1.5s local
    // poll, a remote %-event) used to clear pending_kill out from under the user.
    let mut h = Harness::new(sample());
    let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    h.sw.set_selected(build, &h.state);
    h.sw.arm_kill(&mut h.state);
    assert!(matches!(h.state.modal, Some(Modal::Kill(_))), "kill armed");
    h.sw.rebuild(&mut h.state); // a poll/event rebuild
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(_))),
        "confirm survives a routine rebuild — no time limit"
    );
    // But once the target is actually gone, the stale confirm is dropped.
    h.state.groups = crate::ui::tree::remove_session(&h.state.groups, "local/build");
    h.sw.rebuild(&mut h.state);
    assert!(
        !matches!(h.state.modal, Some(Modal::Kill(_))),
        "a vanished target invalidates the confirm"
    );
}

#[tokio::test]
async fn menu_focus_window_marks_it_active_so_passthrough_follow_keeps_it() {
    // Regression: focusing a different window of the already-displayed session must
    // move there. Without optimistically marking it active, select_active_window
    // (the terminal-view follow) yanks the selection back to the old active window.
    let mut h = Harness::new(sample());
    let s = sess("local", "editor", 2, true, 200); // editor: win 1 active, win 2 not
    let target = RowRef::Window { sess: s, window: 2 };
    let items = modal::menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 5),
        items,
        hovered: Some(at),
    }));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::FocusTerminal
    ));
    assert!(
        matches!(h.sw.current_ref(), Some(RowRef::Window { window, .. }) if *window == 2),
        "selection is on the focused window"
    );
    assert!(
        !h.sw.select_active_window(&mut h.state),
        "window 2 is now active → no yank back to window 1"
    );
}

#[tokio::test]
async fn menu_new_session_opens_input_and_creates() {
    // Regression: 'new session' via the host menu must open the name input and,
    // on confirm, create the session — the full gesture-to-op path.
    let mut h = Harness::new(sample());
    let idx = row_index(
        &h,
        |r| matches!(r, RowRef::Host { source, .. } if source == "local"),
    );
    let (x, y) = row_screen_pos(&h, idx);
    assert!(
        h.sw.menu_open(x, y, &mut h.state),
        "menu opens on the host row"
    );
    // Deliberately move onto the first item (no pre-hover), then release.
    let rect = match &h.state.modal {
        Some(Modal::Menu(m)) => m.rect,
        _ => unreachable!(),
    };
    h.sw.menu_hover(rect.x + 1, rect.y + 1, &mut h.state);
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::Handled
    ));
    assert!(h.state.is_inputting(), "new session opens the name input");
    h.sw.set_input_text("fresh", &mut h.state);
    h.key(KeyCode::Enter).await; // queues + pumps the create op (as the loop would)
    assert!(
        h.ops
            .created
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("fresh")),
        "the session is created: {:?}",
        h.ops.created.lock().unwrap()
    );
}

#[tokio::test]
async fn menu_release_window_focus_selects_window() {
    let mut h = Harness::new(sample());
    let s = sess("local", "editor", 2, true, 200);
    let target = RowRef::Window { sess: s, window: 2 };
    let items = modal::menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(at),
    }));
    // A window row offers focus / rename / kill — no split.
    assert!(!items_have_split(&modal::menu_items(&RowRef::Window {
        sess: sess("local", "editor", 2, true, 200),
        window: 2
    })));
    assert!(matches!(
        h.sw.menu_release(&mut h.state),
        MenuOutcome::FocusTerminal
    ));
}

fn items_have_split(items: &[MenuItem]) -> bool {
    // The menu has no Split action; this test guards against one being added.
    items.iter().any(|i| i.label().contains("split"))
}

#[tokio::test]
async fn menu_open_on_host_row() {
    let mut h = Harness::new(sample());
    let idx = row_index(
        &h,
        |r| matches!(r, RowRef::Host { source, .. } if source == "local"),
    );
    let (x, y) = row_screen_pos(&h, idx);
    assert!(
        h.sw.menu_open(x, y, &mut h.state),
        "menu opens over a host row"
    );
    // A host menu's first item (new session) opens an input.
    let target = RowRef::Host {
        source: "local".into(),
        unreachable: false,
    };
    let items = modal::menu_items(&target);
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 4),
        items,
        hovered: Some(0),
    }));
    assert!(
        matches!(h.sw.menu_release(&mut h.state), MenuOutcome::Handled),
        "new session opens an input"
    );
    assert!(h.state.is_inputting());
}

#[tokio::test]
async fn menu_release_stale_target_cancels() {
    let mut h = Harness::new(sample());
    // A target that does not exist in the tree (rebuilt away during the hold).
    let target = RowRef::Session(sess("local", "ghost", 1, false, 0));
    let items = modal::menu_items(&target);
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: String::new(),
        rect: Rect::new(0, 0, 20, 7),
        items,
        hovered: Some(0),
    }));
    assert!(
        matches!(h.sw.menu_release(&mut h.state), MenuOutcome::None),
        "gone target → no-op"
    );
}

#[tokio::test]
async fn menu_renders_title_and_hovered_item_reversed() {
    use super::MenuItem::*;
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = vec![Focus, Rename, Kill, NewWindow];
    // Box at a known spot; hover the second item (rename).
    h.state.modal = Some(Modal::Menu(Menu {
        target,
        title: "build".into(),
        rect: Rect::new(2, 2, 18, 6),
        items,
        hovered: Some(1),
    }));
    h.draw();
    let out = h.text();
    assert!(
        out.contains("focus") && out.contains("rename") && out.contains("kill"),
        "menu items render:\n{out}"
    );
    assert!(
        out.contains("build"),
        "the menu shows its target's name as the title:\n{out}"
    );

    // The hovered row (rename, at box y+1+1 = 4) is reversed across the box interior.
    let buf = h.buf();
    let reversed = (3..19).any(|x| buf[(x, 4u16)].modifier.contains(Modifier::REVERSED));
    assert!(reversed, "the hovered item renders reversed");
}

#[tokio::test]
async fn help_lists_the_right_click_menu() {
    let mut h = Harness::new(sample());
    h.sw.show_help(&mut h.state);
    h.draw();
    assert!(
        h.text().contains("right-click"),
        "help mentions the right-click menu"
    );
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
    // Modal dismissal (tmux view-mode): the app routes keys to feed_help_key
    // above the tree/terminal split — q closes it; other keys are swallowed (no nav).
    assert!(
        h.sw.feed_help_key(b"q", &mut h.state),
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
    h.key(KeyCode::Home).await; // local host
    let t = h.sw.terminal_view_target();
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    h.key(KeyCode::Right).await; // → editor session
    let t = h.sw.terminal_view_target();
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    h.key(KeyCode::Right).await; // → window 1 (shell) under editor
    let t = h.sw.terminal_view_target();
    assert_eq!(
        (t.source.as_str(), t.target.as_str()),
        ("local", "editor:1")
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
        .draw(|f| sw.render(f, Some(&g), false, TREE_WIDTH, &h.state))
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
    // first launch, before any session is confirmed on screen) renders blank —
    // never the placeholder. The display keeps the last confirmed session until
    // the next is ready (stale-while-revalidate), so a transitional placeholder
    // has no purpose.
    let mut state = crate::state::State::from_sources(vec!["local".into(), "jupiter06".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| sw.render(f, None, true, 0, &state)).unwrap();
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
        .map(|r| r.label.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn j_k_navigate_like_arrows() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    let at_top = cur_row_label(&h);
    h.ch('j').await; // down
    assert_ne!(cur_row_label(&h), at_top, "j moves the selection down");
    h.ch('k').await; // back up
    assert_eq!(cur_row_label(&h), at_top, "k moves the selection up");
}

#[tokio::test]
async fn enter_and_bare_q_are_noops() {
    // Enter is consumed by the app (focus the terminal), not the switcher; bare q does
    // nothing — quit is `prefix q` at the app level. Neither moves the selection or
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
    let mut h = Harness::new(sample()); // editor preselected (local session)
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("a session row yields a target");
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    h.key(KeyCode::Left).await; // ← ascend to the local HOST row
    let t =
        h.sw.current_attach_target(&h.state)
            .expect("host row targets its first session");
    assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
}

#[tokio::test]
async fn current_host_tracks_cursor_source() {
    // The app ensures this host on every move; a host row yields its source
    // even when no session is selected, so the host's tree can be fetched.
    let mut h = Harness::new(sample()); // editor preselected (local)
    assert_eq!(h.sw.current_host().as_deref(), Some("local"));
    h.key(KeyCode::End).await; // jump to the last host row (db-2)
    assert_eq!(h.sw.current_host().as_deref(), Some("db-2"));
}

#[tokio::test]
async fn spinner_renders_right_of_connecting_session() {
    let mut h = Harness::new(sample());
    let mut connecting = std::collections::HashSet::new();
    connecting.insert("jupiter00/inference".to_string());
    h.state.chrome.set_spinner(connecting);
    h.draw();
    let tree = h.tree_text();
    // a braille spinner glyph from the U+2800 block appears on the inference row.
    let line = tree.lines().find(|l| l.contains("inference")).unwrap_or("");
    assert!(
        line.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
        "a braille spinner sits right of a connecting session name:\n{tree}"
    );
}

#[test]
fn long_flash_wraps_in_narrow_hint_bar_instead_of_clipping() {
    // The hint_bar lives in the tree column; a long flash must wrap across lines rather
    // than clip at the column edge (a narrow tree would otherwise hide most of it).
    let mut state = crate::state::State::from_scan(sample());
    state.chrome.flash = "host unreachable — cannot create here".into();
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

#[tokio::test]
async fn flash_clears_on_next_key_restoring_the_hint_bar() {
    // A flash (e.g. "host unreachable — cannot create here") is transient: any key
    // dismisses it so the normal help/status hint_bar returns. Regression: it persisted
    // because only the input-opening actions cleared it, so navigation never did.
    let mut h = Harness::new(sample());
    h.state.chrome.flash = "host unreachable — cannot create here".into();
    h.key(KeyCode::Down).await;
    assert!(
        h.state.chrome.flash.is_empty(),
        "navigation clears the flash, got {:?}",
        h.state.chrome.flash
    );
}

#[tokio::test]
async fn hint_bar_and_help_reflect_new_model() {
    let mut h = Harness::new(sample());
    let hint_bar = h.hint_bar_text();
    assert!(
        !hint_bar.to_lowercase().contains("enter attach"),
        "Enter is a no-op now:\n{hint_bar}"
    );
    assert!(
        hint_bar.contains("focus"),
        "hint_bar mentions focusing the terminal view:\n{hint_bar}"
    );
    h.sw.show_help(&mut h.state); // driven by the app's `prefix ?`
    h.draw();
    let help = h.text();
    assert!(
        help.contains("focus the terminal"),
        "help explains focusing the terminal view:\n{help}"
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
    // Colours set from config (pane-*-border-style) drive the view border: active on the
    // focused half, inactive on the other, hover overrides both while hovered.
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    state.chrome.set_view_border_colors(ViewBorderColors {
        active: Color::Blue,
        inactive: Color::Gray,
        hover: Color::Red,
    });
    let x = TREE_WIDTH;
    let (top, bottom) = (2u16, 27u16);
    let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

    // Tree focused: top = active(Blue), bottom = inactive(Gray).
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, top),
        Color::Blue,
        "configured active on the focused half"
    );
    assert_eq!(
        fg(&buf, bottom),
        Color::Gray,
        "configured inactive on the unfocused half"
    );

    // Hovering the rule overrides with the configured hover colour.
    state.chrome.set_view_border_hovered(true);
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        fg(&buf, top),
        Color::Red,
        "configured hover colour while hovered"
    );
}

#[tokio::test]
async fn view_border_splits_top_bottom_to_mark_focused_side() {
    // The rule splits into halves: the accent (green) half marks WHICH pane has
    // focus — top = tree (left), bottom = terminal (right) — and the other half is dim.
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let x = TREE_WIDTH;
    let (top, bottom) = (2u16, 27u16); // within the top / bottom halves of height 30
    let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

    // Terminal focused: accent on the bottom (terminal side), inactive on top. The inactive
    // half is the tmux default (terminal default = Color::Reset), not a dim grey.
    term.draw(|f| sw.render(f, None, true, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(buf[(x, top)].symbol(), "│", "view border still drawn");
    assert_eq!(
        fg(&buf, bottom),
        Color::Green,
        "terminal-view focus: bottom half accent"
    );
    assert_eq!(
        fg(&buf, top),
        Color::Reset,
        "terminal-view focus: top half inactive (tmux default)"
    );

    // Tree focused: accent on the top (tree side), inactive on bottom.
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(fg(&buf, top), Color::Green, "tree focus: top half accent");
    assert_eq!(
        fg(&buf, bottom),
        Color::Reset,
        "tree focus: bottom half inactive"
    );
}

#[tokio::test]
async fn view_border_highlights_on_hover() {
    // Hover swaps the rule to the HEAVY vertical (┃) — box-drawing has no bold form,
    // so the thicker glyph IS the weight cue — and recolours it brighter. No fill.
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let x = TREE_WIDTH;
    state.chrome.set_view_border_hovered(true);
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
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
            Color::Yellow,
            "hover: yellow (psmux hover) at row {y}"
        );
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "hover: not reversed/filled (no block) at row {y}",
        );
    }
}

#[tokio::test]
async fn view_border_glyph_reflects_auto_hide_mode() {
    // ║ (double) when auto-hide-tree mode is on, │ (single) when off — so a visible
    // tree that will vanish on blur is distinguishable from a pinned one.
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    let (x, y) = (TREE_WIDTH, 2u16);

    state.chrome.set_auto_hide(false);
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
        .unwrap();
    assert_eq!(
        term.backend().buffer()[(x, y)].symbol(),
        "│",
        "mode off → single line"
    );

    state.chrome.set_auto_hide(true);
    term.draw(|f| sw.render(f, None, false, TREE_WIDTH, &state))
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
    // opaque — this locks it in across help / input / confirm).
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
                if buf[(x, y)].symbol() == "┌" {
                    tl = Some((x, y));
                    break 'o;
                }
            }
        }
        let Some((x0, y0)) = tl else {
            return usize::MAX;
        };
        let mut w = 0;
        while x0 + w < buf.area.width - 1 && buf[(x0 + w, y0)].symbol() != "┐" {
            w += 1;
        }
        let mut hgt = 0;
        while y0 + hgt < buf.area.height - 1 && buf[(x0, y0 + hgt)].symbol() != "└" {
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
        .draw(|f| h.sw.render(f, Some(&g), true, 0, &h.state))
        .unwrap();
    assert_eq!(
        interior_blue(h.buf()),
        0,
        "help popup interior must be opaque"
    );

    let mut h = Harness::new(sample());
    h.ch('/').await;
    let g = blue_grid();
    h.term
        .draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH, &h.state))
        .unwrap();
    assert_eq!(
        interior_blue(h.buf()),
        0,
        "input popup interior must be opaque"
    );

    let mut h = Harness::new(sample());
    let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    h.sw.set_selected(build, &h.state);
    h.sw.user_moved = true;
    h.sw.arm_kill(&mut h.state);
    let g = blue_grid();
    h.term
        .draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH, &h.state))
        .unwrap();
    assert_eq!(
        interior_blue(h.buf()),
        0,
        "confirm popup interior must be opaque"
    );
}

#[test]
fn popup_border_press_then_drag_moves_the_rect() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.open_input(InputMode::Filter, &mut state); // a small popup with room to move both ways
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    let before = sw.popup_geo.rect;
    let (bx, by) = (before.x, before.y); // top-left corner is on the border
    assert!(
        sw.begin_popup_drag(bx, by, &state),
        "press on the border grabs"
    );
    sw.drag_popup(bx + 5, by + 3);
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    assert_eq!(sw.popup_geo.rect.x, before.x + 5, "moved right by 5");
    assert_eq!(sw.popup_geo.rect.y, before.y + 3, "moved down by 3");
    sw.end_popup_drag();
    assert!(!sw.popup_drag_active());
}

#[test]
fn modals_are_mutually_exclusive() {
    // Opening any modal closes the others, so the drawn popup always matches where
    // keystrokes route (the context menu can open input/confirm bypassing handle_key).
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // launch preselects the host row; descend to a killable session
    sw.arm_kill(&mut state);
    assert!(matches!(state.modal, Some(Modal::Kill(_))));
    sw.open_input(InputMode::Rename, &mut state); // as the menu's Rename would
    assert!(state.is_inputting(), "input opened");
    assert!(
        !matches!(state.modal, Some(Modal::Kill(_))),
        "arming an input cancels a pending kill"
    );
    sw.show_help(&mut state);
    assert!(
        matches!(state.modal, Some(Modal::Help))
            && !state.is_inputting()
            && !matches!(state.modal, Some(Modal::Kill(_))),
        "help closes the input"
    );
    sw.arm_kill(&mut state);
    assert!(
        matches!(state.modal, Some(Modal::Kill(_))) && !matches!(state.modal, Some(Modal::Help)),
        "arming a kill closes help"
    );
}

#[test]
fn closed_popup_cannot_be_grabbed_even_with_a_stale_rect() {
    // popup_rect is refreshed only on render; a popup closed by a keystroke leaves a
    // stale rect. A press must NOT grab a popup that is no longer open.
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.open_input(InputMode::Filter, &mut state);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    let r = sw.popup_geo.rect; // border rect is now cached
    sw.close_input(&mut state); // close WITHOUT re-rendering → popup_rect is stale
    assert!(
        !sw.begin_popup_drag(r.x, r.y, &state),
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
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    assert!(
        sw.popup_geo.rect.width <= 10,
        "popup fits the narrow screen"
    );
}

#[test]
fn popup_interior_press_does_not_grab() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    let r = sw.popup_geo.rect;
    assert!(
        !sw.begin_popup_drag(r.x + 2, r.y + 2, &state),
        "interior press does not start a drag"
    );
}

#[test]
fn popup_drag_clamps_within_screen() {
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    sw.show_help(&mut state);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    let r = sw.popup_geo.rect;
    assert!(sw.begin_popup_drag(r.x, r.y, &state));
    sw.drag_popup(r.x.saturating_sub(50), r.y); // yank far left, past the edge
    term.draw(|f| sw.render(f, None, false, 0, &state)).unwrap();
    assert_eq!(sw.popup_geo.rect.x, 0, "clamped to the left screen edge");
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
fn feed_help_key_is_modal_and_closes_on_q_or_esc() {
    // tmux view-mode style: while open, every key is consumed; q/Esc closes, the
    // rest are swallowed; while closed, nothing is consumed (falls through).
    let mut state = crate::state::State::from_scan(sample());
    let mut sw = Switcher::new(&mut state);
    assert!(
        !sw.feed_help_key(b"q", &mut state),
        "closed → not consumed, routes normally"
    );

    sw.toggle_help(&mut state);
    assert!(sw.feed_help_key(b"j", &mut state), "open → consumed");
    assert!(
        matches!(state.modal, Some(Modal::Help)),
        "a non-close key is swallowed but keeps help open"
    );
    assert!(
        sw.feed_help_key(b"\x1b[A", &mut state),
        "an arrow (ESC [) is swallowed, not a close"
    );
    assert!(
        matches!(state.modal, Some(Modal::Help)),
        "arrow keeps help open"
    );

    assert!(sw.feed_help_key(b"q", &mut state), "q → consumed");
    assert!(!matches!(state.modal, Some(Modal::Help)), "q closes help");

    sw.toggle_help(&mut state);
    assert!(sw.feed_help_key(b"\x1b", &mut state), "lone Esc → consumed");
    assert!(!matches!(state.modal, Some(Modal::Help)), "Esc closes help");
}

#[tokio::test]
async fn input_renders_as_a_centered_popup_not_the_bottom_pane() {
    let mut h = Harness::new(sample());
    h.ch('/').await; // open the filter input
    let buf = h.buf();
    let w = buf.area.width;
    let last = buf.area.height - 1;
    // The entry field is not on the bottom row.
    let bottom: String = (0..w).map(|x| buf[(x, last)].symbol()).collect();
    assert!(
        !bottom.contains('❯'),
        "entry must not be on the bottom row anymore:\n{bottom}"
    );
    // It is in a centered bordered box somewhere in the middle rows.
    let whole: String = (0..buf.area.height)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    assert!(whole.contains('❯'), "entry field present in a popup");
    assert!(whole.contains("Esc to cancel"), "popup shows the Esc hint");
}

#[tokio::test]
async fn input_esc_cancels_without_acting() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await;
    h.ch('n').await;
    assert!(h.state.is_inputting(), "input open");
    h.key(KeyCode::Esc).await;
    assert!(!h.state.is_inputting(), "Esc closes the input");
    assert!(
        h.ops.created.lock().unwrap().is_empty(),
        "Esc must not create anything"
    );
}

#[tokio::test]
async fn kill_confirm_is_a_centered_red_popup_not_the_hint_bar() {
    let mut h = Harness::new(sample());
    let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    h.sw.set_selected(build, &h.state);
    h.sw.user_moved = true;
    h.key(KeyCode::Char('x')).await; // arm the confirm
    let buf = h.buf();
    let last = buf.area.height - 1;
    let hint_bar: String = (0..buf.area.width)
        .map(|x| buf[(x, last)].symbol())
        .collect();
    assert!(
        !hint_bar.contains("[y]es"),
        "confirm must not be in the hint_bar:\n{hint_bar}"
    );
    // A red "kill" cell exists in a centered box (not the hint_bar row).
    let red_kill = (0..last)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .any(|(x, y)| buf[(x, y)].symbol() == "k" && buf[(x, y)].fg == Color::Red);
    assert!(
        red_kill,
        "the confirm popup shows red 'kill' text above the hint_bar"
    );
}

#[tokio::test]
async fn kill_on_window_row_targets_the_window() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (window 1)
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &mut h.state,
    ); // arm kill (raw, not pumped)
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(PendingKill::Window { .. }))),
        "kill on window row must set a window PendingKill"
    );
    let target = match &h.state.modal {
        Some(Modal::Kill(PendingKill::Window { target, .. })) => target.clone(),
        _ => panic!("expected Window variant"),
    };
    assert_eq!(target, "editor:1");
    // confirm with y
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        &mut h.state,
    );
    let op = only_run_op(cmds).expect("kill window queued");
    assert!(
        matches!(op, crate::model::MuxOp::KillWindow { ref target, .. } if target == "editor:1")
    );
}

#[tokio::test]
async fn armed_window_kill_survives_a_same_tree_rebuild() {
    // A routine rebuild (streamed panes with the SAME windows) must NOT cancel an
    // armed window kill — there is no time limit. A later 'y' still kills the right
    // window. (Only a rebuild that actually removes the target invalidates it; the
    // session case is covered by kill_confirm_survives_a_rebuild_until_the_target_vanishes.)
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (window 1, name "shell")
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &mut h.state,
    ); // arm kill (raw)
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(PendingKill::Window { .. }))),
        "arm_kill must set a window PendingKill"
    );
    // Stream the same panes (a rebuild) — the target window still exists.
    let s = sample();
    let editor_panes = s.panes["local/editor"].clone();
    h.sw.apply_panes("local/editor".to_string(), editor_panes, &mut h.state);
    assert!(
        matches!(h.state.modal, Some(Modal::Kill(_))),
        "the confirm survives a same-tree rebuild — no time limit"
    );
    // 'y' now confirms and queues the kill of the armed window.
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        &mut h.state,
    );
    let op = only_run_op(cmds).expect("kill queued after surviving rebuild");
    assert!(
        matches!(op, crate::model::MuxOp::KillWindow { ref target, .. } if target == "editor:1")
    );
}

#[tokio::test]
async fn rename_on_window_row_targets_the_window() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (window 1, name "shell")
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE),
        &mut h.state,
    ); // open rename (raw)
       // input should be open for window rename
    assert!(
        h.state.is_inputting(),
        "rename on window row must open input"
    );
    assert!(
        matches!(&h.state.modal, Some(Modal::Input(i)) if i.target.is_some()),
        "rename on window must have a target"
    );
    h.sw.set_input_text("newname", &mut h.state);
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    ); // confirm (raw)
    let op = only_run_op(cmds).expect("rename window queued");
    assert!(
        matches!(op, crate::model::MuxOp::RenameWindow { ref target, ref new_name, .. }
            if target == "editor:1" && new_name == "newname")
    );
}

#[tokio::test]
async fn rename_window_unchanged_name_is_ignored() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (window 1, name "shell")
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE),
        &mut h.state,
    ); // open rename (raw)
    assert!(
        h.state.is_inputting(),
        "rename on window row must open input"
    );
    h.sw.set_input_text("shell", &mut h.state); // same as current name
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(
        only_run_op(cmds).is_none(),
        "unchanged window name must not queue a RenameWindow op"
    );
}

#[tokio::test]
async fn rename_window_rejects_leading_dash() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host
    h.key(KeyCode::Right).await; // → editor (session)
    h.key(KeyCode::Right).await; // → editor's first window (window 1)
    h.sw.handle_key(
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE),
        &mut h.state,
    ); // open rename (raw)
    assert!(
        h.state.is_inputting(),
        "rename on window row must open input"
    );
    h.sw.set_input_text("-bad", &mut h.state);
    let cmds = h.sw.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut h.state,
    );
    assert!(
        only_run_op(cmds).is_none(),
        "leading-dash window rename must be refused"
    );
    assert!(
        h.state.chrome.flash.contains("cannot start with"),
        "leading-dash window rename must set a flash message, got {:?}",
        h.state.chrome.flash
    );
}

#[tokio::test]
async fn kill_on_host_row_flashes_error() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host row
    h.ch('x').await;
    assert!(
        h.state.chrome.flash.to_lowercase().contains("cannot kill"),
        "kill on host row must flash an error, got {:?}",
        h.state.chrome.flash
    );
    assert!(
        !matches!(h.state.modal, Some(Modal::Kill(_))),
        "no kill queued"
    );
}

#[tokio::test]
async fn rename_on_host_row_flashes_error() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await; // local host row
    h.ch('R').await;
    assert!(
        h.state
            .chrome
            .flash
            .to_lowercase()
            .contains("cannot rename"),
        "rename on host row must flash an error, got {:?}",
        h.state.chrome.flash
    );
    assert!(!h.state.is_inputting(), "no input opened");
}

#[test]
fn removed_window_selection_falls_to_previous_sibling_then_parent() {
    // two windows under jup/api; selection on window 1. Remove window 1 → selection to
    // window 0 (previous sibling). Remove window 0 (now the only/topmost) → selection
    // to the session row (parent).
    let mut state = crate::state::State::from_scan(two_window_scan());
    let mut sw = Switcher::new(&mut state);
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session): launch preselects the host row
    sw.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → window 0
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state); // ↓ window 1
    assert!(matches!(
        sw.current_ref(),
        Some(RowRef::Window { window: 1, .. })
    ));
    // simulate window 1 removed: re-apply panes without window 1.
    sw.apply_panes(
        "jup/api".into(),
        vec![win(0, "w0", true, vec![pane(0, true, "bash")])],
        &mut state,
    );
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })),
        "removed window → previous sibling"
    );
    // remove window 0 too (session now has no window rows): selection to the session.
    sw.apply_panes("jup/api".into(), vec![], &mut state);
    assert!(
        matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "api"),
        "topmost removed → parent (session row)"
    );
}

#[test]
fn render_tree_width_zero_gives_terminal_full_width() {
    use crate::display::grid::Grid;
    // A two-source skeleton is enough. With tree_width == 0 the tree column and
    // its view border are gone, so the terminal view owns the left edge (x=0): the
    // live grid's content begins at column 0.
    let mut state = crate::state::State::from_sources(vec!["local".into(), "jupiter06".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut g = Grid::new(10, 40);
    g.feed(b"EDGE-CONTENT");

    // tree_width == 0 → no tree column, no view border: the terminal view starts at x=0.
    term.draw(|f| sw.render(f, Some(&g), true, 0, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    // Column 0 row 0 must NOT be the view border rule '│' (the view border is gone).
    assert_ne!(
        buf[(0, 0)].symbol(),
        "│",
        "view border must be absent when tree hidden"
    );
    // The live grid content begins at x=0, proving the terminal view owns the left edge.
    let row0: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(
        row0.starts_with("EDGE-CONTENT"),
        "terminal view fills row 0 from x=0: {row0:?}"
    );

    // Sanity: with a normal width the view border rule IS present at the tree edge.
    term.draw(|f| sw.render(f, Some(&g), true, 20, &state))
        .unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        buf[(20, 0)].symbol(),
        "│",
        "view border present at x=tree_width when shown"
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
    let (_title, lines) = modal::help_lines(&state.chrome.ui_prefix);
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
    let (_title, lines_default) = modal::help_lines(&state_default.chrome.ui_prefix);
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
                    windows: 1,
                    attached: false,
                    last_attached: 200,
                },
                Session {
                    source: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                },
            ],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    // Selection starts on the most-recent session row (api). Jump to db by address.
    assert!(sw.select_address("jup/db", &state), "moved to jup/db");
    assert_eq!(sw.terminal_view_target().target, "db");
    // Already-there → no move; unknown address → no move, selection unchanged.
    assert!(!sw.select_address("jup/db", &state), "already on jup/db");
    assert!(
        !sw.select_address("jup/ghost", &state),
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
