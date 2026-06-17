//! The interactive session switcher: a two-pane navigator (a unified
//! Host·Session·Window·Pane tree on the left, a live preview on the right) with a
//! hidden input row and a footer. ratatui is immediate-mode, so this owns its
//! state machine, a flattened row model, key/mouse handling, and a render pass
//! that draws to either the live terminal or a headless `TestBackend` (the
//! control channel's `dump`).

use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};
use ratatui::Frame;

use crate::session::{Pane, Session, WindowPanes};
use crate::ui::ansi::ansi_to_text;
use crate::ui::tree::{self, Group};

/// Tree pane width: border + 1-cell inner padding each side + content.
pub const TREE_WIDTH: u16 = 48;

// Per-level node colours, so the four tree levels read apart at a glance.
const COLOR_HOST: Color = Color::Yellow;
const COLOR_SESSION: Color = Color::Green;
const COLOR_WINDOW: Color = Color::Magenta;
const COLOR_PANE: Color = Color::Cyan;
/// Transient per-element state hints (scanning…, loading…, (empty), unreachable)
/// render dim so pending state reads apart from settled content.
const COLOR_HINT: Color = Color::DarkGray;

/// Shown in the preview pane when the focused target's active pane is itself
/// running xmux. Capturing it would draw the switcher inside its own preview (an
/// infinite overlay); the note stands in for that suppressed capture.
const PREVIEW_SELF_NOTE: &str = "  This pane is running xmux.\n\n  Preview hidden here so the switcher is not\n  drawn inside its own preview.";

/// A fully-populated snapshot of the reachable environment.
#[derive(Clone, Default)]
pub struct Scan {
    pub groups: Vec<Group>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
}

/// The side-effecting actions the switcher delegates to the host program. The
/// event loop also drives the streaming probes through it: [`Ops::sources`] seeds
/// the host skeletons, then [`Ops::list_sessions`] (one per source) and
/// [`Ops::panes`] (one per session) feed the tree incrementally.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved source aliases in display order — synchronous, no probing —
    /// so the UI can paint host skeletons before any probe runs.
    fn sources(&self) -> Vec<String>;
    /// Probes one source's sessions. `Ok` (possibly empty) ⇒ reachable; `Err` ⇒
    /// unreachable (the message is shown as the host's failure reason).
    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session>;
    async fn kill(&self, s: &Session) -> anyhow::Result<()>;
    async fn rename(&self, s: &Session, new_name: &str) -> anyhow::Result<()>;
    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>>;
    async fn capture(&self, source: &str, target: &str) -> anyhow::Result<String>;
}

/// The outcome of one switcher run. `window >= 0` means attach with that window
/// selected; `-1` means the session's current window.
#[derive(Debug, Clone)]
pub struct SwitchResult {
    pub chosen: Option<Session>,
    pub window: i64,
}

impl Default for SwitchResult {
    fn default() -> Self {
        SwitchResult {
            chosen: None,
            window: -1,
        }
    }
}

/// What a tree row references. Hosts, sessions, and windows are selectable; panes
/// and loading placeholders are shown for context but never selectable, so the
/// cursor skips them.
#[derive(Clone)]
enum RowRef {
    Host { source: String, unreachable: bool },
    Session(Session),
    Window { sess: Session, window: i64 },
    Pane,
    /// A "panes loading…" placeholder under a session whose detail is in flight.
    Loading,
}

struct Row {
    label: String,
    /// A trailing dim annotation (scanning…, (empty), ⚠ unreachable: …) — kept
    /// apart from `label` so the name stays in its level colour and the state
    /// reads dim.
    hint: Option<String>,
    indent: usize,
    color: Color,
    reference: RowRef,
}

impl Row {
    fn selectable(&self) -> bool {
        !matches!(self.reference, RowRef::Pane | RowRef::Loading)
    }
}

/// The preview target whose active pane attaching here would land on.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PreviewTarget {
    pub source: String,
    pub target: String, // empty ⇒ no preview
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Filter,
    New,
    Rename,
}

struct Input {
    mode: InputMode,
    label: String,
    buffer: String,
}

/// The switcher state machine.
pub struct Switcher {
    groups: Vec<Group>,
    panes: HashMap<String, Vec<WindowPanes>>,
    /// Sources whose `list-sessions` has not yet returned (host shows scanning…).
    scanning: HashSet<String>,
    /// Session addresses whose `list-panes` has resolved (success or failure) —
    /// until then the session shows a loading… placeholder.
    panes_loaded: HashSet<String>,
    /// Set once the user explicitly moves the cursor; while false, streaming
    /// results advance the preselect toward the most-recent session.
    user_moved: bool,
    /// Signals the event loop to (re)kick the streaming probes — set on the
    /// initial seed and on an `r` re-scan; the loop reads + clears it.
    rescan_kick: bool,

    rows: Vec<Row>,
    selected: usize,
    name_col_width: usize,

    filter: String,
    input: Option<Input>,
    pending_kill: Option<Session>,
    flash: String,

    preview_target: PreviewTarget,
    preview_cache: HashMap<String, String>,
    preview_text: String,
    preview_title: String,
    dialog: Option<String>,
    /// Set when the preview target changes — the run loop's poller reads + clears
    /// it to refresh immediately rather than waiting for the next tick.
    poll_kick: bool,
    /// True when the focused target's active pane is itself running xmux, so a
    /// capture would mirror this UI recursively. Set in `on_focus_changed`; gates
    /// the capture ([`Switcher::preview_capturable`]) and swaps in a note.
    preview_self: bool,

    list_state: ListState,
    tree_inner: Rect,

    show_help: bool,
    result: SwitchResult,
    exit: bool,
}

