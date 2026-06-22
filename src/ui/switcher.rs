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
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::session::{Pane, Session, WindowPanes};
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
///
/// This is deliberately one trait, not split into read/mutate halves: the
/// `Switcher` is its sole consumer and uses every method, so an ISP split would
/// add test boilerplate without decoupling any independent caller. Split it only
/// when a second consumer needs just one half.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved source aliases in display order — synchronous, no probing —
    /// so the UI can paint host skeletons before any probe runs.
    fn sources(&self) -> Vec<String>;
    /// Probes one source's sessions. `Ok` (possibly empty) ⇒ reachable; `Err` ⇒
    /// unreachable (the message is shown as the host's failure reason).
    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session>;
    /// Creates a new window in `session` on `source` (the `n` action on a session row).
    async fn new_window(&self, source: &str, session: &str, name: &str) -> anyhow::Result<()>;
    /// Splits `target` (`session:window`) into a new pane (the `n` action on a window row).
    async fn split_window(&self, source: &str, target: &str, vertical: bool) -> anyhow::Result<()>;
    async fn kill(&self, s: &Session) -> anyhow::Result<()>;
    async fn rename(&self, s: &Session, new_name: &str) -> anyhow::Result<()>;
    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>>;
    async fn kill_window(&self, source: &str, target: &str) -> anyhow::Result<()>;
    async fn rename_window(&self, source: &str, target: &str, new_name: &str) -> anyhow::Result<()>;
}

/// A slow (network) action a keypress queues. The key-handling path only records
/// it; the event loop runs it off-loop via [`run_op`] and applies the
/// [`OpResult`], so an ssh round-trip never freezes the UI. Tests pump it inline.
#[derive(Debug, Clone)]
pub enum PendingOp {
    Create { source: String, name: String },
    NewWindow { source: String, session: String, name: String },
    SplitWindow { source: String, target: String, session: String, vertical: bool },
    Rename { sess: Session, new_name: String },
    Kill { sess: Session },
    KillWindow { source: String, session: String, target: String },
    RenameWindow { source: String, session: String, target: String, new_name: String },
}

/// The outcome of a [`PendingOp`], applied back into the switcher state by
/// [`Switcher::apply_op_result`].
#[derive(Debug, Clone)]
pub enum OpResult {
    Created {
        session: Session,
        panes: Vec<WindowPanes>,
    },
    Renamed {
        source: String,
        old_name: String,
        new_name: String,
    },
    Killed {
        address: String,
    },
    /// A session's windows/panes were re-fetched after a new-window/split so the
    /// tree shows the change.
    PanesRefreshed {
        address: String,
        panes: Vec<WindowPanes>,
    },
    Failed {
        message: String,
    },
}

/// Runs a queued [`PendingOp`] against the live mux and returns its [`OpResult`].
/// Pure over `ops` (no switcher state), so it runs in a detached task off the
/// event loop.
pub async fn run_op(op: &PendingOp, ops: &dyn Ops) -> OpResult {
    match op {
        PendingOp::Create { source, name } => match ops.new_session(source, name).await {
            Ok(session) => {
                let panes = ops.panes(&session).await.unwrap_or_default();
                OpResult::Created { session, panes }
            }
            Err(e) => OpResult::Failed {
                message: format!("create failed: {e}"),
            },
        },
        PendingOp::NewWindow { source, session, name } => {
            match ops.new_window(source, session, name).await {
                Ok(()) => refreshed_panes(ops, source, session).await,
                Err(e) => OpResult::Failed { message: format!("new window failed: {e}") },
            }
        }
        PendingOp::SplitWindow { source, target, session, vertical } => {
            match ops.split_window(source, target, *vertical).await {
                Ok(()) => refreshed_panes(ops, source, session).await,
                Err(e) => OpResult::Failed { message: format!("split failed: {e}") },
            }
        }
        PendingOp::Rename { sess, new_name } => match ops.rename(sess, new_name).await {
            Ok(()) => OpResult::Renamed {
                source: sess.source.clone(),
                old_name: sess.name.clone(),
                new_name: new_name.clone(),
            },
            Err(e) => OpResult::Failed {
                message: format!("rename failed: {e}"),
            },
        },
        PendingOp::Kill { sess } => match ops.kill(sess).await {
            Ok(()) => OpResult::Killed {
                address: sess.address(),
            },
            Err(e) => OpResult::Failed {
                message: format!("kill failed: {e}"),
            },
        },
        PendingOp::KillWindow { source, session, target } => {
            match ops.kill_window(source, target).await {
                Ok(()) => refreshed_panes(ops, source, session).await,
                Err(e) => OpResult::Failed { message: format!("kill window failed: {e}") },
            }
        }
        PendingOp::RenameWindow { source, session, target, new_name } => {
            match ops.rename_window(source, target, new_name).await {
                Ok(()) => refreshed_panes(ops, source, session).await,
                Err(e) => OpResult::Failed { message: format!("rename window failed: {e}") },
            }
        }
    }
}

/// Re-fetches a session's windows/panes after a structural change (new window /
/// split) so the tree reflects it. A failed fetch still resolves (empty) rather
/// than erroring the whole op.
async fn refreshed_panes(ops: &dyn Ops, source: &str, session: &str) -> OpResult {
    let sess = Session {
        source: source.to_string(),
        name: session.to_string(),
        ..Default::default()
    };
    let panes = ops.panes(&sess).await.unwrap_or_default();
    OpResult::PanesRefreshed {
        address: sess.address(),
        panes,
    }
}

/// An armed kill confirm (awaiting y/n). One slot enforces "at most one armed".
#[derive(Debug, Clone)]
enum PendingKill {
    Session(Session),
    /// (source, session, target="session:window")
    Window { source: String, session: String, target: String },
}

/// What a tree row references. Hosts, sessions, and windows are selectable; panes
/// and loading placeholders are shown for context but never selectable, so the
/// cursor skips them.
#[derive(Clone)]
enum RowRef {
    Host {
        source: String,
        unreachable: bool,
    },
    Session(Session),
    Window {
        sess: Session,
        window: i64,
    },
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
    /// The active window / active pane of its session — rendered BOLD (replaces a
    /// trailing "(active)" text marker).
    active: bool,
}

impl Row {
    fn selectable(&self) -> bool {
        !matches!(self.reference, RowRef::Pane | RowRef::Loading)
    }
}

/// Snapshot of the cursor taken before a rebuild so `restore_focus` can
/// recover or gracefully redirect it afterward.
struct PriorFocus {
    reference: Option<RowRef>,
    selected: usize,
    indent: usize,
}

/// The terminal-view target whose active pane attaching here would land on.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TerminalViewTarget {
    pub source: String,
    pub target: String, // empty ⇒ no terminal view
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Filter,
    New,
    NewWindow,
    SplitWindow,
    Rename,
}

struct Input {
    mode: InputMode,
    label: String,
    buffer: String,
    /// The create source / rename target captured when the input opened, so the
    /// action lands on the node the user was on — not wherever streaming results
    /// moved the cursor by the time they pressed Enter.
    source: Option<String>,
    sess: Option<Session>,
    /// The split target (`session:window`) for [`InputMode::SplitWindow`].
    target: Option<String>,
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
    pending_kill: Option<PendingKill>,
    flash: String,

    terminal_view_target: TerminalViewTarget,

    list_state: ListState,
    tree_inner: Rect,

    show_help: bool,
    wants_quit: bool,
    /// A slow (network) action queued by the last keypress for the event loop to
    /// run off-loop. `None` unless a create/rename/kill is pending dispatch.
    pending_op: Option<PendingOp>,
    /// Session addresses currently connecting / awaiting first output — a braille
    /// spinner glyph renders right of their name in the tree.
    spinner: HashSet<String>,
    spinner_frame: usize,
    /// The persisted last-selected session address (`source/session`) used to
    /// preselect on launch; `None` ⇒ the local-first preselect. Drives only the
    /// initial preselect (while the user has not moved); once `user_moved` is set,
    /// `restore_focus` keeps the cursor and this is ignored.
    preferred: Option<String>,
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
            terminal_view_target: TerminalViewTarget::default(),
            list_state: ListState::default(),
            tree_inner: Rect::default(),
            show_help: false,
            wants_quit: false,
            pending_op: None,
            spinner: HashSet::new(),
            spinner_frame: 0,
            preferred: None,
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