impl Switcher {
    fn blank() -> Self {
        Switcher {
            groups: Vec::new(),
            panes: HashMap::new(),
            scanning: HashSet::new(),
            panes_loaded: HashSet::new(),
            user_moved: false,
            rescan_kick: false,
            rows: Vec::new(),
            selected: 0,
            name_col_width: 0,
            filter: String::new(),
            input: None,
            pending_kill: None,
            flash: String::new(),
            preview_target: PreviewTarget::default(),
            preview_cache: HashMap::new(),
            preview_text: String::new(),
            preview_title: " Preview ".into(),
            dialog: None,
            poll_kick: false,
            preview_self: false,
            list_state: ListState::default(),
            tree_inner: Rect::default(),
            show_help: false,
            result: SwitchResult::default(),
            exit: false,
        }
    }

    /// Builds from a complete snapshot: every host is resolved (reachable or
    /// unreachable per its `err`) and every session's panes are considered known.
    pub fn new(scan: Scan) -> Self {
        let mut s = Switcher::blank();
        s.groups = scan.groups;
        s.panes = scan.panes;
        s.panes_loaded = s
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|sess| sess.address()))
            .collect();
        s.rebuild();
        s
    }

    /// Seeds the switcher from the resolved source list alone — no probing — so
    /// the first frame paints host-skeleton rows, each in a scanning state, in
    /// tens of milliseconds. Streamed [`apply_source_result`]/[`apply_panes`]
    /// calls fill the tree in afterward.
    pub fn from_sources(aliases: Vec<String>) -> Self {
        let mut s = Switcher::blank();
        s.scanning = aliases.iter().cloned().collect();
        s.groups = aliases
            .into_iter()
            .map(|source| Group {
                source,
                err: None,
                sessions: Vec::new(),
            })
            .collect();
        s.rescan_kick = true; // the event loop kicks the probes on the first frame
        s.rebuild();
        s
    }

    pub fn result(&self) -> SwitchResult {
        self.result.clone()
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn preview_target(&self) -> PreviewTarget {
        self.preview_target.clone()
    }

    /// Takes the pending poll-kick flag (true once after the target changed).
    pub fn take_poll_kick(&mut self) -> bool {
        std::mem::take(&mut self.poll_kick)
    }

    /// Takes the pending rescan-kick flag (true once after seeding or an `r`
    /// re-scan) — the event loop spawns the streaming probes when it is set.
    pub fn take_rescan_kick(&mut self) -> bool {
        std::mem::take(&mut self.rescan_kick)
    }

    /// Whether the current preview target should be captured. False when there is
    /// no target, or its active pane is running xmux (a self-overlay whose capture
    /// would mirror this UI recursively).
    pub fn preview_capturable(&self) -> bool {
        !self.preview_target.target.is_empty() && !self.preview_self
    }

    // --- tree model ---------------------------------------------------------

    fn visible_groups(&self) -> Vec<Group> {
        if self.filter.is_empty() {
            return self.groups.clone();
        }
        let filtered = tree::filter_groups(&self.groups, &self.filter);
        if filtered.is_empty() {
            // XM-01: a non-matching filter must not be a dead end.
            return self
                .groups
                .iter()
                .map(|g| Group {
                    source: g.source.clone(),
                    err: g.err.clone(),
                    sessions: Vec::new(),
                })
                .collect();
        }
        filtered
    }

    fn rebuild(&mut self) {
        let groups = self.visible_groups();

        self.name_col_width = 0;
        for g in &groups {
            if g.err.is_some() {
                continue;
            }
            for sess in &g.sessions {
                self.name_col_width = self.name_col_width.max(sess.name.chars().count());
            }
        }

        let mut rows = Vec::new();
        let mut preselect: Option<usize> = None;
        let mut best_recency = i64::MIN;

        for g in &groups {
            let scanning = self.scanning.contains(&g.source);
            let unreachable = g.err.is_some();
            rows.push(Row {
                label: g.source.clone(),
                hint: self.host_hint(g, scanning),
                indent: 0,
                color: COLOR_HOST,
                reference: RowRef::Host {
                    source: g.source.clone(),
                    unreachable,
                },
            });
            if scanning || unreachable {
                continue;
            }
            for sess in &g.sessions {
                if sess.last_attached > best_recency {
                    best_recency = sess.last_attached;
                    preselect = Some(rows.len());
                }
                rows.push(Row {
                    label: self.session_label(sess),
                    hint: None,
                    indent: 2,
                    color: COLOR_SESSION,
                    reference: RowRef::Session(sess.clone()),
                });
                if self.panes_loaded.contains(&sess.address()) {
                    if let Some(windows) = self.panes.get(&sess.address()) {
                        for w in windows {
                            rows.push(Row {
                                label: window_label(w),
                                hint: None,
                                indent: 4,
                                color: COLOR_WINDOW,
                                reference: RowRef::Window {
                                    sess: sess.clone(),
                                    window: w.index,
                                },
                            });
                            for p in &w.panes {
                                rows.push(Row {
                                    label: pane_label(p),
                                    hint: None,
                                    indent: 6,
                                    color: COLOR_PANE,
                                    reference: RowRef::Pane,
                                });
                            }
                        }
                    }
                } else {
                    // Panes still in flight for this session — a dim placeholder
                    // stands where its windows will appear.
                    rows.push(Row {
                        label: "loading…".into(),
                        hint: None,
                        indent: 4,
                        color: COLOR_HINT,
                        reference: RowRef::Loading,
                    });
                }
            }
        }

        self.rows = rows;
        let target = preselect
            .or_else(|| self.rows.iter().position(Row::selectable))
            .unwrap_or(0);
        self.set_selected(target);
    }

    /// The dim trailing annotation for a host row: its scan state when it has no
    /// sessions to show — scanning…, ⚠ unreachable: <reason>, or (empty).
    fn host_hint(&self, g: &Group, scanning: bool) -> Option<String> {
        if scanning {
            Some("scanning…".into())
        } else if let Some(err) = &g.err {
            Some(format!("⚠ unreachable: {err}"))
        } else if g.sessions.is_empty() {
            Some("(empty)".into())
        } else {
            None
        }
    }

    fn session_label(&self, sess: &Session) -> String {
        let pad = self
            .name_col_width
            .saturating_sub(sess.name.chars().count());
        let star = if sess.attached { "  ●" } else { "" };
        format!(
            "{}{}   {}{}",
            sess.name,
            " ".repeat(pad),
            plural(sess.windows),
            star
        )
    }

    // --- selection / navigation --------------------------------------------

    fn selectable_indices(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect()
    }

    fn set_selected(&mut self, idx: usize) {
        if self.rows.is_empty() {
            return;
        }
        let idx = idx.min(self.rows.len() - 1);
        self.selected = idx;
        self.list_state.select(Some(idx));
        self.on_focus_changed();
    }

    fn move_selection(&mut self, delta: isize) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.user_moved = true;
        let cur = sel.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        let n = sel.len() as isize;
        let next = ((cur + delta) % n + n) % n;
        self.set_selected(sel[next as usize]);
    }

    fn move_to(&mut self, pos: isize) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.user_moved = true;
        let idx = if pos < 0 || pos as usize >= sel.len() {
            sel.len() - 1
        } else {
            pos as usize
        };
        self.set_selected(sel[idx]);
    }

    fn current_ref(&self) -> Option<&RowRef> {
        self.rows.get(self.selected).map(|r| &r.reference)
    }

    fn current_source(&self) -> Option<String> {
        match self.current_ref()? {
            RowRef::Host { source, .. } => Some(source.clone()),
            RowRef::Session(s) => Some(s.source.clone()),
            RowRef::Window { sess, .. } => Some(sess.source.clone()),
            RowRef::Pane | RowRef::Loading => None,
        }
    }

    fn current_session(&self) -> Option<Session> {
        match self.current_ref()? {
            RowRef::Session(s) => Some(s.clone()),
            RowRef::Window { sess, .. } => Some(sess.clone()),
            _ => None,
        }
    }

    fn current_host_unreachable(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { unreachable, .. }) if *unreachable)
    }

    // --- preview ------------------------------------------------------------

    fn first_session_of(&self, source: &str) -> Option<Session> {
        self.groups
            .iter()
            .find(|g| g.source == source && !g.sessions.is_empty())
            .map(|g| g.sessions[0].clone())
    }

    fn target_for(&self, reference: &RowRef) -> PreviewTarget {
        match reference {
            RowRef::Host { source, .. } => match self.first_session_of(source) {
                Some(sess) => PreviewTarget {
                    source: sess.source,
                    target: sess.name,
                },
                None => PreviewTarget::default(),
            },
            RowRef::Session(s) => PreviewTarget {
                source: s.source.clone(),
                target: s.name.clone(),
            },
            RowRef::Window { sess, window } => PreviewTarget {
                source: sess.source.clone(),
                target: format!("{}:{}", sess.name, window),
            },
            RowRef::Pane | RowRef::Loading => PreviewTarget::default(),
        }
    }

    /// The current command of the active pane the focused target would capture,
    /// from the already-fetched pane map. The active pane of a session target is
    /// the active pane of its active window; of a window target, the active pane
    /// of that window. `None` when the panes are not known.
    fn focused_pane_command(&self, reference: &RowRef) -> Option<&str> {
        let (address, window) = match reference {
            RowRef::Session(s) => (s.address(), None),
            RowRef::Window { sess, window } => (sess.address(), Some(*window)),
            RowRef::Host { source, .. } => (self.first_session_of(source)?.address(), None),
            RowRef::Pane | RowRef::Loading => return None,
        };
        let windows = self.panes.get(&address)?;
        let win = match window {
            Some(idx) => windows.iter().find(|w| w.index == idx)?,
            None => windows.iter().find(|w| w.active).or_else(|| windows.first())?,
        };
        let pane = win
            .panes
            .iter()
            .find(|p| p.active)
            .or_else(|| win.panes.first())?;
        Some(&pane.command)
    }

    /// Whether the focused target's active pane is running xmux itself.
    fn focused_runs_xmux(&self) -> bool {
        self.current_ref()
            .and_then(|r| self.focused_pane_command(r))
            .is_some_and(command_is_xmux)
    }

    fn on_focus_changed(&mut self) {
        let tgt = match self.current_ref() {
            Some(r) => self.target_for(r),
            None => PreviewTarget::default(),
        };
        if tgt == self.preview_target {
            return;
        }
        self.preview_target = tgt.clone();
        if tgt.target.is_empty() {
            self.preview_title = " Preview ".into();
            self.dialog = None;
            self.preview_text.clear();
            self.preview_self = false;
            return;
        }
        self.preview_title = format!(" Preview · {} ", tgt.target);
        // A pane already running xmux would capture this very switcher — show a
        // note instead of recursing, and skip the capture (preview_capturable).
        if self.focused_runs_xmux() {
            self.preview_self = true;
            self.dialog = None;
            self.preview_text = PREVIEW_SELF_NOTE.to_string();
            return;
        }
        self.preview_self = false;
        let key = preview_key(&tgt);
        if let Some(cached) = self.preview_cache.get(&key) {
            // revisit: keep the cached render visible and float a reconnecting
            // dialog over it; the poller refreshes and dismisses the dialog.
            self.preview_text = cached.clone();
            self.dialog = Some("⟳ reconnecting…".into());
        } else {
            // first visit: a loading dialog until the first capture lands.
            self.preview_text.clear();
            self.dialog = Some("⟳ loading preview…".into());
        }
        self.poll_kick = true;
    }

    /// Applies a freshly captured preview for `target` (called by the poller). If
    /// the cursor is still on `target`, dismisses the dialog and shows it.
    pub fn apply_capture(&mut self, target: &PreviewTarget, text: Option<String>) {
        match text {
            Some(text) => {
                self.preview_cache.insert(preview_key(target), text.clone());
                if *target == self.preview_target {
                    self.dialog = None;
                    self.preview_text = text;
                }
            }
            None => {
                if *target == self.preview_target {
                    self.dialog = Some("preview unavailable".into());
                }
            }
        }
    }

    // --- key handling -------------------------------------------------------

    pub async fn handle_key(&mut self, ev: KeyEvent, ops: &dyn Ops) {
        if self.input.is_some() {
            self.handle_input_key(ev, ops).await;
            return;
        }
        if self.show_help {
            // The help overlay is modal: any key dismisses it.
            self.show_help = false;
            return;
        }
        if self.pending_kill.is_some() {
            self.resolve_kill(ev, ops).await;
            return;
        }
        match ev.code {
            KeyCode::Esc => self.quit(),
            KeyCode::Enter => self.on_enter(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(-1),
            KeyCode::Char(c) => match c {
                'q' => self.quit(),
                '/' => self.open_input(InputMode::Filter),
                'n' => self.open_input(InputMode::New),
                'R' => self.open_input(InputMode::Rename),
                'x' => self.arm_kill(),
                'r' => self.request_rescan(),
                '?' => self.show_help = true,
                _ => {}
            },
            _ => {}
        }
    }

    fn on_enter(&mut self) {
        let Some(reference) = self.current_ref().cloned() else {
            return;
        };
        match reference {
            RowRef::Host {
                source,
                unreachable,
            } => {
                if unreachable {
                    return;
                }
                if let Some(sess) = self.first_session_of(&source) {
                    self.choose(sess, -1);
                }
            }
            RowRef::Session(s) => self.choose(s, -1),
            RowRef::Window { sess, window } => self.choose(sess, window),
            RowRef::Pane | RowRef::Loading => {}
        }
    }

    fn choose(&mut self, sess: Session, window: i64) {
        self.result = SwitchResult {
            chosen: Some(sess),
            window,
        };
        self.exit = true;
    }

    fn quit(&mut self) {
        self.result = SwitchResult {
            chosen: None,
            window: -1,
        };
        self.exit = true;
    }

    // --- input row ----------------------------------------------------------

    fn open_input(&mut self, mode: InputMode) {
        self.flash.clear();
        match mode {
            InputMode::Filter => {
                self.input = Some(Input {
                    mode,
                    label: " filter: ".into(),
                    buffer: self.filter.clone(),
                });
            }
            InputMode::New => {
                if self.current_source().is_none() {
                    return;
                }
                if self.current_host_unreachable() {
                    self.flash = "host unreachable — cannot create here".into();
                    return;
                }
                self.input = Some(Input {
                    mode,
                    label: " new session name (empty = auto): ".into(),
                    buffer: String::new(),
                });
            }
            InputMode::Rename => {
                let Some(sess) = self.current_session() else {
                    return;
                };
                self.input = Some(Input {
                    mode,
                    label: " rename to: ".into(),
                    buffer: sess.name,
                });
            }
        }
    }

    fn close_input(&mut self) {
        self.input = None;
    }

    async fn handle_input_key(&mut self, ev: KeyEvent, ops: &dyn Ops) {
        match ev.code {
            KeyCode::Enter => {
                let (mode, val) = {
                    let input = self.input.as_ref().expect("input active");
                    (input.mode, input.buffer.trim().to_string())
                };
                match mode {
                    InputMode::Filter => {
                        self.filter = val;
                        self.close_input();
                        self.rebuild();
                    }
                    InputMode::New => {
                        self.do_create(&val, ops).await;
                        self.close_input();
                    }
                    InputMode::Rename => {
                        self.do_rename(&val, ops).await;
                        self.close_input();
                    }
                }
            }
            KeyCode::Esc => self.close_input(),
            KeyCode::Backspace => {
                if let Some(input) = self.input.as_mut() {
                    input.buffer.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(input) = self.input.as_mut() {
                    input.buffer.push(c);
                }
            }
            _ => {}
        }
    }

    /// Test/host hook: set the active input buffer directly.
    pub fn set_input_text(&mut self, text: &str) {
        if let Some(input) = self.input.as_mut() {
            input.buffer = text.to_string();
        }
    }

    async fn do_create(&mut self, name: &str, ops: &dyn Ops) {
        let Some(src) = self.current_source() else {
            return;
        };
        let created = match ops.new_session(&src, name).await {
            Ok(s) => s,
            Err(e) => {
                self.flash = format!("create failed: {e}");
                return;
            }
        };
        if let Ok(wins) = ops.panes(&created).await {
            self.panes.insert(created.address(), wins);
        }
        let addr = created.address();
        self.panes_loaded.insert(addr.clone());
        self.groups = tree::add_session(&self.groups, created);
        self.rebuild();
        if let Some(i) = self.row_of_session(&addr) {
            self.user_moved = true;
            self.set_selected(i);
        }
    }

    async fn do_rename(&mut self, new_name: &str, ops: &dyn Ops) {
        let Some(sess) = self.current_session() else {
            return;
        };
        if new_name.is_empty() || new_name == sess.name {
            return;
        }
        if new_name.starts_with('-') {
            // the mux silently no-ops a '-'-leading name (getopt eats it) — refuse.
            self.flash = "rename: name cannot start with '-'".into();
            return;
        }
        if let Err(e) = ops.rename(&sess, new_name).await {
            self.flash = format!("rename failed: {e}");
            return;
        }
        let new_addr = format!("{}/{}", sess.source, new_name);
        if let Some(wins) = self.panes.remove(&sess.address()) {
            self.panes.insert(new_addr.clone(), wins);
        }
        if self.panes_loaded.remove(&sess.address()) {
            self.panes_loaded.insert(new_addr);
        }
        self.groups = tree::rename_session(&self.groups, &sess.address(), new_name);
        self.rebuild();
    }

    fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == address))
    }

    // --- kill (inline confirm) ----------------------------------------------

    fn arm_kill(&mut self) {
        if let Some(sess) = self.current_session() {
            self.pending_kill = Some(sess);
        }
    }

    async fn resolve_kill(&mut self, ev: KeyEvent, ops: &dyn Ops) {
        let sess = self.pending_kill.take();
        if let Some(sess) = sess {
            if matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                if let Err(e) = ops.kill(&sess).await {
                    self.flash = format!("kill failed: {e}");
                } else {
                    self.panes.remove(&sess.address());
                    self.panes_loaded.remove(&sess.address());
                    self.groups = tree::remove_session(&self.groups, &sess.address());
                    self.rebuild();
                }
            }
        }
    }

    // --- refresh ------------------------------------------------------------

    /// Resets every host to its scanning skeleton and signals the event loop to
    /// re-kick the streaming probes (the `r` re-scan) — sessions and panes stream
    /// back in exactly as on first launch. Keeps the cursor on the focused host
    /// if the user had moved it there.
    pub fn request_rescan(&mut self) {
        let focus = self.current_ref().cloned();
        self.scanning = self.groups.iter().map(|g| g.source.clone()).collect();
        for g in self.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        self.panes.clear();
        self.panes_loaded.clear();
        self.rescan_kick = true;
        self.rebuild();
        self.restore_focus(focus);
    }

    /// Streams in one source's `list-sessions` outcome: clears its scanning
    /// state and replaces that host's sessions (reachable) or records its failure
    /// (unreachable). The host authoritatively owns its session list.
    pub fn apply_source_result(
        &mut self,
        source: String,
        mut sessions: Vec<Session>,
        err: Option<String>,
    ) {
        let focus = self.current_ref().cloned();
        self.scanning.remove(&source);
        tree::sort_by_recency(&mut sessions);
        if let Some(g) = self.groups.iter_mut().find(|g| g.source == source) {
            g.err = err;
            g.sessions = sessions;
        } else {
            self.groups.push(Group {
                source,
                err,
                sessions,
            });
        }
        self.rebuild();
        self.restore_focus(focus);
    }

    /// Streams in one session's `list-panes` outcome, clearing its loading
    /// placeholder. An empty `panes` (a failed/timed-out fetch) still resolves the
    /// session — it shows no children rather than spinning forever.
    pub fn apply_panes(&mut self, address: String, panes: Vec<WindowPanes>) {
        let focus = self.current_ref().cloned();
        self.panes_loaded.insert(address.clone());
        self.panes.insert(address, panes);
        self.rebuild();
        self.restore_focus(focus);
    }

    /// After a streamed update rebuilds the rows: if the user has driven the
    /// cursor, keep it on the focused node when it survives; otherwise let the
    /// rebuild's recency preselect stand, so an untouched cursor follows the
    /// most-recent session as hosts stream in.
    fn restore_focus(&mut self, focus: Option<RowRef>) {
        if self.user_moved {
            if let Some(i) = focus.and_then(|f| self.row_matching(&f)) {
                self.set_selected(i);
            }
        }
    }

    /// The row index targeting the same node as `focus`, if it survives a
    /// rebuild — so a re-scan keeps the cursor in place rather than snapping to
    /// the recency preselect.
    fn row_matching(&self, focus: &RowRef) -> Option<usize> {
        self.rows.iter().position(|r| same_node(&r.reference, focus))
    }

    // --- mouse --------------------------------------------------------------

    fn in_tree(&self, col: u16, row: u16) -> bool {
        self.tree_inner.contains(Position { x: col, y: row })
    }

    /// Single click: move the cursor to the clicked row (select; never attach).
    pub fn mouse_select(&mut self, col: u16, row: u16) {
        if !self.in_tree(col, row) {
            return;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
        if self.rows.get(idx).is_some_and(Row::selectable) {
            self.user_moved = true;
            self.set_selected(idx);
        }
    }

    /// Double click: attach the current node (the preceding single click already
    /// moved the cursor).
    pub fn mouse_attach(&mut self, col: u16, row: u16) {
        if self.in_tree(col, row) {
            self.on_enter();
        }
    }

    /// Scroll wheel: move the cursor (panes skipped) in the given direction.
    pub fn mouse_scroll(&mut self, down: bool) {
        self.move_selection(if down { 1 } else { -1 });
    }

    // --- render -------------------------------------------------------------

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let input_h = if self.input.is_some() { 1 } else { 0 };
        let v = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(area);
        self.render_header(frame, v[0]);
        let mid =
            Layout::horizontal([Constraint::Length(TREE_WIDTH), Constraint::Min(0)]).split(v[1]);
        self.render_tree(frame, mid[0]);
        self.render_preview(frame, mid[1]);
        if input_h == 1 {
            self.render_input(frame, v[2]);
        }
        self.render_footer(frame, v[3]);
        if self.show_help {
            self.render_help(frame, area);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let text = Text::from(vec![
            Line::from(Span::raw("xmux").bold()),
            Line::from(Span::raw("cross-environment MUX manager").dim()),
        ]);
        frame.render_widget(Paragraph::new(text), area);
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.filter.is_empty() {
            " Hosts · Sessions · Windows · Panes ".to_string()
        } else {
            format!(" filter: {} ", self.filter)
        };
        let block = Block::bordered()
            .title(title)
            .padding(Padding::horizontal(1));
        self.tree_inner = block.inner(area);

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let indent = " ".repeat(row.indent);
                let selected = i == self.selected;
                let mut style = Style::default().fg(row.color);
                if selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let mut spans = vec![
                    Span::raw(indent),
                    Span::styled(pad_label(&row.label), style),
                ];
                if let Some(hint) = &row.hint {
                    let mut hint_style = Style::default().fg(COLOR_HINT);
                    if selected {
                        hint_style = hint_style.add_modifier(Modifier::REVERSED);
                    }
                    spans.push(Span::styled(format!("{hint} "), hint_style));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block);
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(self.preview_title.clone());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(ansi_to_text(&self.preview_text)), inner);

        if let Some(msg) = &self.dialog {
            let w = (msg.chars().count() as u16) + 4;
            let h = 3;
            let rect = centered_rect(w, h, inner);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(msg.clone())
                    .centered()
                    .block(Block::bordered()),
                rect,
            );
        }
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        if let Some(input) = &self.input {
            let line = format!("{}{}", input.label, input.buffer);
            frame.render_widget(Paragraph::new(line), area);
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let text = if let Some(sess) = &self.pending_kill {
            format!(" kill {}? [y]es / [n]o", sess.address())
        } else if !self.flash.is_empty() {
            format!(" {}", self.flash)
        } else if !self.scanning.is_empty() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the help line) once every host has settled.
            let total = self.groups.len();
            let done = total.saturating_sub(self.scanning.len());
            format!(" ⟳ scanning hosts {done}/{total}… · q quit · ? help")
        } else {
            " enter attach · n new · R rename · x kill · / filter · r refresh · ? help · q quit"
                .to_string()
        };
        frame.render_widget(Paragraph::new(text), area);
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        const LINES: &[&str] = &[
            "↑ / ↓        move (panes are skipped)",
            "PgUp / PgDn  jump by 10",
            "Home / End   first / last node",
            "Enter        attach (host → recent · session · window)",
            "n            new session on the focused host",
            "R            rename the focused session",
            "x            kill the focused session (y / n confirm)",
            "/            fuzzy filter <source>/<name>",
            "r            re-scan every host",
            "?            toggle this help",
            "q / Esc      quit",
            "",
            "mouse: click selects · double-click attaches · wheel scrolls",
        ];
        let inner_w = LINES.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let w = (inner_w + 4).min(area.width); // text + a space each side + borders
        let h = (LINES.len() as u16 + 2).min(area.height);
        let rect = centered_rect(w, h, area);
        let text = Text::from(
            LINES
                .iter()
                .map(|l| Line::from(format!(" {l}")))
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" keys ")),
            rect,
        );
    }
}