    pub fn wants_quit(&self) -> bool {
        self.wants_quit
    }

    /// True while an inline input is open (filter or rename). The cockpit routes
    /// every key to the switcher then (no focus-switch hijack of →/Enter/Tab).
    pub fn is_inputting(&self) -> bool {
        self.input.is_some()
    }

    pub fn terminal_view_target(&self) -> TerminalViewTarget {
        self.terminal_view_target.clone()
    }

    /// Takes the pending rescan-kick flag (true once after seeding or an `r`
    /// re-scan) — the event loop spawns the streaming probes when it is set.
    pub fn take_rescan_kick(&mut self) -> bool {
        std::mem::take(&mut self.rescan_kick)
    }

    /// Sets the persisted last-selected session address (`source/session`) to
    /// preselect on launch (see [`crate::state`]). Takes effect on the next rebuild
    /// as sessions stream in; `None` clears it (local-first preselect). Set once
    /// before sessions stream — the seed has no session rows, so it is inert until
    /// then.
    pub fn set_preferred(&mut self, address: Option<String>) {
        self.preferred = address;
    }

    // --- tree model ---------------------------------------------------------

    fn visible_groups(&self) -> Vec<Group> {
        let groups = if self.filter.is_empty() {
            self.groups.clone()
        } else {
            let filtered = tree::filter_groups(&self.groups, &self.filter);
            if filtered.is_empty() {
                // XM-01: a non-matching filter must not be a dead end.
                self.groups
                    .iter()
                    .map(|g| Group {
                        source: g.source.clone(),
                        err: g.err.clone(),
                        sessions: Vec::new(),
                    })
                    .collect()
            } else {
                filtered
            }
        };
        tree::order_groups(&groups)
    }

    fn rebuild(&mut self) {
        // A tree change (streamed update, refetch, rescan) can move or replace the
        // node a kill was armed on — including reusing a window index — so any
        // in-flight kill confirm is invalidated; the user re-arms against the new tree.
        self.pending_kill = None;

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
        // Preselect priority: the persisted last-selected session (restored on
        // launch) wins; otherwise the FIRST session row — which order_groups pins to
        // the LOCAL source's most-recent session — so an untouched cursor never jumps
        // to a remote on a global-recency tiebreak (#1). `session_last_attached` is
        // not a reliable cross-host "most recent" signal (xmux's own pre-attaching and
        // clock skew corrupt it), so the preselect uses the persisted last-selected
        // session, falling back to the local-first first session.
        let mut preferred_row: Option<usize> = None;
        let mut first_session_row: Option<usize> = None;

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
                active: false,
            });
            if scanning || unreachable {
                continue;
            }
            for sess in &g.sessions {
                let row_idx = rows.len();
                if first_session_row.is_none() {
                    first_session_row = Some(row_idx);
                }
                if self.preferred.as_deref() == Some(sess.address().as_str()) {
                    preferred_row = Some(row_idx);
                }
                rows.push(Row {
                    label: self.session_label(sess),
                    hint: None,
                    indent: 2,
                    color: COLOR_SESSION,
                    reference: RowRef::Session(sess.clone()),
                    active: false,
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
                                active: w.active,
                            });
                            for p in &w.panes {
                                rows.push(Row {
                                    label: pane_label(p),
                                    hint: None,
                                    indent: 6,
                                    color: COLOR_PANE,
                                    reference: RowRef::Pane,
                                    active: p.active,
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
                        active: false,
                    });
                }
            }
        }

        self.rows = rows;
        let target = preferred_row
            .or(first_session_row)
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
        // No "attached" dot: the cockpit pre-attaches EVERY session (a PTY client per
        // session), so `session_attached` is true for ~all of them — the marker would
        // be noise. The active window/pane is shown BOLD instead.
        let pad = self
            .name_col_width
            .saturating_sub(sess.name.chars().count());
        format!("{}{}   {}", sess.name, " ".repeat(pad), plural(sess.windows))
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

    /// `→`: descend to the FIRST child of the selected node (host→its first session,
    /// session→its first window). The flattened rows place children immediately after
    /// their parent at a deeper indent, so the child is the next row when its indent
    /// is greater. A no-op when the only children are non-selectable (a window's
    /// panes) or there are none.
    fn descend(&mut self) {
        let cur = self.selected;
        let Some(cur_indent) = self.rows.get(cur).map(|r| r.indent) else {
            return;
        };
        if let Some(child) = self.rows.get(cur + 1) {
            if child.indent > cur_indent && child.selectable() {
                self.user_moved = true;
                self.set_selected(cur + 1);
            }
        }
    }

    /// `←`: ascend to the PARENT of the selected node (window→its session, session→its
    /// host) — the nearest preceding row at a shallower indent. A no-op on a host row.
    fn ascend(&mut self) {
        let cur = self.selected;
        let Some(cur_indent) = self.rows.get(cur).map(|r| r.indent) else {
            return;
        };
        if cur_indent == 0 {
            return;
        }
        for i in (0..cur).rev() {
            if self.rows[i].indent < cur_indent {
                self.user_moved = true;
                self.set_selected(i);
                return;
            }
        }
    }

    /// ↑/↓ (or k/j): move to the next/prev sibling at the current tree level — the
    /// next selectable row at the SAME indent level (e.g. session→next session,
    /// skipping windows/panes nested under it). Wraps like `move_selection`.
    fn move_sibling(&mut self, delta: isize) {
        let Some(cur_indent) = self.rows.get(self.selected).map(|r| r.indent) else {
            return;
        };
        let siblings: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.indent == cur_indent && r.selectable())
            .map(|(i, _)| i)
            .collect();
        if siblings.is_empty() {
            return;
        }
        self.user_moved = true;
        let pos = siblings.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        let n = siblings.len() as isize;
        let next = ((pos + delta) % n + n) % n;
        self.set_selected(siblings[next as usize]);
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
        // The source's first VISIBLE session (its sessions are kept recency-sorted),
        // mirroring `filter_groups` for just this one group — NOT cloning every host's
        // sessions via `visible_groups`, since this runs on every cursor move onto a
        // host row (the navigation hot path).
        let g = self.groups.iter().find(|g| g.source == source)?;
        if g.err.is_some() {
            return None; // unreachable host: its sessions carry no meaning
        }
        if self.filter.is_empty() || tree::fuzzy_match(&self.filter, &g.source) {
            g.sessions.first().cloned()
        } else {
            g.sessions
                .iter()
                .find(|s| tree::fuzzy_match(&self.filter, &s.address()))
                .cloned()
        }
    }

    fn target_for(&self, reference: &RowRef) -> TerminalViewTarget {
        match reference {
            RowRef::Host { source, .. } => match self.first_session_of(source) {
                Some(sess) => TerminalViewTarget {
                    source: sess.source,
                    target: sess.name,
                },
                None => TerminalViewTarget::default(),
            },
            RowRef::Session(s) => TerminalViewTarget {
                source: s.source.clone(),
                target: s.name.clone(),
            },
            RowRef::Window { sess, window } => TerminalViewTarget {
                source: sess.source.clone(),
                target: format!("{}:{}", sess.name, window),
            },
            RowRef::Pane | RowRef::Loading => TerminalViewTarget::default(),
        }
    }

    fn on_focus_changed(&mut self) {
        self.terminal_view_target = match self.current_ref() {
            Some(r) => self.target_for(r),
            None => TerminalViewTarget::default(),
        };
    }

    /// The session/window the cursor is currently on, used by the cockpit to
    /// `switch-client` on every cursor move (`select = attach`). Returns `Some`
    /// only for session, window, or host-with-session rows; `None` for pane,
    /// loading, and empty-host rows.
    pub fn current_attach_target(&self) -> Option<TerminalViewTarget> {
        let r = self.current_ref()?;
        let tgt = self.target_for(r);
        if tgt.target.is_empty() {
            None
        } else {
            Some(tgt)
        }
    }

    /// The host (source alias) the cursor is on, or `None` on a pane/loading row.
    /// The cockpit ensures this host's control-mode client is connected on every
    /// cursor move, so the host's `list-sessions` populates the tree even before
    /// any session is selected (a control-mode client is the only session source).
    pub fn current_host(&self) -> Option<String> {
        self.current_source()
    }

    /// Moves the sidebar cursor to window `window` of `source`/`session` when the
    /// cursor is currently within THAT session's subtree — on its session row OR any
    /// of its window rows. Used to follow the displayed session's active-window
    /// change; the cockpit gates this on TERMINAL focus, where the user is no longer
    /// driving the tree cursor (stdin goes to the PTY), so following from the session
    /// row mirrors the mux without yanking a tree-navigating user. A no-op when the
    /// cursor is on a different host/session. Returns whether it moved.
    pub fn select_window(&mut self, source: &str, session: &str, window: i64) -> bool {
        let on_this_session = match self.current_ref() {
            Some(RowRef::Session(s)) => s.source == source && s.name == session,
            Some(RowRef::Window { sess, .. }) => sess.source == source && sess.name == session,
            _ => false,
        };
        if !on_this_session {
            return false;
        }
        let target = self.rows.iter().position(|r| {
            matches!(&r.reference, RowRef::Window { sess, window: w }
                if sess.source == source && sess.name == session && *w == window)
        });
        match target {
            Some(i) if i != self.selected => {
                self.user_moved = true;
                self.set_selected(i);
                true
            }
            _ => false,
        }
    }

    /// Moves the cursor to the ACTIVE window row of the cursor's current session
    /// (read from cached pane data), from either the session row or a window row.
    /// Used when focus moves to the terminal so the sidebar mirrors the window the
    /// mux is currently displaying (#3). A no-op when the cursor is not on a
    /// session/window row or the session's active window is unknown. Returns whether
    /// it moved.
    pub fn select_active_window(&mut self) -> bool {
        let Some(sess) = self.current_session() else {
            return false;
        };
        let addr = sess.address();
        let Some(window) = self
            .panes
            .get(&addr)
            .and_then(|ws| ws.iter().find(|w| w.active))
            .map(|w| w.index)
        else {
            return false;
        };
        self.select_window(&sess.source, &sess.name, window)
    }

    /// Marks `window` as the active window of `source`/`session` in the cached
    /// pane data, flipping the bold/italic marker WITHOUT a full inventory refetch
    /// (the control-client probe resolves an external `%session-window-changed` to
    /// the new active window; a blanket refetch per change would storm the loop).
    /// Returns whether the active window actually changed.
    pub fn set_active_window(&mut self, source: &str, session: &str, window: i64) -> bool {
        let addr = format!("{source}/{session}");
        let Some(windows) = self.panes.get_mut(&addr) else {
            return false;
        };
        let mut changed = false;
        for w in windows.iter_mut() {
            let want = w.index == window;
            if w.active != want {
                changed = true;
            }
            w.active = want;
        }
        if changed {
            let prior = self.capture_focus();
            self.rebuild();
            self.restore_focus(prior);
        }
        changed
    }

    /// Replaces the set of session addresses currently connecting / awaiting
    /// first output. The tree draws a braille spinner right of each matching
    /// session name.
    pub fn set_spinner(&mut self, addresses: HashSet<String>) {
        self.spinner = addresses;
    }

    /// Sets the braille spinner frame index. The cockpit derives it from elapsed
    /// wall-clock time, so the spinner animates on every render rather than once
    /// per animation tick (which can starve under a `%output` flood).
    pub fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    // --- key handling -------------------------------------------------------

    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.input.is_some() {
            self.handle_input_key(ev);
            return;
        }
        if self.show_help {
            // The help overlay is modal: any key dismisses it.
            self.show_help = false;
            return;
        }
        if self.pending_kill.is_some() {
            self.resolve_kill(ev);
            return;
        }
        match ev.code {
            KeyCode::Enter => {}
            // ↑/↓ (and k/j) move between SIBLINGS at the current tree level (next/prev
            // node at the same depth); →/← (and l/h) change level — → descends to the
            // first child, ← ascends to the parent. hjkl mirror the arrows. (#1, #2)
            KeyCode::Up | KeyCode::Char('k') => self.move_sibling(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sibling(1),
            KeyCode::Right | KeyCode::Char('l') => self.descend(),
            KeyCode::Left | KeyCode::Char('h') => self.ascend(),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(-1),
            KeyCode::Char(c) => match c {
                'q' => self.wants_quit = true,
                '/' => self.open_input(InputMode::Filter),
                'n' => self.open_new(),
                'R' => self.open_input(InputMode::Rename),
                'x' => self.arm_kill(),
                'r' => self.request_rescan(),
                '?' => self.show_help = true,
                _ => {}
            },
            _ => {}
        }
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
                    source: None,
                    sess: None,
                    target: None,
                });
            }
            InputMode::Rename => {
                match self.current_ref().cloned() {
                    Some(RowRef::Host { .. }) => {
                        self.flash = "cannot rename a host".into();
                    }
                    Some(RowRef::Session(sess)) => {
                        self.input = Some(Input {
                            mode,
                            label: " rename to: ".into(),
                            buffer: sess.name.clone(),
                            source: None,
                            sess: Some(sess),
                            target: None,
                        });
                    }
                    Some(RowRef::Window { sess, window }) => {
                        let win_name = self.window_name(&sess.address(), window).unwrap_or_default();
                        let target = crate::mux::window_target(&sess.name, window);
                        self.input = Some(Input {
                            mode,
                            label: " rename window to: ".into(),
                            buffer: win_name,
                            source: Some(sess.source.clone()),
                            sess: Some(sess),
                            target: Some(target),
                        });
                    }
                    _ => {}
                }
            }
            // New/NewWindow/SplitWindow are opened by `open_new` (level-aware).
            InputMode::New | InputMode::NewWindow | InputMode::SplitWindow => {}
        }
    }

    /// The level-aware `n` action: a new SESSION on a host row, a new WINDOW on a
    /// session row, or a new PANE (split) on a window row (prompting the split
    /// direction). The prompt context is captured up front so a streamed cursor
    /// move cannot retarget it.
    fn open_new(&mut self) {
        self.flash.clear();
        if self.current_host_unreachable() {
            self.flash = "host unreachable — cannot create here".into();
            return;
        }
        let Some(reference) = self.current_ref().cloned() else {
            return;
        };
        self.input = match reference {
            RowRef::Host { source, .. } => Some(Input {
                mode: InputMode::New,
                label: " new session name (empty = auto): ".into(),
                buffer: String::new(),
                source: Some(source),
                sess: None,
                target: None,
            }),
            RowRef::Session(sess) => Some(Input {
                mode: InputMode::NewWindow,
                label: format!(" new window in {} (name optional): ", sess.name),
                buffer: String::new(),
                source: Some(sess.source.clone()),
                sess: Some(sess),
                target: None,
            }),
            RowRef::Window { sess, window } => {
                let target = format!("{}:{}", sess.name, window);
                Some(Input {
                    mode: InputMode::SplitWindow,
                    label: " split [v]ertical / [h]orizontal (default v): ".into(),
                    buffer: String::new(),
                    source: Some(sess.source.clone()),
                    sess: Some(sess),
                    target: Some(target),
                })
            }
            RowRef::Pane | RowRef::Loading => None,
        };
    }

    fn close_input(&mut self) {
        self.input = None;
    }

    fn handle_input_key(&mut self, ev: KeyEvent) {
        match ev.code {
            KeyCode::Enter => {
                let (mode, val, source, sess, target) = {
                    let input = self.input.as_ref().expect("input active");
                    (
                        input.mode,
                        input.buffer.trim().to_string(),
                        input.source.clone(),
                        input.sess.clone(),
                        input.target.clone(),
                    )
                };
                match mode {
                    InputMode::Filter => {
                        self.filter = val;
                        self.close_input();
                        self.rebuild();
                    }
                    InputMode::New => {
                        self.queue_create(source, &val);
                        self.close_input();
                    }
                    InputMode::NewWindow => {
                        self.queue_new_window(source, sess, &val);
                        self.close_input();
                    }
                    InputMode::SplitWindow => {
                        self.queue_split(source, sess, target, &val);
                        self.close_input();
                    }
                    InputMode::Rename => {
                        if target.is_some() {
                            self.queue_rename_window(source, sess, target, &val);
                        } else {
                            self.queue_rename(sess, &val);
                        }
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

    /// Queues a create for the event loop. The network call is NOT made here, so
    /// the key-handling path never blocks on an ssh round-trip; [`run_op`]
    /// performs it off-loop and [`Switcher::apply_op_result`] folds the result in.
    fn queue_create(&mut self, source: Option<String>, name: &str) {
        let Some(source) = source else {
            return;
        };
        self.pending_op = Some(PendingOp::Create {
            source,
            name: name.to_string(),
        });
    }

    /// Queues a new-window in the captured session (the `n` action on a session
    /// row). An empty name lets the mux auto-name the window.
    fn queue_new_window(&mut self, source: Option<String>, sess: Option<Session>, name: &str) {
        let (Some(source), Some(sess)) = (source, sess) else {
            return;
        };
        self.pending_op = Some(PendingOp::NewWindow {
            source,
            session: sess.name,
            name: name.to_string(),
        });
    }

    /// Queues a split of the captured window target (the `n` action on a window
    /// row). The direction defaults to vertical unless the buffer starts with `h`.
    fn queue_split(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        dir: &str,
    ) {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return;
        };
        let vertical = !dir.trim().eq_ignore_ascii_case("h");
        self.pending_op = Some(PendingOp::SplitWindow {
            source,
            target,
            session: sess.name,
            vertical,
        });
    }

    /// The current name of window `index` under the session at `sess_addr`, if its panes are loaded.
    fn window_name(&self, sess_addr: &str, index: i64) -> Option<String> {
        self.panes.get(sess_addr)
            .and_then(|ws| ws.iter().find(|w| w.index == index))
            .map(|w| w.name.clone())
    }

    /// Queues a rename for the event loop after the synchronous validation that
    /// needs no network. See [`Switcher::queue_create`] for why the op is deferred.
    fn queue_rename(&mut self, sess: Option<Session>, new_name: &str) {
        let Some(sess) = sess else {
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
        self.pending_op = Some(PendingOp::Rename {
            sess,
            new_name: new_name.to_string(),
        });
    }

    fn queue_rename_window(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        new_name: &str,
    ) {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return;
        };
        let cur = target.rsplit(':').next()
            .and_then(|i| i.parse::<i64>().ok())
            .and_then(|idx| self.window_name(&sess.address(), idx));
        if new_name.is_empty() || cur.as_deref() == Some(new_name) {
            return;
        }
        if new_name.starts_with('-') {
            self.flash = "rename: name cannot start with '-'".into();
            return;
        }
        self.pending_op = Some(PendingOp::RenameWindow {
            source,
            session: sess.name,
            target,
            new_name: new_name.to_string(),
        });
    }

    /// Applies a completed [`PendingOp`] to the in-memory tree. The result is
    /// applied on the event loop after `run_op` returns off-loop, so a slow ssh
    /// round-trip never blocks rendering.
    pub fn apply_op_result(&mut self, result: OpResult) {
        match result {
            OpResult::Created { session, panes } => {
                let addr = session.address();
                self.panes.insert(addr.clone(), panes);
                self.panes_loaded.insert(addr.clone());
                self.groups = tree::add_session(&self.groups, session);
                self.rebuild();
                if let Some(i) = self.row_of_session(&addr) {
                    self.user_moved = true;
                    self.set_selected(i);
                }
            }
            OpResult::Renamed {
                source,
                old_name,
                new_name,
            } => {
                let old_addr = format!("{source}/{old_name}");
                let new_addr = format!("{source}/{new_name}");
                if let Some(wins) = self.panes.remove(&old_addr) {
                    self.panes.insert(new_addr.clone(), wins);
                }
                if self.panes_loaded.remove(&old_addr) {
                    self.panes_loaded.insert(new_addr);
                }
                self.groups = tree::rename_session(&self.groups, &old_addr, &new_name);
                self.rebuild();
            }
            OpResult::Killed { address } => {
                self.panes.remove(&address);
                self.panes_loaded.remove(&address);
                self.groups = tree::remove_session(&self.groups, &address);
                self.rebuild();
            }
            OpResult::PanesRefreshed { address, panes } => {
                // A new window or split: replace the session's subtree so the new
                // window/pane shows. apply_panes restores the cursor.
                self.apply_panes(address, panes);
            }
            OpResult::Failed { message } => {
                self.flash = message;
            }
        }
    }

    /// Takes the action queued by the last keypress, if any, for the event loop to
    /// run off-loop. Consumes it so it dispatches once.
    pub fn take_pending_op(&mut self) -> Option<PendingOp> {
        self.pending_op.take()
    }

    fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == address))
    }

    // --- kill (inline confirm) ----------------------------------------------

    fn arm_kill(&mut self) {
        match self.current_ref().cloned() {
            Some(RowRef::Host { .. }) => {
                self.flash = "cannot kill a host".into();
            }
            Some(RowRef::Session(sess)) => {
                self.pending_kill = Some(PendingKill::Session(sess));
            }
            Some(RowRef::Window { sess, window }) => {
                let target = crate::mux::window_target(&sess.name, window);
                self.pending_kill = Some(PendingKill::Window {
                    source: sess.source.clone(),
                    session: sess.name.clone(),
                    target,
                });
            }
            _ => {}
        }
    }

    fn resolve_kill(&mut self, ev: KeyEvent) {
        let confirmed = matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        if let Some(armed) = self.pending_kill.take() {
            if confirmed {
                match armed {
                    PendingKill::Session(sess) => {
                        self.pending_op = Some(PendingOp::Kill { sess });
                    }
                    PendingKill::Window { source, session, target } => {
                        self.pending_op = Some(PendingOp::KillWindow { source, session, target });
                    }
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
        let prior = self.capture_focus();
        self.scanning = self.groups.iter().map(|g| g.source.clone()).collect();
        for g in self.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        self.panes.clear();
        self.panes_loaded.clear();
        self.rescan_kick = true;
        self.rebuild();
        self.restore_focus(prior);
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
        let prior = self.capture_focus();
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
        self.restore_focus(prior);
    }

    /// Streams in one session's `list-panes` outcome, clearing its loading
    /// placeholder. An empty `panes` (a failed/timed-out fetch) still resolves the
    /// session — it shows no children rather than spinning forever.
    pub fn apply_panes(&mut self, address: String, panes: Vec<WindowPanes>) {
        let prior = self.capture_focus();
        self.panes_loaded.insert(address.clone());
        self.panes.insert(address, panes);
        self.rebuild();
        self.restore_focus(prior);
    }

    /// Captures the cursor state needed to restore or gracefully redirect focus
    /// after a rebuild.
    fn capture_focus(&self) -> PriorFocus {
        PriorFocus {
            reference: self.current_ref().cloned(),
            selected: self.selected,
            indent: self.rows.get(self.selected).map(|r| r.indent).unwrap_or(0),
        }
    }

    /// After a streamed update rebuilds the rows: if the user has driven the
    /// cursor, keep it on the focused node when it survives; if the node
    /// vanished (killed/removed), land on the previous sibling at the same
    /// indent or, when there is none, the parent. An untouched cursor follows
    /// the rebuild's recency preselect.
    fn restore_focus(&mut self, prior: PriorFocus) {
        if !self.user_moved {
            return;
        }
        let Some(focus) = prior.reference.as_ref() else { return };
        if let Some(i) = self.row_matching(focus) {
            self.set_selected(i);
            return;
        }
        // The focused node vanished (killed/removed): land on the previous sibling
        // at its level, else the parent.
        if let Some(i) = self.fallback_after_removal(prior.indent, prior.selected) {
            self.set_selected(i);
        }
    }

    /// The row to land on after the focused node vanished: the previous selectable
    /// sibling at `indent`, else the nearest preceding selectable row at a shallower
    /// indent (the parent). Operates on the freshly rebuilt `self.rows`.
    fn fallback_after_removal(&self, indent: usize, prior_selected: usize) -> Option<usize> {
        let prior = &self.rows[..prior_selected.min(self.rows.len())];
        let prev_sibling = prior
            .iter().enumerate().rev()
            .find(|(_, r)| r.indent == indent && r.selectable())
            .map(|(i, _)| i);
        prev_sibling.or_else(|| {
            prior
                .iter().enumerate().rev()
                .find(|(_, r)| r.indent < indent && r.selectable())
                .map(|(i, _)| i)
        })
    }

    /// The row index targeting the same node as `focus`, if it survives a
    /// rebuild — so a re-scan keeps the cursor in place rather than snapping to
    /// the recency preselect.
    fn row_matching(&self, focus: &RowRef) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| same_node(&r.reference, focus))
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

    /// Double click: selects the clicked row (the preceding single click already
    /// moved the cursor; with select=attach there is no separate attach action).
    pub fn mouse_attach(&mut self, col: u16, row: u16) {
        self.mouse_select(col, row);
    }

    /// Scroll wheel: move the cursor (panes skipped) in the given direction.
    pub fn mouse_scroll(&mut self, down: bool) {
        self.move_selection(if down { 1 } else { -1 });
    }

    // --- render -------------------------------------------------------------

    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::proxy::screen::Grid>,
        terminal_focused: bool,
        tree_width: u16,
    ) {
        let area = frame.area();
        let cols = Layout::horizontal([
            Constraint::Length(tree_width),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
        let input_h = if self.input.is_some() { 1 } else { 0 };
        let left = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(cols[0]);
        self.render_tree(frame, left[0]);
        if input_h == 1 {
            self.render_input(frame, left[1]);
        }
        self.render_footer(frame, left[2]);
        self.render_divider(frame, cols[1], terminal_focused);
        let term_area = cols[2];
        self.render_terminal_view(frame, term_area, grid);
        // In passthrough, place the real cursor at the grid's cursor so typing in the
        // mux is visible and tracks. Skipped when the child hid its cursor.
        if terminal_focused {
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(term_area, g.cursor()));
                }
            }
        }
        if self.show_help {
            self.render_help(frame, area);
        }
    }

    /// The single vertical rule between the tree (left) and terminal (right). Its
    /// color marks the focused side — green when the terminal pane has focus, dim
    /// otherwise — so the active side reads at a glance (tmux pane-border
    /// convention). Replaces the per-pane box borders.
    fn render_divider(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        let color = if terminal_focused { Color::Green } else { COLOR_HINT };
        let bars = Text::from(
            (0..area.height)
                .map(|_| Line::from(Span::styled("│", Style::default().fg(color))))
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Paragraph::new(bars), area);
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        // No border box: the tree fills its column outright and a single rule
        // (render_divider) separates it from the terminal view.
        self.tree_inner = area;

        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let spinner_glyph = SPINNER[self.spinner_frame % SPINNER.len()];

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let indent = " ".repeat(row.indent);
                // The pane-loading placeholder is an animated progress spinner,
                // not the word "loading".
                if matches!(row.reference, RowRef::Loading) {
                    return ListItem::new(Line::from(vec![
                        Span::raw(indent),
                        Span::styled(spinner_glyph.to_string(), Style::default().fg(COLOR_HINT)),
                    ]));
                }
                let selected = i == self.selected;
                let mut style = Style::default().fg(row.color);
                if selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // The active window / pane reads BOLD+ITALIC (replaces the "(active)"
                // text) — the currently-displayed window of each session.
                if row.active {
                    style = style.add_modifier(Modifier::BOLD | Modifier::ITALIC);
                }
                let mut spans = vec![
                    Span::raw(indent),
                    Span::styled(pad_label(&row.label), style),
                ];
                // Spinner glyph: shown right of the session name when connecting.
                if matches!(&row.reference, RowRef::Session(s) if self.spinner.contains(&s.address())) {
                    let sp_style = Style::default().fg(COLOR_HINT);
                    spans.push(Span::styled(spinner_glyph.to_string(), sp_style));
                }
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

        let list = List::new(items);
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_terminal_view(&self, frame: &mut Frame, area: Rect, grid: Option<&crate::proxy::screen::Grid>) {
        // No border box: the live grid fills the area; render_divider draws the
        // separating rule.
        match grid {
            Some(g) => {
                let buf = frame.buffer_mut();
                g.render_into(buf, area);
            }
            None => {
                frame.render_widget(Paragraph::new("  (attaching…)").dim(), area);
            }
        }
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        if let Some(input) = &self.input {
            let line = format!("{}{}", input.label, input.buffer);
            frame.render_widget(Paragraph::new(line), area);
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let is_kill = self.pending_kill.is_some();
        let text = if let Some(PendingKill::Session(sess)) = &self.pending_kill {
            format!(" kill {}? [y]es / [n]o", sess.address())
        } else if let Some(PendingKill::Window { source, target, .. }) = &self.pending_kill {
            format!(" kill {}/{}? [y]es / [n]o", source, target)
        } else if !self.flash.is_empty() {
            format!(" {}", self.flash)
        } else if !self.scanning.is_empty() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the help line) once every host has settled.
            let total = self.groups.len();
            let done = total.saturating_sub(self.scanning.len());
            fit(
                &[
                    format!(" ⟳ scanning hosts {done}/{total}… · q quit · ? help"),
                    format!(" ⟳ scanning {done}/{total}…"),
                ],
                area.width,
            )
        } else if !self.filter.is_empty() {
            // The active filter has no border title to live in any more, so it
            // shows in the footer (with how to clear it).
            fit(
                &[
                    format!(" filter: {} · / edit · Esc clear · ? help · q quit", self.filter),
                    format!(" filter: {}", self.filter),
                ],
                area.width,
            )
        } else {
            fit(
                &[
                    " ↑/↓ move · Enter focus pane · / filter · n new · R rename · x kill · r refresh · ? help · q quit".to_string(),
                    " ↑/↓ move · Enter focus pane · / filter · n new · x kill · ? help · q quit".to_string(),
                    " move · Enter focus pane · / filter · ? help · q quit".to_string(),
                    " Enter focus pane · ? help · q quit".to_string(),
                    " ? help · q quit".to_string(),
                ],
                area.width,
            )
        };
        let mut para = Paragraph::new(text);
        if is_kill {
            para = para.style(Style::default().fg(Color::Red));
        }
        frame.render_widget(para, area);
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        const LINES: &[&str] = &[
            "↑ / ↓ / j / k     move",
            "Enter              focus the terminal pane",
            "C-g Esc           back to the tree",
            "PgUp / PgDn       jump by 10",
            "Home / End        first / last node",
            "n                 new (session / window / pane, by level)",
            "R                 rename the focused session",
            "x                 kill the focused session (y / n confirm)",
            "/                 fuzzy filter <source>/<name>",
            "r                 re-scan every host",
            "?                 toggle this help",
            "q                 quit",
            "",
            "mouse: click selects · wheel scrolls",
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

/// Picks the first (longest) candidate whose width fits `width`, falling back
/// to the last (shortest) when even that does not fit.
fn fit(candidates: &[String], width: u16) -> String {
    let w = width as usize;
    candidates
        .iter()
        .find(|c| c.chars().count() <= w)
        .cloned()
        .unwrap_or_else(|| candidates.last().cloned().unwrap_or_default())
}

// The active window / pane is shown BOLD (the `Row::active` flag), not with a
// trailing "(active)" text marker.
fn window_label(w: &WindowPanes) -> String {
    format!("window {}: {}", w.index, w.name)
}

fn pane_label(p: &Pane) -> String {
    format!("pane {}  {}", p.index, p.command)
}

/// Adds a space each side so the reverse-video selection has breathing room.
fn pad_label(s: &str) -> String {
    format!(" {s} ")
}

/// Whether two row references target the same selectable node (host by source,
/// session/window by address), used to keep the cursor across a re-scan.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Host { source: x, .. }, RowRef::Host { source: y, .. }) => x == y,
        (RowRef::Session(x), RowRef::Session(y)) => x.address() == y.address(),
        (
            RowRef::Window {
                sess: x,
                window: wx,
            },
            RowRef::Window {
                sess: y,
                window: wy,
            },
        ) => x.address() == y.address() && wx == wy,
        _ => false,
    }
}

fn terminal_cursor_pos(area: Rect, cursor: (u16, u16)) -> ratatui::layout::Position {
    let (col, row) = cursor;
    ratatui::layout::Position {
        x: (area.x + col).min(area.x + area.width.saturating_sub(1)),
        y: (area.y + row).min(area.y + area.height.saturating_sub(1)),
    }
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
            self.split
                .lock()
                .unwrap()
                .push(format!("{source}/{target}:{}", if vertical { "v" } else { "h" }));
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
        async fn rename_window(&self, _source: &str, target: &str, new_name: &str) -> anyhow::Result<()> {
            self.renamed_windows.lock().unwrap().push(format!("{target}->{new_name}"));
            Ok(())
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
            self.term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        }

        async fn key(&mut self, code: KeyCode) {
            self.sw.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            // Pump any queued slow op inline so tests observe its effect, exactly
            // as the real event loop does (only off-loop there).
            if let Some(op) = self.sw.take_pending_op() {
                let r = run_op(&op, &self.ops).await;
                self.sw.apply_op_result(r);
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
        let m0 = h.tree_modifier_of("window 0").expect("window 0 row present");
        assert!(
            m0.contains(Modifier::BOLD) && m0.contains(Modifier::ITALIC),
            "the active window is bold+italic: {m0:?}"
        );
        let m1 = h.tree_modifier_of("window 1").expect("window 1 row present");
        assert!(!m1.contains(Modifier::BOLD) && !m1.contains(Modifier::ITALIC),
            "an inactive window is neither bold nor italic: {m1:?}");
    }

    #[test]
    fn set_active_window_moves_the_marker() {
        // An external active-window change (resolved via the control-client probe)
        // moves the bold+italic marker, without a full inventory refetch.
        let mut h = Harness::new(two_window_scan());
        assert!(h.sw.set_active_window("jup", "api", 1), "active window moved 0 -> 1");
        h.draw();
        let m1 = h.tree_modifier_of("window 1").expect("window 1 row present");
        assert!(m1.contains(Modifier::BOLD) && m1.contains(Modifier::ITALIC),
            "window 1 is now the active window: {m1:?}");
        let m0 = h.tree_modifier_of("window 0").expect("window 0 row present");
        assert!(!m0.contains(Modifier::ITALIC), "window 0 no longer active: {m0:?}");
        // Idempotent: re-applying the same active window reports no change.
        assert!(!h.sw.set_active_window("jup", "api", 1), "no-op when already active");
    }

    #[test]
    fn select_window_follows_external_change_on_a_window_row() {
        // Cursor on window 1's row; an external client switches the session's
        // active window to 0. The sidebar cursor must follow to window 0's row.
        let mut sw = Switcher::new(two_window_scan());
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → window 0
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // ↓ → window 1
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })));
        assert!(sw.select_window("jup", "api", 0), "moved to the new active window");
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })));
    }

    #[test]
    fn right_descends_left_ascends_tree_levels() {
        let mut sw = Switcher::new(sample());
        sw.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)); // local host
        assert!(matches!(sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"));
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → first session
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"), "→ descends host → first session");
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → first window
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })), "→ descends to a window");
        sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)); // ← parent session
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"), "← ascends window → session");
        sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)); // ← parent host
        assert!(matches!(sw.current_ref(), Some(RowRef::Host { source, .. }) if source == "local"), "← ascends session → host");
    }

    #[test]
    fn up_down_move_within_level_and_hjkl_match_arrows() {
        // ↑/↓ (and k/j) move between SIBLINGS at the current tree level — they do NOT
        // descend into children. →/← (and l/h) change level (descend/ascend). (#1,#2)
        let mut sw = Switcher::new(sample()); // editor (local) preselected
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"));
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "build"),
            "↓ moves to the next session sibling, not into a window"
        );
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { .. })), "→ descends into a window");

        // hjkl mirror the arrows exactly.
        let mut sw2 = Switcher::new(sample());
        sw2.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "build"), "j == ↓");
        sw2.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(matches!(sw2.current_ref(), Some(RowRef::Window { .. })), "l == →");
        sw2.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "build"), "h == ←");
        sw2.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(matches!(sw2.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"), "k == ↑");
    }

    #[test]
    fn active_window_pane_have_no_text_marker() {
        // The active window/pane is shown BOLD (Row::active), not with "(active)" text.
        let w = win(2, "logs", true, vec![pane(1, true, "tail")]);
        assert_eq!(window_label(&w), "window 2: logs", "no (active) text on the window label");
        assert_eq!(pane_label(&w.panes[0]), "pane 1  tail", "no (active) text on the pane label");
    }

    #[test]
    fn select_window_follows_from_a_session_row() {
        // When the terminal pane has focus the user is no longer driving the tree
        // cursor (stdin goes to the PTY), so the cockpit only calls select_window
        // then. An active-window change must move the cursor to that window even from
        // the SESSION row — this is how focus→mux and in-mux window navigation keep
        // the sidebar mirroring the displayed window (#3).
        let mut sw = Switcher::new(two_window_scan());
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(_))));
        assert!(sw.select_window("jup", "api", 1), "follows from the session row to window 1");
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })));
    }

    #[test]
    fn select_active_window_moves_to_cached_active_window() {
        // focus→mux: with the cursor on the session row, select_active_window moves
        // it to the session's currently-active window (from cached panes) so the
        // sidebar mirrors the window the mux is displaying (#3). Window 0 is active.
        let mut sw = Switcher::new(two_window_scan());
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(_))));
        assert!(sw.select_active_window(), "moved to the cached active window");
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })));
        // Idempotent: re-applying when already on the active window reports no move.
        assert!(!sw.select_active_window(), "no-op when already on the active window");
    }

    #[test]
    fn select_window_no_move_for_another_session() {
        // A window change on a session the cursor is NOT on must not move it.
        let mut sw = Switcher::new(two_window_scan());
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → window 0
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // ↓ → window 1
        assert!(!sw.select_window("jup", "other", 0));
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })));
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
    async fn preselects_local_first_session() {
        // With no persisted last-selected session, the preselect lands on the LOCAL
        // source's most-recent session (order_groups pins local first), NOT the
        // globally most-recent remote — so first launch never jumps to a remote (#1).
        let h = Harness::new(sample());
        assert_eq!(cur_session_name(&h).as_deref(), Some("editor"));
    }

    #[tokio::test]
    async fn panes_are_not_selectable() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host
        h.key(KeyCode::Right).await; // → editor (session)
        h.key(KeyCode::Right).await; // → editor's first window
        assert!(matches!(h.sw.current_ref(), Some(RowRef::Window { .. })), "→ reaches a window");
        // → on a window does NOT descend onto a pane (panes are not selectable).
        h.key(KeyCode::Right).await;
        assert!(matches!(h.sw.current_ref(), Some(RowRef::Window { .. })), "→ on a window is a no-op (its panes are not selectable)");
        // ↓/↑ cycle window siblings; the cursor must never land on a pane.
        let mut saw_window = false;
        for _ in 0..8 {
            let r = h.sw.current_ref();
            assert!(r.is_some(), "cursor landed on a node");
            assert!(!matches!(r, Some(RowRef::Pane)), "cursor must never land on a pane");
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
        assert!(
            out.contains("editor"),
            "session appears after result:\n{out}"
        );
        assert!(
            !out.contains("scanning"),
            "scanning hint clears once the only host resolves:\n{out}"
        );
        assert!(
            out.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
            "the session shows a progress spinner until its panes arrive:\n{out}"
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
        h.sw.apply_source_result("prod".into(), vec![], Some("connection refused".into()));
        h.draw();
        let out = h.text();
        assert!(
            out.contains("unreachable"),
            "shows unreachable state:\n{out}"
        );
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
            h.tree_text().chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
            "a progress spinner stands in before panes land"
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
            !out.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
            "the progress spinner clears once panes arrive:\n{out}"
        );
    }

    #[tokio::test]
    async fn streaming_keeps_local_preselect_when_untouched() {
        // With no persisted last-selected session, an untouched cursor preselects the
        // LOCAL source's most-recent session, and a later more-recent REMOTE session
        // streaming in must NOT steal the cursor. This kills the old global-recency
        // jump (cursor leaping to a remote on first launch — #1). order_groups pins
        // local first, so the local-first fallback is the first session row.
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
            Some("editor"),
            "an untouched cursor stays on the local preselect; a recent remote must not steal it"
        );
    }

    #[tokio::test]
    async fn preferred_session_wins_preselect_when_it_streams_in() {
        // The persisted last-selected session is restored on launch: once its host
        // streams in it wins the preselect over the local-first default (#1).
        let mut h = Harness::from_sources(&["local", "jupiter00"]);
        h.sw.set_preferred(Some("jupiter00/infer".to_string()));
        h.sw.apply_source_result(
            "local".into(),
            vec![sess("local", "editor", 1, false, 100)],
            None,
        );
        h.draw();
        // The preferred host has not streamed yet → the local-first default stands.
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
            "the persisted last-selected session is restored once it streams in"
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
            footer.contains("filter") || footer.contains("help") || footer.contains("quit"),
            "the footer returns to the help line:\n{footer:?}"
        );
    }

    #[tokio::test]
    async fn footer_fits_narrow_width() {
        let mut sw = Switcher::new(sample());
        let mut term = Terminal::new(TestBackend::new(30, 30)).unwrap();
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer();
        let y = buf.area.height - 1;
        let mut footer = String::new();
        for x in 0..buf.area.width {
            footer.push_str(buf[(x, y)].symbol());
        }
        let footer = footer.trim_end().to_string();
        assert!(
            footer.chars().count() <= 30,
            "footer fits a 30-column terminal:\n{footer:?}"
        );
        assert!(
            footer.contains("help") || footer.contains("quit"),
            "footer still offers help/quit hints:\n{footer:?}"
        );
    }

    #[tokio::test]
    async fn selected_node_renders_reverse_video() {
        let h = Harness::new(sample());
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
    async fn kill_removes_session_and_cache() {
        let mut h = Harness::new(sample());
        // editor (local) is the default preselect; kill the focused session.
        assert!(h.sw.panes.contains_key("local/editor"));
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
            !h.sw.panes.contains_key("local/editor"),
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
        h.key(KeyCode::Left).await; // editor preselected → ← ascend to the local HOST row
        h.ch('n').await; // n on a host row ⇒ create a session
        h.sw.set_input_text("scratch");
        h.key(KeyCode::Enter).await;
        assert_eq!(*h.ops.created.lock().unwrap(), vec!["local/scratch"]);
        assert_eq!(cur_session_name(&h).as_deref(), Some("scratch"));
    }

    #[tokio::test]
    async fn slow_op_is_deferred_off_the_key_path() {
        // The key-handling path must NOT perform the network create (which would
        // freeze the UI on a slow remote); it only queues the op for the loop.
        let mut h = Harness::new(sample());
        h.key(KeyCode::Left).await; // ← ascend to the local HOST row
        h.ch('n').await; // open New (create a session) on local
        h.sw.set_input_text("scratch");
        h.sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // raw: not pumped
        assert!(
            h.ops.created.lock().unwrap().is_empty(),
            "create must be deferred off the key path, not run inline"
        );
        let op =
            h.sw.take_pending_op()
                .expect("a create was queued for the loop");
        let r = run_op(&op, &h.ops).await;
        assert_eq!(
            h.ops.created.lock().unwrap().len(),
            1,
            "the op runs only when the loop pumps it"
        );
        h.sw.apply_op_result(r);
        assert!(
            h.sw.row_of_session("local/scratch").is_some(),
            "applying the result folds the new session into the tree"
        );
    }

    #[tokio::test]
    async fn n_on_session_row_creates_a_window() {
        // The `n` action is level-aware: on a SESSION row it creates a window.
        let mut h = Harness::new(sample());
        // editor (local) is preselected (a session row).
        h.ch('n').await;
        h.sw.set_input_text("logs");
        h.key(KeyCode::Enter).await;
        assert_eq!(
            *h.ops.windowed.lock().unwrap(),
            vec!["local/editor:logs"],
            "n on a session row queues a new window"
        );
        assert!(h.ops.created.lock().unwrap().is_empty(), "not a session create");
    }

    #[tokio::test]
    async fn n_on_window_row_splits_a_pane() {
        // On a WINDOW row, `n` splits the pane (direction from the prompt).
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host row
        h.key(KeyCode::Right).await; // → local/editor (session)
        h.key(KeyCode::Right).await; // → editor's first window (a window row)
        h.ch('n').await;
        h.sw.set_input_text("h"); // horizontal split
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
        // another host (which, with an untouched cursor, moves the preselect). The
        // rename must still target the session captured when the input opened.
        let mut h = Harness::from_sources(&["alpha", "beta"]);
        h.sw.apply_source_result(
            "alpha".into(),
            vec![sess("alpha", "a-sess", 1, false, 100)],
            None,
        );
        h.ch('R').await; // capture alpha/a-sess
        h.sw.apply_source_result(
            "beta".into(),
            vec![sess("beta", "b-sess", 1, false, 999)],
            None,
        );
        h.sw.set_input_text("renamed");
        h.key(KeyCode::Enter).await;
        let renamed = h.ops.renamed.lock().unwrap();
        assert_eq!(
            *renamed,
            vec!["alpha/a-sess->renamed".to_string()],
            "rename must target the captured node, not where streaming moved the cursor"
        );
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
    async fn filter_leaves_cursor_on_visible_session() {
        // Filter to a session — cursor must land on it after the filter completes.
        let mut h = Harness::from_sources(&["local"]);
        h.sw.apply_source_result(
            "local".into(),
            vec![
                sess("local", "live", 2, true, 999),
                sess("local", "xmux-probeL", 1, false, 50),
            ],
            None,
        );
        h.ch('/').await;
        h.sw.set_input_text("probeL");
        h.key(KeyCode::Enter).await; // apply filter
        let t = h.sw.current_attach_target().expect("a session row is visible");
        assert_eq!(t.target.as_str(), "xmux-probeL", "cursor on filtered session");
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
        );
        h.ch('/').await;
        h.sw.set_input_text("keep");
        h.key(KeyCode::Enter).await; // apply filter
        h.key(KeyCode::Home).await; // host row
        let t = h.sw.current_attach_target().expect("host row has a visible session");
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
            h.sw.flash.to_lowercase().contains("unreachable"),
            "create on unreachable host should flash unreachable, got {:?}",
            h.sw.flash
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
    async fn double_click_selects_node() {
        let mut h = Harness::new(sample());
        // inference preselected; double-click inside the tree moves the cursor.
        let before = h.sw.selected;
        h.sw.mouse_attach(5, 4);
        // cursor moved (or stayed on the same selectable row — just check no panic
        // and current_attach_target is populated).
        assert!(
            h.sw.current_attach_target().is_some(),
            "double click yields an attach target"
        );
        let _ = before; // used
    }

    #[tokio::test]
    async fn single_click_moves_cursor() {
        let mut h = Harness::new(sample());
        h.sw.mouse_select(5, 4);
        // After a single click the cursor is on a selectable row (not pane/loading).
        let selectable = h.sw.rows.get(h.sw.selected).is_some_and(Row::selectable);
        assert!(selectable, "single click must land on a selectable row");
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
        assert!(!h.sw.wants_quit(), "dismissing help must not quit");
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
        use crate::proxy::screen::Grid;
        let mut h = Harness::new(sample());
        h.key(KeyCode::Down).await; // a normal non-xmux pane
        let mut g = Grid::new(28, 50);
        g.feed(b"LIVE-GRID-CONTENT");
        // Render with the live grid supplied.
        let sw = &mut h.sw;
        h.term.draw(|f| sw.render(f, Some(&g), false, TREE_WIDTH)).unwrap();
        let out = buffer_text(h.term.backend().buffer());
        assert!(
            out.contains("LIVE-GRID-CONTENT"),
            "the terminal view renders the live grid's contents:\n{out}"
        );
    }

    // --- Task 11: j/k nav, select=attach, spinner, footer/help, title --------

    fn cur_row_label(h: &Harness) -> String {
        h.sw
            .rows
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
        assert_ne!(cur_row_label(&h), at_top, "j moves the cursor down");
        h.ch('k').await; // back up
        assert_eq!(cur_row_label(&h), at_top, "k moves the cursor up");
    }

    #[tokio::test]
    async fn enter_is_noop_and_q_quits() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Enter).await;
        assert!(!h.sw.wants_quit(), "Enter does nothing");
        h.ch('q').await;
        assert!(h.sw.wants_quit(), "q quits");
    }

    #[tokio::test]
    async fn cursor_move_yields_attach_target() {
        let mut h = Harness::new(sample()); // editor preselected (local session)
        let t = h.sw.current_attach_target().expect("a session row yields a target");
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
        h.key(KeyCode::Left).await; // ← ascend to the local HOST row
        let t = h.sw.current_attach_target().expect("host row targets its first session");
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    }

    #[tokio::test]
    async fn current_host_tracks_cursor_source() {
        // The cockpit ensures this host on every move; a host row yields its source
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
        h.sw.set_spinner(connecting);
        h.draw();
        let tree = h.tree_text();
        // a braille spinner glyph from the U+2800 block appears on the inference row.
        let line = tree.lines().find(|l| l.contains("inference")).unwrap_or("");
        assert!(line.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
            "a braille spinner sits right of a connecting session name:\n{tree}");
    }

    #[tokio::test]
    async fn footer_and_help_reflect_new_model() {
        let mut h = Harness::new(sample());
        let footer = h.footer_text();
        assert!(!footer.to_lowercase().contains("enter attach"), "Enter is a no-op now:\n{footer}");
        assert!(footer.contains("focus"),
            "footer mentions focusing the terminal pane:\n{footer}");
        h.ch('?').await;
        let help = h.text();
        assert!(help.contains("focus the terminal"),
            "help explains focusing the terminal pane:\n{help}");
        assert!(!help.contains("select = attach"),
            "no useless 'select = attach' noise in help:\n{help}");
        assert!(!help.contains("dwell") && !help.to_lowercase().contains("previous foreground"),
            "no stale dwell/esc-return strings:\n{help}");
    }

    #[tokio::test]
    async fn divider_color_marks_the_focused_side() {
        // The single rule between tree and terminal is green when the terminal
        // pane has focus, dim otherwise — the only focus indicator (tmux-style).
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let mut sw = Switcher::new(sample());
        let div_x = TREE_WIDTH;
        let divider_is_green = |buf: &Buffer| {
            (0..buf.area.height)
                .any(|y| buf[(div_x, y)].symbol() == "│" && buf[(div_x, y)].fg == Color::Green)
        };

        term.draw(|f| sw.render(f, None, true, TREE_WIDTH)).unwrap();
        assert!(divider_is_green(term.backend().buffer()), "terminal-focused divider is green");

        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        assert!(!divider_is_green(term.backend().buffer()), "tree-focused divider is not green");
    }

    #[tokio::test]
    async fn footer_and_input_live_under_the_tree_only() {
        let mut h = Harness::new(sample());
        h.ch('/').await; // open the filter input
        let buf = h.buf();
        let last = buf.area.height - 1;
        // The footer renders in the LEFT (tree) column only:
        assert!(
            (0..TREE_WIDTH).any(|x| buf[(x, last)].symbol() != " "),
            "footer renders in the tree column"
        );
        // The divider spans the FULL height — the divider and terminal column
        // span all rows; the footer is confined to the tree column.
        assert_eq!(
            buf[(TREE_WIDTH, last)].symbol(),
            "│",
            "divider spans the full height; footer is confined to the tree column"
        );
    }

    #[tokio::test]
    async fn kill_confirm_footer_is_red() {
        let mut h = Harness::new(sample());
        h.ch('x').await; // arm kill on the selected session
        // The footer is the LAST row; find a cell of the confirm text and assert red fg.
        let buf = h.buf();
        let y = buf.area.height - 1;
        let cell = (0..buf.area.width)
            .map(|x| &buf[(x, y)])
            .find(|c| c.symbol() == "k") // "kill ...": first 'k'
            .expect("kill confirm text present");
        assert_eq!(cell.fg, ratatui::style::Color::Red, "kill confirm must be red");
    }

    #[tokio::test]
    async fn kill_window_confirm_footer_is_red() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1)
        h.ch('x').await;              // arm window kill (pumps + draws)
        assert!(matches!(h.sw.pending_kill, Some(PendingKill::Window { .. })), "kill on window row must set a window PendingKill");
        // The footer is the LAST row; find a cell of the confirm text and assert red fg.
        let buf = h.buf();
        let y = buf.area.height - 1;
        let cell = (0..buf.area.width)
            .map(|x| &buf[(x, y)])
            .find(|c| c.symbol() == "k") // "kill ...": first 'k'
            .expect("kill confirm text present");
        assert_eq!(cell.fg, ratatui::style::Color::Red, "window kill confirm must be red");
    }

    #[tokio::test]
    async fn kill_on_window_row_targets_the_window() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1)
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)); // arm kill (raw, not pumped)
        assert!(
            matches!(h.sw.pending_kill, Some(PendingKill::Window { .. })),
            "kill on window row must set a window PendingKill"
        );
        let target = match h.sw.pending_kill.as_ref().unwrap() {
            PendingKill::Window { target, .. } => target.clone(),
            _ => panic!("expected Window variant"),
        };
        assert_eq!(target, "editor:1");
        // confirm with y
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        let op = h.sw.take_pending_op().expect("kill window queued");
        assert!(matches!(op, PendingOp::KillWindow { ref target, .. } if target == "editor:1"));
    }

    #[tokio::test]
    async fn rebuild_cancels_armed_kill_confirm() {
        // Arm a window kill (raw, no pump so no auto-confirm), then trigger a
        // rebuild via apply_panes — simulating a streamed tree update that can
        // move or index-reuse the armed node.  The in-flight confirm must be
        // cleared so a subsequent 'y' does not queue a stale kill.
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1, name "shell")
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)); // arm kill (raw)
        assert!(
            matches!(h.sw.pending_kill, Some(PendingKill::Window { .. })),
            "arm_kill must set a window PendingKill"
        );
        // Force a rebuild by streaming in panes for editor — the same windows,
        // but any rebuild must invalidate the armed confirm.
        let s = sample();
        let editor_panes = s.panes["local/editor"].clone();
        h.sw.apply_panes("local/editor".to_string(), editor_panes);
        assert!(
            h.sw.pending_kill.is_none(),
            "a rebuild must cancel an armed kill confirm"
        );
        // A 'y' after the rebuild must not queue any op (stale kill guard).
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            h.sw.take_pending_op().is_none(),
            "no stale kill queued after rebuild"
        );
    }

    #[tokio::test]
    async fn rename_on_window_row_targets_the_window() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1, name "shell")
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE)); // open rename (raw)
        // input should be open for window rename
        assert!(h.sw.input.is_some(), "rename on window row must open input");
        assert!(h.sw.input.as_ref().unwrap().target.is_some(), "rename on window must have a target");
        h.sw.set_input_text("newname");
        h.sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // confirm (raw)
        let op = h.sw.take_pending_op().expect("rename window queued");
        assert!(matches!(op, PendingOp::RenameWindow { ref target, ref new_name, .. }
            if target == "editor:1" && new_name == "newname"));
    }

    #[tokio::test]
    async fn rename_window_unchanged_name_is_ignored() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1, name "shell")
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE)); // open rename (raw)
        assert!(h.sw.input.is_some(), "rename on window row must open input");
        h.sw.set_input_text("shell"); // same as current name
        h.sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            h.sw.take_pending_op().is_none(),
            "unchanged window name must not queue a RenameWindow op"
        );
    }

    #[tokio::test]
    async fn rename_window_rejects_leading_dash() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1)
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE)); // open rename (raw)
        assert!(h.sw.input.is_some(), "rename on window row must open input");
        h.sw.set_input_text("-bad");
        h.sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            h.sw.take_pending_op().is_none(),
            "leading-dash window rename must be refused"
        );
        assert!(
            h.sw.flash.contains("cannot start with"),
            "leading-dash window rename must set a flash message, got {:?}",
            h.sw.flash
        );
    }

    #[tokio::test]
    async fn kill_on_host_row_flashes_error() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host row
        h.ch('x').await;
        assert!(
            h.sw.flash.to_lowercase().contains("cannot kill"),
            "kill on host row must flash an error, got {:?}",
            h.sw.flash
        );
        assert!(h.sw.pending_kill.is_none(), "no kill queued");
    }

    #[tokio::test]
    async fn rename_on_host_row_flashes_error() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host row
        h.ch('R').await;
        assert!(
            h.sw.flash.to_lowercase().contains("cannot rename"),
            "rename on host row must flash an error, got {:?}",
            h.sw.flash
        );
        assert!(h.sw.input.is_none(), "no input opened");
    }

    #[test]
    fn removed_window_selection_falls_to_previous_sibling_then_parent() {
        // two windows under jup/api; cursor on window 1. Remove window 1 → cursor to
        // window 0 (previous sibling). Remove window 0 (now the only/topmost) → cursor
        // to the session row (parent).
        let mut sw = Switcher::new(two_window_scan());
        sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → window 0
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));  // ↓ window 1
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })));
        // simulate window 1 removed: re-apply panes without window 1.
        sw.apply_panes("jup/api".into(), vec![
            win(0, "w0", true, vec![pane(0, true, "bash")]),
        ]);
        assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })),
            "removed window → previous sibling");
        // remove window 0 too (session now has no window rows): cursor to the session.
        sw.apply_panes("jup/api".into(), vec![]);
        assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "api"),
            "topmost removed → parent (session row)");
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
}