fn plural(n: i64) -> String {
    if n == 1 {
        "1 window".to_string()
    } else {
        format!("{n} windows")
    }
}

fn window_label(w: &WindowPanes) -> String {
    let mut s = format!("window {}: {}", w.index, w.name);
    if w.active {
        s.push_str("  (active)");
    }
    s
}

fn pane_label(p: &Pane) -> String {
    let mut s = format!("pane {}  {}", p.index, p.command);
    if p.active {
        s.push_str("  (active)");
    }
    s
}

/// Adds a space each side so the reverse-video selection has breathing room.
fn pad_label(s: &str) -> String {
    format!(" {s} ")
}

fn preview_key(t: &PreviewTarget) -> String {
    format!("{}\u{0}{}", t.source, t.target)
}

/// Whether two row references target the same selectable node (host by source,
/// session/window by address), used to keep the cursor across a re-scan.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Host { source: x, .. }, RowRef::Host { source: y, .. }) => x == y,
        (RowRef::Session(x), RowRef::Session(y)) => x.address() == y.address(),
        (
            RowRef::Window { sess: x, window: wx },
            RowRef::Window { sess: y, window: wy },
        ) => x.address() == y.address() && wx == wy,
        _ => false,
    }
}

/// Whether `pane_current_command` names the xmux binary — so capturing that pane
/// would mirror this UI. Matches the bare name and the Windows `.exe`.
fn command_is_xmux(command: &str) -> bool {
    let c = command.trim().to_ascii_lowercase();
    c == "xmux" || c == "xmux.exe"
}

fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        async fn capture(&self, _source: &str, _target: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    // --- headless harness ---------------------------------------------------

    struct Harness {
        sw: Switcher,
        term: Terminal<TestBackend>,
        ops: RecordOps,
    }

    impl Harness {
        fn new(scan: Scan) -> Self {
            let backend = TestBackend::new(100, 30);
            let term = Terminal::new(backend).unwrap();
            let mut h = Harness {
                sw: Switcher::new(scan),
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
            let mut h = Harness {
                sw: Switcher::from_sources(aliases),
                term,
                ops: RecordOps::default(),
            };
            h.draw();
            h
        }

        fn footer_text(&self) -> String {
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
            self.term.draw(|f| sw.render(f)).unwrap();
        }

        async fn key(&mut self, code: KeyCode) {
            self.sw
                .handle_key(KeyEvent::new(code, KeyModifiers::NONE), &self.ops)
                .await;
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
            "unreachable",
        ] {
            assert!(out.contains(want), "tree missing {want:?}\n{out}");
        }
    }

    #[tokio::test]
    async fn preselects_most_recent_session() {
        let h = Harness::new(sample());
        assert_eq!(cur_session_name(&h).as_deref(), Some("inference"));
    }

    #[tokio::test]
    async fn panes_are_not_selectable() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;
        let mut saw_window = false;
        for _ in 0..14 {
            let r = h.sw.current_ref();
            assert!(r.is_some(), "cursor landed on a non-selectable node");
            if matches!(r, Some(RowRef::Window { .. })) {
                saw_window = true;
            }
            h.key(KeyCode::Down).await;
        }
        assert!(saw_window, "navigation should reach window nodes");
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
            "each host shows a scanning hint:\n{out}"
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
        );
        h.draw();
        let out = h.tree_text();
        assert!(out.contains("editor"), "session appears after result:\n{out}");
        assert!(
            !out.contains("scanning"),
            "scanning hint clears once the only host resolves:\n{out}"
        );
        assert!(
            out.contains("loading"),
            "the session shows a loading hint until its panes arrive:\n{out}"
        );
    }

    #[tokio::test]
    async fn apply_source_result_empty_shows_empty_hint() {
        let mut h = Harness::from_sources(&["local"]);
        h.sw.apply_source_result("local".into(), vec![], None);
        h.draw();
        let out = h.text();
        assert!(
            out.contains("empty"),
            "a reachable host with no sessions reads (empty):\n{out}"
        );
        assert!(!out.contains("scanning"), "no longer scanning:\n{out}");
    }

    #[tokio::test]
    async fn apply_source_result_unreachable_shows_reason() {
        let mut h = Harness::from_sources(&["prod"]);
        h.sw.apply_source_result(
            "prod".into(),
            vec![],
            Some("connection refused".into()),
        );
        h.draw();
        let out = h.text();
        assert!(out.contains("unreachable"), "shows unreachable state:\n{out}");
        assert!(
            out.contains("connection refused"),
            "shows the failure reason:\n{out}"
        );
    }

    #[tokio::test]
    async fn apply_panes_attaches_and_clears_loading() {
        let mut h = Harness::from_sources(&["local"]);
        h.sw.apply_source_result(
            "local".into(),
            vec![sess("local", "editor", 1, false, 100)],
            None,
        );
        h.draw();
        assert!(
            h.tree_text().contains("loading"),
            "panes loading before they land"
        );
        h.sw.apply_panes(
            "local/editor".into(),
            vec![win(1, "shell", true, vec![pane(1, true, "bash")])],
        );
        h.draw();
        let out = h.tree_text();
        assert!(
            out.contains("window 1: shell"),
            "panes attach under the session:\n{out}"
        );
        assert!(
            !out.contains("loading"),
            "the loading hint clears once panes arrive:\n{out}"
        );
    }

    #[tokio::test]
    async fn streaming_auto_advances_to_recent_when_untouched() {
        // While the user has not moved the cursor, each result advances the
        // preselect toward the globally most-recent session.
        let mut h = Harness::from_sources(&["local", "jupiter00"]);
        h.sw.apply_source_result(
            "local".into(),
            vec![sess("local", "editor", 1, false, 100)],
            None,
        );
        h.draw();
        assert_eq!(cur_session_name(&h).as_deref(), Some("editor"));
        h.sw.apply_source_result(
            "jupiter00".into(),
            vec![sess("jupiter00", "infer", 1, false, 300)],
            None,
        );
        h.draw();
        assert_eq!(
            cur_session_name(&h).as_deref(),
            Some("infer"),
            "an untouched cursor follows the most-recent session as it streams in"
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
        );
        h.draw();
        // editor preselected (most recent local); move down to build.
        h.key(KeyCode::Down).await;
        assert_eq!(cur_session_name(&h).as_deref(), Some("build"));
        // A more-recent remote session streams in; the cursor must NOT jump.
        h.sw.apply_source_result(
            "jupiter00".into(),
            vec![sess("jupiter00", "infer", 1, false, 300)],
            None,
        );
        h.draw();
        assert_eq!(
            cur_session_name(&h).as_deref(),
            Some("build"),
            "once the user has moved, streaming updates keep the cursor put"
        );
    }

    #[tokio::test]
    async fn footer_shows_scanning_progress_then_clears() {
        let mut h = Harness::from_sources(&["local", "jupiter00"]);
        let footer = h.footer_text();
        assert!(
            footer.contains("scanning"),
            "footer shows a global scanning indicator:\n{footer:?}"
        );
        assert!(
            footer.contains("/2"),
            "footer shows the host progress fraction:\n{footer:?}"
        );
        h.sw.apply_source_result("local".into(), vec![], None);
        h.sw.apply_source_result("jupiter00".into(), vec![], None);
        h.draw();
        let footer = h.footer_text();
        assert!(
            !footer.contains("scanning"),
            "the scanning indicator clears once all hosts settle:\n{footer:?}"
        );
        assert!(
            footer.contains("attach"),
            "the footer returns to the help line:\n{footer:?}"
        );
    }

    #[tokio::test]
    async fn preview_target_follows_cursor() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;
        assert!(matches!(h.sw.current_ref(), Some(RowRef::Host { .. })));
        let t = h.sw.preview_target();
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));

        h.key(KeyCode::Down).await; // editor
        let t = h.sw.preview_target();
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));

        h.key(KeyCode::Down).await; // window 1: shell
        assert!(matches!(h.sw.current_ref(), Some(RowRef::Window { .. })));
        let t = h.sw.preview_target();
        assert_eq!(
            (t.source.as_str(), t.target.as_str()),
            ("local", "editor:1")
        );
    }

    #[tokio::test]
    async fn selected_node_renders_reverse_video() {
        let h = Harness::new(sample());
        let sel = h.tree_row_of("inference").expect("inference row");
        let other = h.tree_row_of("editor").expect("editor row");
        assert!(h.tree_row_reversed(sel), "selected row must be reversed");
        assert!(
            !h.tree_row_reversed(other),
            "non-selected row must not be reversed"
        );
    }

    #[tokio::test]
    async fn enter_attaches_session() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Enter).await; // inference preselected
        let r = h.sw.result();
        assert_eq!(
            r.chosen.as_ref().map(|s| s.name.as_str()),
            Some("inference")
        );
        assert_eq!(r.window, -1);
    }

    #[tokio::test]
    async fn enter_attaches_window() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Down).await; // inference → window 1: train
        let (name, window) = match h.sw.current_ref() {
            Some(RowRef::Window { sess, window }) => (sess.name.clone(), *window),
            other => panic!(
                "expected window node, got something else: {}",
                other.is_some()
            ),
        };
        h.key(KeyCode::Enter).await;
        let r = h.sw.result();
        assert_eq!(r.chosen.as_ref().map(|s| s.name.clone()), Some(name));
        assert_eq!(r.window, window);
    }

    #[tokio::test]
    async fn enter_on_host_attaches_recent_session() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host
        h.key(KeyCode::Enter).await;
        let r = h.sw.result();
        assert_eq!(r.chosen.as_ref().map(|s| s.name.as_str()), Some("editor"));
        assert_eq!(r.window, -1);
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
    async fn kill_removes_session_and_cache() {
        let mut h = Harness::new(sample());
        assert!(h.sw.panes.contains_key("jupiter00/inference"));
        h.ch('x').await; // arm
        assert!(
            h.text().contains("kill jupiter00/inference?"),
            "expected inline kill confirm:\n{}",
            h.text()
        );
        h.ch('y').await;
        assert_eq!(h.ops.killed.lock().unwrap().len(), 1);
        assert_eq!(h.ops.killed.lock().unwrap()[0].name, "inference");
        assert!(
            !h.sw.panes.contains_key("jupiter00/inference"),
            "kill must invalidate cache"
        );
        assert!(
            !h.text().contains("inference"),
            "killed session must disappear"
        );
    }

    #[tokio::test]
    async fn create_adds_and_selects() {
        let mut h = Harness::new(sample());
        h.ch('n').await; // inference preselected ⇒ create on jupiter00
        h.sw.set_input_text("scratch");
        h.key(KeyCode::Enter).await;
        assert_eq!(*h.ops.created.lock().unwrap(), vec!["jupiter00/scratch"]);
        assert_eq!(cur_session_name(&h).as_deref(), Some("scratch"));
    }

    #[tokio::test]
    async fn rename_rejects_leading_dash() {
        let mut h = Harness::new(sample());
        h.ch('R').await;
        h.sw.set_input_text("-bad");
        h.key(KeyCode::Enter).await;
        assert!(
            h.ops.renamed.lock().unwrap().is_empty(),
            "leading-dash rename must be refused"
        );
    }

    #[tokio::test]
    async fn create_on_unreachable_host_refused() {
        let mut h = Harness::new(sample());
        // from inference: Down → its window, Down → the unreachable db-2 host.
        h.key(KeyCode::Down).await;
        h.key(KeyCode::Down).await;
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
            h.sw.flash.to_lowercase().contains("unreachable"),
            "create on unreachable host should flash unreachable, got {:?}",
            h.sw.flash
        );
        assert!(h.ops.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn preview_shows_loading_until_fetched() {
        let mut h = Harness::new(sample());
        // inference preselected (first visit) ⇒ a loading dialog.
        assert!(
            h.sw.dialog
                .as_deref()
                .is_some_and(|d| d.contains("loading")),
            "first visit should show loading dialog, got {:?}",
            h.sw.dialog
        );
        h.key(KeyCode::Down).await; // → window 1: train (also first visit)
        assert!(
            h.sw.dialog
                .as_deref()
                .is_some_and(|d| d.contains("loading")),
            "moving to a new node should show loading"
        );
    }

    #[tokio::test]
    async fn preview_reconnecting_on_revisit() {
        let mut h = Harness::new(sample());
        h.sw.preview_cache
            .insert("jupiter00\u{0}inference".into(), "CACHED-CONTENT".into());
        h.key(KeyCode::Down).await; // away (window 1: train)
        h.key(KeyCode::Up).await; // back to inference (revisit)
        assert_eq!(
            h.sw.preview_text, "CACHED-CONTENT",
            "revisit keeps cached content"
        );
        assert!(
            h.sw.dialog
                .as_deref()
                .is_some_and(|d| d.contains("reconnecting")),
            "revisit should float reconnecting dialog, got {:?}",
            h.sw.dialog
        );
    }

    #[tokio::test]
    async fn preview_blank_on_host_without_session() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Down).await; // inference → window 1: train
        h.key(KeyCode::Down).await; // → db-2 (unreachable, no session ⇒ no target)
        assert!(
            h.sw.dialog.is_none(),
            "host with no session must not float a dialog"
        );
        assert!(
            h.sw.preview_text.trim().is_empty(),
            "host with no session clears preview"
        );
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
        assert!(
            matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2")
        );
        h.key(KeyCode::Down).await; // wrap bottom → top
        assert!(
            matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local")
        );
        h.key(KeyCode::Up).await; // wrap top → bottom
        assert!(
            matches!(h.sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "db-2")
        );
    }

    #[tokio::test]
    async fn double_click_attaches_current_node() {
        let mut h = Harness::new(sample());
        // inference preselected; a double-click inside the tree attaches it.
        h.sw.mouse_attach(5, 4);
        let r = h.sw.result();
        assert_eq!(
            r.chosen.as_ref().map(|s| s.name.as_str()),
            Some("inference")
        );
    }

    #[tokio::test]
    async fn single_click_does_not_attach() {
        let mut h = Harness::new(sample());
        h.sw.mouse_select(5, 4);
        assert!(
            h.sw.result().chosen.is_none(),
            "single click must not attach"
        );
    }

    #[tokio::test]
    async fn quit_leaves_no_choice() {
        let mut h = Harness::new(sample());
        h.ch('q').await;
        assert!(h.sw.result().chosen.is_none(), "quit must leave no choice");
    }

    #[tokio::test]
    async fn help_overlay_toggles() {
        let mut h = Harness::new(sample());
        assert!(!h.text().contains("keys"), "help hidden initially");
        h.ch('?').await;
        let out = h.text();
        assert!(
            out.contains("keys"),
            "? should show the help overlay:\n{out}"
        );
        assert!(out.contains("fuzzy filter"), "help should list keybindings");
        // Any key dismisses the modal without acting on the tree.
        h.key(KeyCode::Down).await;
        assert!(
            !h.text().contains("fuzzy filter"),
            "a key should dismiss help"
        );
        assert!(h.sw.result().chosen.is_none());
    }

    #[tokio::test]
    async fn preview_suppressed_when_focused_pane_runs_xmux() {
        // A pane already running xmux would, if captured, draw this very switcher
        // inside its own preview — an infinite overlay. Focusing it must skip the
        // capture and show a note instead.
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "selfsess", 1, true, 500)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "local/selfsess".to_string(),
            vec![win(0, "xmux", true, vec![pane(0, true, "xmux")])],
        );
        let h = Harness::new(Scan { groups, panes });
        // selfsess is the only session ⇒ preselected and previewed.
        assert!(
            !h.sw.preview_capturable(),
            "a pane running xmux must not be captured (would mirror the UI)"
        );
        assert!(
            h.text().contains("Preview hidden"),
            "the preview must show a note, not the recursive overlay:\n{}",
            h.text()
        );
        assert!(
            h.sw.dialog.is_none(),
            "a suppressed preview floats no loading/reconnecting dialog"
        );
    }

    #[tokio::test]
    async fn preview_captures_when_focused_pane_is_not_xmux() {
        // A normal pane (a shell/editor) is the "original screen" the user wants
        // to peek — it must still be captured and shown.
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "work", 1, true, 500)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "local/work".to_string(),
            vec![win(0, "shell", true, vec![pane(0, true, "vim")])],
        );
        let h = Harness::new(Scan { groups, panes });
        assert!(
            h.sw.preview_capturable(),
            "a normal pane must be captured so the original screen is shown"
        );
        assert!(
            !h.text().contains("Preview hidden"),
            "a normal pane must not be treated as a self-overlay"
        );
    }
}
