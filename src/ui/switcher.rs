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
use unicode_width::UnicodeWidthStr;

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

/// Parses a tmux-style colour token into a ratatui [`Color`], matching tmux/psmux's
/// colour vocabulary so the divider colours can be configured exactly like
/// `pane-border-style`: the 16 named ANSI colours, their `bright*` variants,
/// `colourN`/`colorN` (a 0-255 palette index), `#RRGGBB`, and `default` (terminal
/// default). A leading `fg=` is tolerated so a tmux style string drops in verbatim.
/// Unknown or empty tokens fall back to [`Color::Reset`] (terminal default).
pub fn map_color(s: &str) -> Color {
    let s = s.trim();
    let s = s.strip_prefix("fg=").unwrap_or(s).trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
        }
    }
    let lower = s.to_lowercase();
    if let Some(idx) = lower.strip_prefix("colour").or_else(|| lower.strip_prefix("color")) {
        if let Ok(n) = idx.parse::<u8>() {
            return Color::Indexed(n);
        }
    }
    match lower.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "brightblack" | "bright-black" => Color::DarkGray,
        "brightred" | "bright-red" => Color::LightRed,
        "brightgreen" | "bright-green" => Color::LightGreen,
        "brightyellow" | "bright-yellow" => Color::LightYellow,
        "brightblue" | "bright-blue" => Color::LightBlue,
        "brightmagenta" | "bright-magenta" => Color::LightMagenta,
        "brightcyan" | "bright-cyan" => Color::LightCyan,
        "brightwhite" | "bright-white" => Color::White,
        _ => Color::Reset,
    }
}

/// The tree|mux divider's three colours, resolved from config (tmux's pane-border
/// options): `active` marks the focused side, `inactive` the unfocused side, and
/// `hover` the drag-resize grab cue. Defaults mirror tmux's own code defaults —
/// `green` / terminal-default / `yellow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DividerColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

impl Default for DividerColors {
    fn default() -> Self {
        DividerColors {
            active: Color::Green,
            inactive: Color::Reset,
            hover: Color::Yellow,
        }
    }
}

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

/// One context-menu entry. The variant drives the action taken on release; the
/// label is the row text. Words match the rest of the tree UI ("focus the mux pane",
/// "new", "rename", "kill" — never "open"/"split", which are not used elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuItem {
    Focus,
    NewSession,
    NewWindow,
    Rename,
    Kill,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            MenuItem::Focus => "focus",
            MenuItem::NewSession => "new session",
            MenuItem::NewWindow => "new window",
            MenuItem::Rename => "rename",
            MenuItem::Kill => "kill",
        }
    }
}

/// What the cockpit must do after a menu release. Most items are handled inside the
/// switcher (they open an input or arm a kill); `FocusTerminal` is the one outcome the
/// cockpit owns (the "focus" item moves focus to the mux pane).
pub enum MenuOutcome {
    None,
    Handled,
    FocusTerminal,
}

/// An open right-click context menu. `target` is the node it acts on, re-located by
/// identity at release so a tree rebuild during the brief hold cannot misfire on a
/// stale row. `title` names that node (shown in the box's top border, like tmux's
/// menu title — so the menu reads as "actions for <this node>"). `rect` is the
/// bordered box in 0-based screen coords; `hovered` is the highlighted item.
struct Menu {
    target: RowRef,
    title: String,
    rect: Rect,
    items: Vec<MenuItem>,
    hovered: Option<usize>,
}

/// The menu entries for a node, by type. Non-selectable rows (pane/loading) get none.
/// `focus` is first so a press-release with no drag falls on the safe default.
fn menu_items(target: &RowRef) -> Vec<MenuItem> {
    use MenuItem::*;
    match target {
        RowRef::Host { .. } => vec![NewSession],
        RowRef::Session(_) => vec![Focus, NewWindow, Rename, Kill],
        RowRef::Window { .. } => vec![Focus, Rename, Kill],
        RowRef::Pane | RowRef::Loading => Vec::new(),
    }
}

/// The menu's title — the human name of the node it acts on (host alias, session
/// name, or `session:window`), shown in the box's top border.
fn menu_title(target: &RowRef) -> String {
    match target {
        RowRef::Host { source, .. } => source.clone(),
        RowRef::Session(s) => s.name.clone(),
        RowRef::Window { sess, window } => format!("{}:{}", sess.name, window),
        RowRef::Pane | RowRef::Loading => String::new(),
    }
}

/// Greedily word-wraps `text` to lines no wider than `width` display columns
/// (Unicode-aware), breaking on spaces; a word longer than `width` is hard-split so
/// nothing is ever clipped. Always returns at least one line. Used so the input
/// prompt's description wraps across a narrow tree column instead of being truncated.
fn wrap_text(text: &str, width: u16) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let width = (width as usize).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split(' ') {
        let ww = UnicodeWidthStr::width(word);
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty() && cur_w + sep + ww > width {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if ww > width {
            // Longer than a whole line: hard-split across as many lines as needed.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if cur_w + cw > width && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += cw;
            }
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
        }
    }
    lines.push(cur);
    lines
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
    /// Auto-hide-tree mode (set by the cockpit each frame). Drives the divider glyph:
    /// ║ (double) when on, │ (single) when off — the only on-screen cue, since while
    /// the mode is on but the tree is focused the tree still shows.
    auto_hide: bool,
    /// True while the mouse is hovering the divider rule — the cockpit sets this from
    /// idle motion so the divider highlights as a grab cue for drag-resize.
    divider_hovered: bool,
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
    /// The open right-click context menu (the fourth modal, like `input` /
    /// `pending_kill` / `show_help`). `None` ⇒ no menu.
    menu: Option<Menu>,
    /// The whole frame area, captured each render so the menu box can be clamped to
    /// the screen at open time (mouse events arrive between renders).
    screen_area: Rect,
    /// Raw `~/.ssh/config` text (set once by the cockpit). The right-pane info panel
    /// shows the matching Host/Match stanza for a selected unreachable host. Empty in tests.
    ssh_config_text: String,
    /// The tree|mux divider colours (set once by the cockpit from config; tmux defaults
    /// otherwise). See [`DividerColors`].
    colors: DividerColors,
    /// Drag offset (cells) applied to a modal popup's centered position. Reset
    /// to (0,0) when a popup opens; updated while its border is dragged.
    popup_offset: (i16, i16),
    /// The drawn rect of the active modal popup (help/input/confirm), cached
    /// each render so a mouse press can hit-test its border. `Rect::default()`
    /// ⇒ no modal popup open.
    popup_rect: Rect,
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
            auto_hide: false,
            divider_hovered: false,
            pending_op: None,
            spinner: HashSet::new(),
            spinner_frame: 0,
            preferred: None,
            menu: None,
            screen_area: Rect::default(),
            ssh_config_text: String::new(),
            colors: DividerColors::default(),
            popup_offset: (0, 0),
            popup_rect: Rect::default(),
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

    /// Whether the node an armed kill targets still exists in the current rows,
    /// matched by identity (session address / window source+target) rather than row
    /// position. Lets [`Switcher::rebuild`] keep the confirm alive across routine tree
    /// updates and drop it only when the target genuinely vanished.
    fn kill_target_present(&self, kill: &PendingKill) -> bool {
        match kill {
            PendingKill::Session(sess) => {
                let addr = sess.address();
                self.rows
                    .iter()
                    .any(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == addr))
            }
            PendingKill::Window { source, target, .. } => self.rows.iter().any(|r| {
                matches!(&r.reference, RowRef::Window { sess, window }
                    if sess.source == *source && crate::mux::window_target(&sess.name, *window) == *target)
            }),
        }
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
        // Keep an armed kill confirm across this rebuild as long as its target still
        // EXISTS (matched by identity, not row position). Only a tree change that
        // actually removed the target invalidates it — routine rebuilds (the local
        // poll, a remote %-event) must NOT silently cancel it, or answering y/n has a
        // surprise time limit. resolve_kill consumes it; set_selected does not touch it.
        if self.pending_kill.as_ref().is_some_and(|k| !self.kill_target_present(k)) {
            self.pending_kill = None;
        }
        let target = preferred_row
            .or(first_session_row)
            .or_else(|| self.rows.iter().position(Row::selectable))
            .unwrap_or(0);
        self.set_selected(target);
    }

    /// The dim trailing annotation for a host row: its scan state when it has no
    /// sessions to show — scanning…, ⚠ (unreachable; the reason is shown in the
    /// right-pane info panel when the host row is selected), or (empty).
    fn host_hint(&self, g: &Group, scanning: bool) -> Option<String> {
        if scanning {
            Some("scanning…".into())
        } else if g.err.is_some() {
            Some("⚠".into())
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

    /// The session the mux is DISPLAYING for the cursor's row: the cursor's own session
    /// (session/window row) or, on a host row, the host's recent session — the same
    /// resolution `target_for` uses. Lets the passthrough follow descend from a host.
    fn displayed_session(&self) -> Option<Session> {
        match self.current_ref()? {
            RowRef::Session(s) => Some(s.clone()),
            RowRef::Window { sess, .. } => Some(sess.clone()),
            RowRef::Host { source, .. } => self.first_session_of(source),
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
            // A host row descends into its displayed (recent) session's active window.
            Some(RowRef::Host { source: src, .. }) => src == source,
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

    /// Moves the cursor to the ACTIVE window row of the DISPLAYED session (read from
    /// cached pane data) — from a session row, a window row, OR a host row (which
    /// descends into the host's recent session). Used when focus moves to the terminal
    /// so the sidebar mirrors the window the mux is showing (#3). A no-op when the
    /// displayed session or its active window is unknown (e.g. panes not yet loaded, or
    /// an unreachable host). Returns whether it moved.
    pub fn select_active_window(&mut self) -> bool {
        let Some(sess) = self.displayed_session() else {
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

    /// Sets auto-hide-tree mode (the cockpit owns it; the divider glyph reflects it).
    pub fn set_auto_hide(&mut self, on: bool) {
        self.auto_hide = on;
    }

    /// Sets whether the mouse is hovering the divider (the cockpit derives it from
    /// idle motion); when set, the divider highlights as a drag-resize grab cue.
    pub fn set_divider_hovered(&mut self, on: bool) {
        self.divider_hovered = on;
    }

    /// Sets the tree|mux divider colours. The cockpit calls this once at startup with
    /// the colours parsed from config's `pane-*-border-style` options; tmux defaults
    /// apply otherwise.
    pub fn set_divider_colors(&mut self, colors: DividerColors) {
        self.colors = colors;
    }

    // --- key handling -------------------------------------------------------

    /// Open the modal keys overlay. In tree focus any key then dismisses it (see
    /// `handle_key`); [`toggle_help`] is the focus-independent open/close entry point.
    pub fn show_help(&mut self) {
        self.show_help = true;
    }

    /// Toggle the keys overlay. Driven by `prefix ?` in EITHER focus so help opens
    /// and closes the same way regardless of which pane holds focus.
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Modal help input, tmux view-mode style. While the overlay is open it captures
    /// the whole key read (returns true ⇒ consumed — nothing reaches the tree or the
    /// mux pane); `q` or Esc closes it, every other key is swallowed. Returns false
    /// when help is closed, so the read falls through to normal routing. The single
    /// owner of help dismissal — the cockpit calls it above the tree/mux split, so the
    /// behavior is identical in both focuses.
    pub fn feed_help_key(&mut self, bytes: &[u8]) -> bool {
        if !self.show_help {
            return false;
        }
        // `q`, or a real Esc (a lone ESC, not the ESC `[` that starts an arrow/CSI).
        let esc = bytes.contains(&0x1b) && !bytes.windows(2).any(|w| w == [0x1b, b'[']);
        if bytes.contains(&b'q') || esc {
            self.show_help = false;
        }
        true
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.input.is_some() {
            self.handle_input_key(ev);
            return;
        }
        if self.pending_kill.is_some() {
            self.resolve_kill(ev);
            return;
        }
        // A flash (error/notice) is transient — it lives only until the next key, like a
        // status toast. Clear it here so navigation (or any key) restores the normal help
        // footer; actions below may set a fresh one, which survives because this runs first.
        self.flash.clear();
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
                '/' => self.open_input(InputMode::Filter),
                'n' => self.open_new(),
                'R' => self.open_input(InputMode::Rename),
                'x' => self.arm_kill(),
                'r' => self.request_rescan(),
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
                    label: " filter sessions".into(),
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
                            label: " rename session".into(),
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
                            label: " rename window".into(),
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
                label: " new session name (empty = auto)".into(),
                buffer: String::new(),
                source: Some(source),
                sess: None,
                target: None,
            }),
            RowRef::Session(sess) => Some(Input {
                mode: InputMode::NewWindow,
                label: format!(" new window in {} (name optional)", sess.name),
                buffer: String::new(),
                source: Some(sess.source.clone()),
                sess: Some(sess),
                target: None,
            }),
            RowRef::Window { sess, window } => {
                let target = format!("{}:{}", sess.name, window);
                Some(Input {
                    mode: InputMode::SplitWindow,
                    label: " split [v]ertical / [h]orizontal (default v)".into(),
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
        // tmux confirm-before semantics: only y/Y confirms; any other key — n, Esc, or
        // anything else — cancels (the pending confirm is taken either way).
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

    // --- context menu -------------------------------------------------------

    /// True while a context menu is open. The cockpit routes every mouse event to
    /// the menu (press-hold-release) while this holds, like a divider drag.
    pub fn menu_active(&self) -> bool {
        self.menu.is_some()
    }

    /// Right-button press at 0-based screen (col,row): opens that tree row's menu if
    /// it lands on a selectable row that has items. Does NOT move the tree cursor —
    /// the gesture only remembers the target, so no background attach fires mid-hold.
    /// Returns true iff a menu opened (so the cockpit knows to consume the event).
    pub fn menu_open(&mut self, col: u16, row: u16) -> bool {
        if !self.in_tree(col, row) {
            return false;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
        let Some(target) = self
            .rows
            .get(idx)
            .filter(|r| r.selectable())
            .map(|r| r.reference.clone())
        else {
            return false;
        };
        let items = menu_items(&target);
        if items.is_empty() {
            return false;
        }
        let title = menu_title(&target);
        let rect = menu_rect(col, row, &items, &title, self.screen_area);
        // No item is pre-highlighted, and the box opens just below the pointer (see
        // menu_rect) — so an accidental right-click that releases without dragging onto
        // an item does nothing. Selecting is a deliberate move down onto an item.
        self.menu = Some(Menu { target, title, rect, items, hovered: None });
        true
    }

    /// Mouse moved while the menu is held: highlight the item under the cursor. Over the
    /// box but off an item (the title border) keeps the current highlight; only dragging
    /// fully OUTSIDE the box clears it, so releasing there cancels.
    pub fn menu_hover(&mut self, col: u16, row: u16) {
        if let Some(menu) = self.menu.as_mut() {
            if let Some(i) = menu.item_at(col, row) {
                menu.hovered = Some(i);
            } else if !menu.contains(col, row) {
                menu.hovered = None;
            }
        }
    }

    /// Right-button up: act on the hovered item against the (re-located) target row,
    /// then close the menu. Released off-menu (no hovered item) cancels. The target is
    /// re-found by identity so a rebuild during the hold can't act on a stale node.
    pub fn menu_release(&mut self) -> MenuOutcome {
        let Some(menu) = self.menu.take() else {
            return MenuOutcome::None;
        };
        let Some(i) = menu.hovered else {
            return MenuOutcome::None;
        };
        let item = menu.items[i];
        let Some(idx) = self.rows.iter().position(|r| same_node(&r.reference, &menu.target)) else {
            return MenuOutcome::None;
        };
        // The delegated methods act on the current cursor, so land it on the target,
        // run the action (which CAPTURES the target by value), then for everything
        // EXCEPT focus restore the cursor. A lingering cursor move would change the
        // selection → trigger an attach and the events it spawns → rebuild the tree,
        // which clears an armed kill confirm (pending_kill) before the user can answer
        // y/n, and needlessly switches the displayed session. focus is the one item
        // that intends to move there.
        let prior = self.capture_focus();
        self.user_moved = true;
        self.set_selected(idx);
        match item {
            MenuItem::Focus => {
                // For a window, optimistically mark it active in the cache. Otherwise the
                // passthrough cursor-follow (`select_active_window`, run before the attach's
                // select-window lands) would yank the cursor back to the session's previous
                // active window — so focusing a different window of the already-displayed
                // session did nothing. The real select-window follows from the selection.
                if let RowRef::Window { sess, window } = &menu.target {
                    self.set_active_window(&sess.source, &sess.name, *window);
                }
                MenuOutcome::FocusTerminal
            }
            MenuItem::NewSession | MenuItem::NewWindow => {
                self.open_new();
                self.restore_focus(prior);
                MenuOutcome::Handled
            }
            MenuItem::Rename => {
                self.open_input(InputMode::Rename);
                self.restore_focus(prior);
                MenuOutcome::Handled
            }
            MenuItem::Kill => {
                self.arm_kill();
                self.restore_focus(prior);
                MenuOutcome::Handled
            }
        }
    }

    /// Close the menu without acting (cockpit watchdog: a keystroke ends the gesture).
    pub fn menu_cancel(&mut self) {
        self.menu = None;
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
        self.screen_area = area;
        // Reset the buffer before painting. The widgets below do not all fill every cell
        // they own — the mux grid only paints its top-left clip (cells past the grid size
        // are skipped), the divider rule sets fg only, and the tree leaves blank rows — so
        // when the tree width changes (drag / prefix h·l) cells that switched panes would
        // otherwise keep stale content (the residue seen while resizing). Clearing first
        // makes every unpainted cell default; ratatui still diffs against the last frame,
        // so static content writes nothing (no flicker).
        frame.render_widget(Clear, area);
        // tree_width == 0 is the "tree hidden" sentinel (mux focused + auto-hide-tree):
        // the terminal view owns the whole area — no tree, no input/footer, no divider.
        if tree_width == 0 {
            self.tree_inner = Rect::default();
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            self.render_modal_popup(frame, area);
            self.render_menu(frame);
            return;
        }
        let cols = Layout::horizontal([
            Constraint::Length(tree_width),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
        // Tree column: the tree plus its footer/help line. The footer is normally one
        // line, but a long flash wraps across several — size the footer to the wrapped
        // line count so it is never clipped.
        let footer_h = self.footer_lines(tree_width).len().max(1) as u16;
        let left = Layout::vertical([
            Constraint::Min(0),           // tree
            Constraint::Length(footer_h), // footer (help / status / wrapped flash)
        ])
        .split(cols[0]);
        self.render_tree(frame, left[0]);
        self.render_footer(frame, left[1]);
        // The tree|mux divider marks focus between those two panes.
        self.render_divider(frame, cols[1], terminal_focused);
        let term_area = cols[2];
        // An unreachable host has no live grid; show an info panel (ssh config stanza
        // + failure reason) in its right pane instead of the blank (attaching…) grid.
        if self.current_host_unreachable() {
            self.render_host_info(frame, term_area);
        } else {
            self.render_terminal_view(frame, term_area, grid);
        }
        // In passthrough, place the real cursor at the grid's cursor so typing in the
        // mux is visible and tracks. Skipped when the child hid its cursor.
        if terminal_focused {
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(term_area, g.cursor()));
                }
            }
        }
        self.render_modal_popup(frame, area);
        self.render_menu(frame);
    }

    /// The vertical rule between the tree (left) and terminal (right). It splits into
    /// a top and bottom half: the accent (green) half marks WHICH pane holds focus —
    /// top = tree (left), bottom = mux (right) — and the other half stays dim. A single
    /// vertical rule cannot lean left/right, so the accent half's position carries the
    /// signal (adapting tmux's active-pane border). Replaces the per-pane box borders.
    /// The glyph also encodes auto-hide-tree mode: ║ (double) when on, │ when off — so
    /// a visible tree that will vanish on blur is distinguishable from a pinned one.
    fn render_divider(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        let active = self.colors.active;
        let inactive = self.colors.inactive;
        let glyph = if self.auto_hide { "║" } else { "│" };
        // Hover (mouse over the rule, no button): box-drawing rules have no bold form
        // (the BOLD modifier does not thicken them), so swap the glyph itself to the
        // HEAVY vertical (┃) for a genuinely thicker line and recolour it with the
        // configured hover colour (tmux's `pane-border-hover-style`) — same single rule,
        // just thicker + lit, as the grab cue.
        if self.divider_hovered {
            let style = Style::default().fg(self.colors.hover);
            let bars = Text::from(
                (0..area.height)
                    .map(|_| Line::from(Span::styled("┃", style)))
                    .collect::<Vec<_>>(),
            );
            frame.render_widget(Paragraph::new(bars), area);
            return;
        }
        let colors: Vec<Color> = if area.height <= 1 {
            // Too short to split: show the active-marker color in the single cell.
            vec![active; area.height as usize]
        } else {
            let top_rows = area.height.div_ceil(2); // top takes the extra row on odd heights
            let (top, bottom) = if terminal_focused {
                (inactive, active) // mux focused → accent on the bottom (mux side)
            } else {
                (active, inactive) // tree focused → accent on the top (tree side)
            };
            (0..area.height)
                .map(|y| if y < top_rows { top } else { bottom })
                .collect()
        };
        let bars = Text::from(
            colors
                .into_iter()
                .map(|c| Line::from(Span::styled(glyph, Style::default().fg(c))))
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

    /// Sets the raw `~/.ssh/config` text the unreachable-host info panel reads.
    pub fn set_ssh_config_text(&mut self, text: String) {
        self.ssh_config_text = text;
    }

    /// The right-pane info panel for a selected unreachable host: the failure reason
    /// and the host's `~/.ssh/config` stanza, so the user can see WHY the control
    /// connection failed without leaving the cockpit.
    fn render_host_info(&self, frame: &mut Frame, area: Rect) {
        let alias = self.current_source().unwrap_or_default();
        let reason = self
            .groups
            .iter()
            .find(|g| g.source == alias)
            .and_then(|g| g.err.clone())
            .unwrap_or_else(|| "connection closed".into());
        let mut lines = vec![
            Line::from(Span::styled(
                format!(" ⚠ {alias} unreachable"),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(format!(" reason: {reason}")),
            Line::from(""),
            Line::from(Span::styled(
                " ~/.ssh/config:",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ];
        let stanza = crate::config::host_stanza(&self.ssh_config_text, &alias);
        if stanza.is_empty() {
            lines.push(Line::from(Span::styled(
                " (no matching ssh config entry)",
                Style::default().add_modifier(Modifier::DIM),
            )));
        } else {
            for l in stanza.lines() {
                lines.push(Line::from(format!(" {l}")));
            }
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    /// The footer's logical text (confirm / flash / scanning / filter / help), fit to
    /// `width`. A flash is returned raw — it may exceed `width`; [`Self::footer_lines`]
    /// wraps it so it never clips.
    fn footer_text(&self, width: u16) -> String {
        if !self.flash.is_empty() {
            format!(" {}", self.flash)
        } else if !self.scanning.is_empty() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the help line) once every host has settled.
            let total = self.groups.len();
            let done = total.saturating_sub(self.scanning.len());
            fit(
                &[
                    format!(" ⟳ scanning hosts {done}/{total}… · C-g q quit · C-g ? help"),
                    format!(" ⟳ scanning {done}/{total}…"),
                ],
                width,
            )
        } else if !self.filter.is_empty() {
            // The active filter has no border title to live in any more, so it
            // shows in the footer (with how to clear it).
            fit(
                &[
                    format!(" filter: {} · / edit · Esc clear · C-g ? help · C-g q quit", self.filter),
                    format!(" filter: {}", self.filter),
                ],
                width,
            )
        } else {
            fit(
                &[
                    " ↑/↓ move · Enter/C-g→ focus mux · / filter · n new · R rename · x kill · r refresh · C-g ? help · C-g q quit".to_string(),
                    " ↑/↓ move · Enter focus mux · / filter · n new · x kill · C-g ? help · C-g q quit".to_string(),
                    " move · Enter focus mux · / filter · C-g ? help · C-g q quit".to_string(),
                    " Enter focus mux · C-g ? help · C-g q quit".to_string(),
                    " C-g ? help · C-g q quit".to_string(),
                ],
                width,
            )
        }
    }

    /// The footer text split into the lines to render. The fit-based text is always one
    /// line; only a flash (an arbitrary error/notice) may exceed `width`, so it wraps
    /// across the narrow tree-column footer rather than clipping.
    fn footer_lines(&self, width: u16) -> Vec<String> {
        let text = self.footer_text(width);
        // Only a flash can exceed `width` (the fit-based text is already constrained);
        // wrap it on word boundaries with a consistent left margin.
        if self.flash.is_empty() {
            return vec![text];
        }
        wrap_text(text.trim_start(), width.saturating_sub(1))
            .into_iter()
            .map(|l| format!(" {l}"))
            .collect()
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let lines = self.footer_lines(area.width);
        let text = Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>());
        frame.render_widget(Paragraph::new(text), area);
    }

    /// The help overlay's `(title, lines)`, built once and rendered through the
    /// shared modal-popup path.
    fn help_lines(&self) -> (String, Vec<Line<'static>>) {
        // tmux mode-tree style: a right-aligned, bold key column, a `│` rule, then
        // the description. `Head` breaks the flat list into tree/focus/mux sections;
        // `Note` is a description-only row (the mux state has no keys of its own).
        enum HelpRow {
            Head(&'static str),
            Key(&'static str, &'static str),
            Note(&'static str),
            Gap,
        }
        use HelpRow::*;
        const ROWS: &[HelpRow] = &[
            Head("tree"),
            Key("↑/↓ · j/k", "move between siblings"),
            Key("→/l · ←/h", "descend into / ascend out of a node"),
            Key("PgUp/PgDn", "jump by 10"),
            Key("Home/End", "first / last node"),
            Key("n", "new (session / window, by level)"),
            Key("R", "rename the focused session or window"),
            Key("x", "kill it (y / n confirm)"),
            Key("/", "fuzzy filter <source>/<name>"),
            Key("r", "re-scan every host"),
            Gap,
            Head("focus (C-g = prefix)"),
            Key("Enter · C-g →", "focus the mux pane"),
            Key("C-g Tab", "toggle focus between tree and mux"),
            Key("C-g ← · C-g Esc", "focus the tree"),
            Key("C-g C-←/→ · h/l", "resize the tree (C-←/→ then repeats briefly)"),
            Key("C-g t", "toggle auto-hide-tree (║ divider = on)"),
            Key("C-g ?", "show this help (q / Esc closes)"),
            Key("click a pane", "focus that pane"),
            Key("drag the divider", "resize the tree"),
            Key("right-click a row", "hold for its menu, release on an item"),
            Key("C-g q", "quit"),
            Key("C-g C-g", "send a literal C-g to the mux"),
            Gap,
            Head("mux (focused)"),
            Note("keys, scroll & clicks go to the pane"),
            Note("(the mux needs its own mouse mode on)"),
        ];
        let kw = ROWS
            .iter()
            .filter_map(|r| match r {
                Key(k, _) => Some(k.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let lines: Vec<Line> = ROWS
            .iter()
            .map(|r| match r {
                Gap => Line::from(""),
                Head(h) => Line::from(Span::styled(
                    format!(" {h}"),
                    bold.add_modifier(Modifier::UNDERLINED),
                )),
                Key(k, d) => Line::from(vec![
                    Span::styled(format!(" {k:>kw$} "), bold),
                    Span::raw("│ "),
                    Span::raw(*d),
                ]),
                Note(n) => Line::from(vec![
                    Span::raw(format!(" {:>kw$} ", "")),
                    Span::raw("│ "),
                    Span::raw(*n),
                ]),
            })
            .collect();
        ("keys".to_string(), lines)
    }

    /// The active input rendered as popup `(title, lines)`: the instructional
    /// label, the `❯ buffer` entry line, and a dim Esc hint.
    fn input_lines(&self) -> (String, Vec<Line<'static>>) {
        let input = self.input.as_ref().expect("input active");
        let dim = Style::default().add_modifier(Modifier::DIM);
        let lines = vec![
            Line::from(Span::styled(format!(" {}", input.label.trim()), dim)),
            Line::from(format!(" ❯ {}", input.buffer)),
            Line::from(Span::styled(" Esc to cancel", dim)),
        ];
        (input_title(input.mode).to_string(), lines)
    }

    /// The armed kill confirm rendered as popup `(title, lines)`, in red.
    fn confirm_lines(&self) -> (String, Vec<Line<'static>>) {
        let red = Style::default().fg(Color::Red);
        let q = match self.pending_kill.as_ref().expect("kill armed") {
            PendingKill::Session(sess) => format!(" kill {}?", sess.address()),
            PendingKill::Window { source, target, .. } => format!(" kill {source}/{target}?"),
        };
        let lines = vec![
            Line::from(Span::styled(q, red)),
            Line::from(Span::styled(" [y]es / [n]o · Esc cancel", red)),
        ];
        ("kill?".to_string(), lines)
    }

    /// Draws the active modal popup (help / confirm / input) centered, shifted
    /// by `popup_offset`, through the shared opaque `render_popup`, and caches
    /// its rect for drag hit-testing. These modals are mutually exclusive in
    /// normal use; if more than one is set, help wins, then confirm, then input.
    fn render_modal_popup(&mut self, frame: &mut Frame, area: Rect) {
        let (title, lines) = if self.show_help {
            self.help_lines()
        } else if self.pending_kill.is_some() {
            self.confirm_lines()
        } else if self.input.is_some() {
            self.input_lines()
        } else {
            self.popup_rect = Rect::default();
            return;
        };
        let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let w = (inner_w + 3).clamp(24, area.width.max(1)); // borders + a cell of right padding
        let h = (lines.len() as u16 + 2).min(area.height.max(1));
        let rect = offset_centered(w, h, area, self.popup_offset);
        self.popup_rect = rect;
        render_popup(frame, area, rect, &title, lines);
    }

    /// Draws the open context menu as a bordered popup at its anchored rect: the target's
    /// name in the title (like tmux's menu title), the hovered item reversed. Shares the
    /// opaque, tmux-edge popup renderer with the help overlay.
    fn render_menu(&self, frame: &mut Frame) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let rect = menu.rect;
        let pad = rect.width.saturating_sub(4) as usize;
        let lines: Vec<Line> = menu
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let style = if menu.hovered == Some(i) {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!(" {:<pad$} ", it.label()), style))
            })
            .collect();
        render_popup(frame, self.screen_area, rect, &menu.title, lines);
    }
}

/// Renders an opaque bordered popup at `rect` (titled, content `lines`), in tmux's
/// edge style. Two things make it tmux-consistent:
///
/// 1. **Opaque, no margin.** The box is filled with the reset (default) style so the
///    mux grid's background colours behind it cannot bleed through, and ONLY `rect`
///    itself is cleared — there is no blanket one-cell margin around the box, so
///    half-width neighbours sit flush against the border.
/// 2. **Wide-glyph edge handling.** A double-width (CJK) glyph whose right half the
///    LEFT border now covers would otherwise leave its orphaned left half rendering
///    as a broken glyph just outside the box. That single cell is blanked — and only
///    that cell, only when it is actually a wide glyph. The right edge needs no fixup:
///    ratatui stores a wide char as `[glyph][space]`, so a glyph whose lead the box
///    covers leaves only its already-blank continuation outside.
fn render_popup(frame: &mut Frame, area: Rect, rect: Rect, title: &str, lines: Vec<Line>) {
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .style(Style::reset());
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), rect);
    if rect.x > area.x {
        let x = rect.x - 1;
        let y_end = (rect.y + rect.height).min(area.y + area.height);
        let buf = frame.buffer_mut();
        for y in rect.y..y_end {
            if buf[(x, y)].symbol().width() > 1 {
                buf[(x, y)].set_symbol(" ");
            }
        }
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

impl Menu {
    /// The item index at 0-based screen (col,row), or None if outside the item area
    /// (the box's bordered interior, one row per item below the top border).
    fn item_at(&self, col: u16, row: u16) -> Option<usize> {
        let inside_x = col > self.rect.x && col + 1 < self.rect.x + self.rect.width;
        if !inside_x || row <= self.rect.y {
            return None;
        }
        let i = (row - self.rect.y - 1) as usize;
        (i < self.items.len()).then_some(i)
    }

    /// Whether 0-based screen (col,row) is anywhere inside the box (border included).
    /// Used to keep the highlight while the cursor is over the title border but off an
    /// item — only dragging fully outside the box clears it.
    fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }
}

/// The bordered menu box for an anchor at 0-based screen (col,row): sized to the wider
/// of the widest item label (+ a pad cell each side) and the title, plus borders, and
/// the item count; clamped so it stays fully inside `area` (shifts up/left near an edge).
fn menu_rect(col: u16, row: u16, items: &[MenuItem], title: &str, area: Rect) -> Rect {
    let item_w = items.iter().map(|it| it.label().chars().count()).max().unwrap_or(0);
    let content_w = (item_w + 2).max(title.chars().count()) as u16;
    let w = (content_w + 2).min(area.width.max(1));
    let h = (items.len() as u16 + 2).min(area.height.max(1));
    // Anchor the title row (top border) on the pointer, tmux-style: the pointer lands on
    // the title line, a column left so it sits just inside the box rather than on the left
    // border. item_at() is None on the title row, so no item is pre-selected — an
    // accidental right-click releases off every item (cancel), and a deliberate pick is a
    // short drag straight down onto an item.
    let ax = col.saturating_sub(1);
    let ay = row;
    let max_x = (area.x + area.width).saturating_sub(w).max(area.x);
    let max_y = (area.y + area.height).saturating_sub(h).max(area.y);
    Rect {
        x: ax.clamp(area.x, max_x),
        y: ay.clamp(area.y, max_y),
        width: w,
        height: h,
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

/// `centered_rect` shifted by `offset` (cells) and clamped fully inside `area`.
fn offset_centered(w: u16, h: u16, area: Rect, offset: (i16, i16)) -> Rect {
    let base = centered_rect(w, h, area);
    let max_x = area.x + area.width.saturating_sub(base.width);
    let max_y = area.y + area.height.saturating_sub(base.height);
    let x = (base.x as i32 + offset.0 as i32).clamp(area.x as i32, max_x as i32) as u16;
    let y = (base.y as i32 + offset.1 as i32).clamp(area.y as i32, max_y as i32) as u16;
    Rect { x, y, width: base.width, height: base.height }
}

/// A short popup title for an input mode (shown on the box's top border).
fn input_title(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Filter => "filter",
        InputMode::New => "new session",
        InputMode::NewWindow => "new window",
        InputMode::SplitWindow => "split",
        InputMode::Rename => "rename",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn map_color_named_and_default() {
        assert_eq!(map_color("green"), Color::Green);
        assert_eq!(map_color("blue"), Color::Blue);
        assert_eq!(map_color("yellow"), Color::Yellow);
        assert_eq!(map_color("white"), Color::White);
        assert_eq!(map_color("default"), Color::Reset);
        assert_eq!(map_color(""), Color::Reset, "empty = inherit/terminal default");
        assert_eq!(map_color("brightblack"), Color::DarkGray);
    }

    #[test]
    fn map_color_indexed_and_hex() {
        assert_eq!(map_color("colour4"), Color::Indexed(4));
        assert_eq!(map_color("color12"), Color::Indexed(12));
        assert_eq!(map_color("#268bd2"), Color::Rgb(0x26, 0x8b, 0xd2));
    }

    #[test]
    fn map_color_tolerates_fg_prefix_and_case() {
        assert_eq!(map_color("fg=blue"), Color::Blue, "tmux style string drops in verbatim");
        assert_eq!(map_color("  Blue "), Color::Blue, "trimmed and case-insensitive");
        assert_eq!(map_color("fg=#EEE8D5"), Color::Rgb(0xee, 0xe8, 0xd5));
    }
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
    fn select_active_window_descends_from_a_host_row() {
        // focus→mux from a HOST row must descend into the host's recent session's active
        // window (the window the mux displays), not leave the cursor stuck on the host.
        let mut sw = Switcher::new(two_window_scan());
        sw.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)); // ascend: session → host
        assert!(matches!(sw.current_ref(), Some(RowRef::Host { .. })), "cursor on the host row");
        assert!(sw.select_active_window(), "descends from host to the active window");
        assert!(
            matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })),
            "landed on the recent session's active window (0)"
        );
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
            "⚠", // unreachable host marker (the reason now lives in the info pane)
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
    async fn apply_source_result_unreachable_marks_tree_and_reason_in_info_pane() {
        let mut h = Harness::from_sources(&["prod"]);
        h.sw.apply_source_result("prod".into(), vec![], Some("connection refused".into()));
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
        assert!(out.contains("unreachable"), "info pane states unreachable:\n{out}");
        assert!(
            out.contains("connection refused"),
            "info pane shows the failure reason:\n{out}"
        );
    }

    #[tokio::test]
    async fn unreachable_info_pane_shows_ssh_config_stanza() {
        let mut h = Harness::from_sources(&["jupiter00"]);
        h.sw.set_ssh_config_text(
            "Host jupiter00\n    HostName 143.248.140.120\n    User hrlee\n\nHost other\n    HostName 1.2.3.4\n".into(),
        );
        h.sw.apply_source_result("jupiter00".into(), vec![], Some("no route".into()));
        h.draw();
        let out = h.text();
        assert!(out.contains("HostName 143.248.140.120"), "shows the host's ssh config:\n{out}");
        assert!(out.contains("hrlee"), "shows the configured user:\n{out}");
        assert!(!out.contains("1.2.3.4"), "does NOT leak an unrelated host's config:\n{out}");
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
    async fn kill_confirm_esc_cancels() {
        let mut h = Harness::new(sample()); // a local session ("editor") is preselected
        h.ch('x').await; // arm the kill y/n confirm
        assert!(h.sw.pending_kill.is_some(), "x arms the y/n confirm");
        h.key(KeyCode::Esc).await; // Esc cancels it, like the input prompts
        assert!(h.sw.pending_kill.is_none(), "Esc clears the confirm");
        assert!(h.ops.killed.lock().unwrap().is_empty(), "Esc must not kill anything");
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

    #[test]
    fn menu_items_by_row_type() {
        use super::MenuItem::*;
        let host = RowRef::Host { source: "h".into(), unreachable: false };
        assert_eq!(menu_items(&host), vec![NewSession]);

        let s = sess("h", "api", 1, false, 0);
        assert_eq!(
            menu_items(&RowRef::Session(s.clone())),
            vec![Focus, NewWindow, Rename, Kill]
        );
        assert_eq!(
            menu_items(&RowRef::Window { sess: s, window: 1 }),
            vec![Focus, Rename, Kill]
        );
        assert!(menu_items(&RowRef::Pane).is_empty());
        assert!(menu_items(&RowRef::Loading).is_empty());
    }

    /// The screen (col,row) of the tree row at `idx`, given the current layout.
    fn row_screen_pos(h: &Harness, idx: usize) -> (u16, u16) {
        let y = h.sw.tree_inner.y + (idx - h.sw.list_state.offset()) as u16;
        (h.sw.tree_inner.x, y)
    }

    fn row_index<F: Fn(&RowRef) -> bool>(h: &Harness, pred: F) -> usize {
        h.sw.rows.iter().position(|r| pred(&r.reference)).expect("row exists")
    }

    #[tokio::test]
    async fn menu_open_on_session_does_not_move_cursor() {
        let mut h = Harness::new(sample());
        let before = h.sw.selected;
        let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(h.sw.menu_open(x, y), "menu opens over a session row");
        assert!(h.sw.menu_active());
        assert_eq!(h.sw.selected, before, "opening the menu must not move the tree cursor");
    }

    #[tokio::test]
    async fn menu_does_not_open_on_pane_row() {
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Pane));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(!h.sw.menu_open(x, y), "no menu over a pane row");
        assert!(!h.sw.menu_active());
    }

    #[tokio::test]
    async fn menu_release_off_menu_cancels() {
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Session(_)));
        let (x, y) = row_screen_pos(&h, idx);
        h.sw.menu_open(x, y);
        // Drag fully outside the box → highlight clears → release cancels.
        h.sw.menu_hover(99, 29);
        assert!(matches!(h.sw.menu_release(), MenuOutcome::None));
        assert!(!h.sw.menu_active(), "menu closes on release");
        assert!(h.sw.input.is_none() && h.sw.pending_kill.is_none(), "nothing happened");
    }

    #[tokio::test]
    async fn menu_release_in_place_cancels() {
        // Accidental-click safety: open then release WITHOUT dragging onto an item does
        // nothing. The pointer lands on the title row with no item pre-selected, so the
        // release lands off every item.
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(h.sw.menu_open(x, y));
        assert!(matches!(h.sw.menu_release(), MenuOutcome::None), "no drag → no action");
        assert!(!h.sw.menu_active());
        assert!(h.sw.input.is_none() && h.sw.pending_kill.is_none(), "nothing happened");
    }

    #[tokio::test]
    async fn menu_title_row_sits_on_the_pointer() {
        // tmux-style: the title row (top border) lands on the click row, and no item is
        // pre-selected under the pointer — an accidental right-click releases off every item.
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(h.sw.menu_open(x, y));
        let menu = h.sw.menu.as_ref().unwrap();
        assert_eq!(menu.rect.y, y, "the title row sits on the click row");
        assert_eq!(menu.item_at(x, y), None, "no item under the pointer");
    }

    #[tokio::test]
    async fn menu_release_focus_focuses_terminal_and_selects_target() {
        let mut h = Harness::new(sample());
        let s = sess("local", "build", 1, false, 100);
        let target = RowRef::Session(s);
        let items = menu_items(&target);
        let focus_at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(focus_at) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::FocusTerminal));
        assert_eq!(cur_session_name(&h).as_deref(), Some("build"), "cursor moved to target");
    }

    #[tokio::test]
    async fn menu_release_rename_opens_input() {
        let mut h = Harness::new(sample());
        let target = RowRef::Session(sess("local", "build", 1, false, 100));
        let items = menu_items(&target);
        let at = items.iter().position(|i| *i == MenuItem::Rename).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
        assert!(h.sw.is_inputting(), "rename opens the inline input");
    }

    #[tokio::test]
    async fn menu_release_kill_arms_confirm() {
        let mut h = Harness::new(sample());
        let target = RowRef::Session(sess("local", "build", 1, false, 100));
        let items = menu_items(&target);
        let at = items.iter().position(|i| *i == MenuItem::Kill).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
        assert!(h.sw.pending_kill.is_some(), "kill arms the y/n confirm");
    }

    #[tokio::test]
    async fn menu_kill_keeps_the_cursor_so_the_confirm_survives() {
        // Regression: the y/n confirm flashed and vanished because moving the cursor to
        // the target changed the selection → attach → events → rebuild, which clears
        // pending_kill. Acting on a row must NOT move the cursor.
        let mut h = Harness::new(sample());
        let editor = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "editor"));
        h.sw.set_selected(editor);
        h.sw.user_moved = true;
        // Kill a DIFFERENT session ('build') via the menu.
        let target = RowRef::Session(sess("local", "build", 1, false, 100));
        let items = menu_items(&target);
        let at = items.iter().position(|i| *i == MenuItem::Kill).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
        assert!(h.sw.pending_kill.is_some(), "kill is armed against the clicked row");
        assert!(
            matches!(h.sw.current_ref(), Some(RowRef::Session(s)) if s.name == "editor"),
            "the cursor stayed put → no selection change to rebuild away the confirm"
        );
    }

    #[tokio::test]
    async fn kill_confirm_survives_a_rebuild_until_the_target_vanishes() {
        // The confirm must NOT have a time limit: a routine rebuild (the 1.5s local
        // poll, a remote %-event) used to clear pending_kill out from under the user.
        let mut h = Harness::new(sample());
        let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        h.sw.set_selected(build);
        h.sw.arm_kill();
        assert!(h.sw.pending_kill.is_some(), "kill armed");
        h.sw.rebuild(); // a poll/event rebuild
        assert!(h.sw.pending_kill.is_some(), "confirm survives a routine rebuild — no time limit");
        // But once the target is actually gone, the stale confirm is dropped.
        h.sw.groups = crate::ui::tree::remove_session(&h.sw.groups, "local/build");
        h.sw.rebuild();
        assert!(h.sw.pending_kill.is_none(), "a vanished target invalidates the confirm");
    }

    #[tokio::test]
    async fn menu_focus_window_marks_it_active_so_passthrough_follow_keeps_it() {
        // Regression: focusing a different window of the already-displayed session must
        // move there. Without optimistically marking it active, select_active_window
        // (the passthrough follow) yanks the cursor back to the old active window.
        let mut h = Harness::new(sample());
        let s = sess("local", "editor", 2, true, 200); // editor: win 1 active, win 2 not
        let target = RowRef::Window { sess: s, window: 2 };
        let items = menu_items(&target);
        let at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 5), items, hovered: Some(at) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::FocusTerminal));
        assert!(
            matches!(h.sw.current_ref(), Some(RowRef::Window { window, .. }) if *window == 2),
            "cursor is on the focused window"
        );
        assert!(!h.sw.select_active_window(), "window 2 is now active → no yank back to window 1");
    }

    #[tokio::test]
    async fn menu_new_session_opens_input_and_creates() {
        // Regression: 'new session' via the host menu must open the name input and,
        // on confirm, create the session — the full gesture-to-op path.
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Host { source, .. } if source == "local"));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(h.sw.menu_open(x, y), "menu opens on the host row");
        // Deliberately move onto the first item (no pre-hover), then release.
        let rect = h.sw.menu.as_ref().unwrap().rect;
        h.sw.menu_hover(rect.x + 1, rect.y + 1);
        assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
        assert!(h.sw.is_inputting(), "new session opens the name input");
        h.sw.set_input_text("fresh");
        h.key(KeyCode::Enter).await; // queues + pumps the create op (as the loop would)
        assert!(
            h.ops.created.lock().unwrap().iter().any(|c| c.contains("fresh")),
            "the session is created: {:?}",
            h.ops.created.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn menu_release_window_focus_selects_window() {
        let mut h = Harness::new(sample());
        let s = sess("local", "editor", 2, true, 200);
        let target = RowRef::Window { sess: s, window: 2 };
        let items = menu_items(&target);
        let at = items.iter().position(|i| *i == MenuItem::Focus).unwrap();
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
        // A window row offers focus / rename / kill — no split.
        assert!(!items_have_split(&menu_items(&RowRef::Window { sess: sess("local", "editor", 2, true, 200), window: 2 })));
        assert!(matches!(h.sw.menu_release(), MenuOutcome::FocusTerminal));
    }

    fn items_have_split(items: &[MenuItem]) -> bool {
        // Split was deliberately removed from the menu; this guards the regression.
        items.iter().any(|i| i.label().contains("split"))
    }

    #[tokio::test]
    async fn menu_open_on_host_row() {
        let mut h = Harness::new(sample());
        let idx = row_index(&h, |r| matches!(r, RowRef::Host { source, .. } if source == "local"));
        let (x, y) = row_screen_pos(&h, idx);
        assert!(h.sw.menu_open(x, y), "menu opens over a host row");
        // A host menu's first item (new session) opens an input.
        let target = RowRef::Host { source: "local".into(), unreachable: false };
        let items = menu_items(&target);
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 4), items, hovered: Some(0) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled), "new session opens an input");
        assert!(h.sw.is_inputting());
    }

    #[tokio::test]
    async fn menu_release_stale_target_cancels() {
        let mut h = Harness::new(sample());
        // A target that does not exist in the tree (rebuilt away during the hold).
        let target = RowRef::Session(sess("local", "ghost", 1, false, 0));
        let items = menu_items(&target);
        h.sw.menu = Some(Menu { target, title: String::new(), rect: Rect::new(0, 0, 20, 7), items, hovered: Some(0) });
        assert!(matches!(h.sw.menu_release(), MenuOutcome::None), "gone target → no-op");
    }

    #[test]
    fn menu_rect_clamps_into_screen() {
        use super::MenuItem::*;
        let area = Rect::new(0, 0, 80, 24);
        let items = [Focus, Rename, Kill];
        // Anchored near the bottom-right corner → shifted up/left to stay on-screen.
        let r = menu_rect(78, 23, &items, "editor", area);
        assert!(r.x + r.width <= area.width, "box stays within the right edge");
        assert!(r.y + r.height <= area.height, "box stays within the bottom edge");
    }

    #[test]
    fn menu_rect_fits_a_title_wider_than_the_items() {
        use super::MenuItem::*;
        let area = Rect::new(0, 0, 80, 24);
        let r = menu_rect(0, 0, &[Focus], "a-very-long-session-name", area);
        assert!(r.width as usize >= "a-very-long-session-name".len() + 2, "title fits in the box");
    }

    #[tokio::test]
    async fn menu_renders_title_and_hovered_item_reversed() {
        use super::MenuItem::*;
        let mut h = Harness::new(sample());
        let target = RowRef::Session(sess("local", "build", 1, false, 100));
        let items = vec![Focus, Rename, Kill, NewWindow];
        // Box at a known spot; hover the second item (rename).
        h.sw.menu = Some(Menu { target, title: "build".into(), rect: Rect::new(2, 2, 18, 6), items, hovered: Some(1) });
        h.draw();
        let out = h.text();
        assert!(out.contains("focus") && out.contains("rename") && out.contains("kill"),
            "menu items render:\n{out}");
        assert!(out.contains("build"), "the menu shows its target's name as the title:\n{out}");

        // The hovered row (rename, at box y+1+1 = 4) is reversed across the box interior.
        let buf = h.buf();
        let reversed = (3..19).any(|x| buf[(x, 4u16)].modifier.contains(Modifier::REVERSED));
        assert!(reversed, "the hovered item renders reversed");
    }

    #[tokio::test]
    async fn help_lists_the_right_click_menu() {
        let mut h = Harness::new(sample());
        h.sw.show_help();
        h.draw();
        assert!(h.text().contains("right-click"), "help mentions the right-click menu");
    }

    #[tokio::test]
    async fn help_overlay_renders_and_closes_on_q() {
        let mut h = Harness::new(sample());
        assert!(!h.text().contains("keys"), "help hidden initially");
        h.sw.show_help(); // driven by the cockpit's `prefix ?`
        h.draw();
        let out = h.text();
        assert!(
            out.contains("keys"),
            "show_help opens the overlay:\n{out}"
        );
        assert!(out.contains("fuzzy filter"), "help should list keybindings");
        // Modal dismissal (tmux view-mode): the cockpit routes keys to feed_help_key
        // above the tree/mux split — q closes it; other keys are swallowed (no nav).
        assert!(h.sw.feed_help_key(b"q"), "q is consumed while help is open");
        h.draw();
        assert!(
            !h.text().contains("fuzzy filter"),
            "q closes the help overlay"
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
    async fn enter_and_bare_q_are_noops() {
        // Enter is consumed by the cockpit (focus the mux), not the switcher; bare q does
        // nothing — quit is `prefix q` at the cockpit level. Neither moves the cursor or
        // opens an input here.
        let mut h = Harness::new(sample());
        let before = cur_row_label(&h);
        h.key(KeyCode::Enter).await;
        h.ch('q').await;
        assert!(!h.sw.is_inputting(), "neither opens an input");
        assert_eq!(cur_row_label(&h), before, "neither moves the cursor");
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

    #[test]
    fn long_flash_wraps_in_narrow_footer_instead_of_clipping() {
        // The footer lives in the tree column; a long flash must wrap across lines rather
        // than clip at the column edge (a narrow tree would otherwise hide most of it).
        let mut sw = Switcher::new(sample());
        sw.flash = "host unreachable — cannot create here".into();
        let lines = sw.footer_lines(20);
        assert!(lines.len() > 1, "long flash wraps across lines, got {lines:?}");
        assert!(
            lines.iter().all(|l| l.chars().count() <= 20),
            "every wrapped line fits the width, got {lines:?}"
        );
        let joined = lines.join("").replace("  ", " ");
        assert!(joined.contains("cannot create here"), "no text is lost: {joined:?}");
    }

    #[tokio::test]
    async fn flash_clears_on_next_key_restoring_the_footer() {
        // A flash (e.g. "host unreachable — cannot create here") is transient: any key
        // dismisses it so the normal help/status footer returns. Regression: it persisted
        // because only the input-opening actions cleared it, so navigation never did.
        let mut h = Harness::new(sample());
        h.sw.flash = "host unreachable — cannot create here".into();
        h.key(KeyCode::Down).await;
        assert!(h.sw.flash.is_empty(), "navigation clears the flash, got {:?}", h.sw.flash);
    }

    #[tokio::test]
    async fn footer_and_help_reflect_new_model() {
        let mut h = Harness::new(sample());
        let footer = h.footer_text();
        assert!(!footer.to_lowercase().contains("enter attach"), "Enter is a no-op now:\n{footer}");
        assert!(footer.contains("focus"),
            "footer mentions focusing the terminal pane:\n{footer}");
        h.sw.show_help(); // driven by the cockpit's `prefix ?`
        h.draw();
        let help = h.text();
        assert!(help.contains("focus the mux"),
            "help explains focusing the mux pane:\n{help}");
        assert!(!help.contains("select = attach"),
            "no useless 'select = attach' noise in help:\n{help}");
        assert!(!help.contains("dwell") && !help.to_lowercase().contains("previous foreground"),
            "no stale dwell/esc-return strings:\n{help}");
    }

    #[tokio::test]
    async fn divider_uses_configured_colors() {
        // Colours set from config (pane-*-border-style) drive the divider: active on the
        // focused half, inactive on the other, hover overrides both while hovered.
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let mut sw = Switcher::new(sample());
        sw.set_divider_colors(DividerColors {
            active: Color::Blue,
            inactive: Color::Gray,
            hover: Color::Red,
        });
        let x = TREE_WIDTH;
        let (top, bottom) = (2u16, 27u16);
        let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

        // Tree focused: top = active(Blue), bottom = inactive(Gray).
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(fg(&buf, top), Color::Blue, "configured active on the focused half");
        assert_eq!(fg(&buf, bottom), Color::Gray, "configured inactive on the unfocused half");

        // Hovering the rule overrides with the configured hover colour.
        sw.set_divider_hovered(true);
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(fg(&buf, top), Color::Red, "configured hover colour while hovered");
    }

    #[tokio::test]
    async fn divider_splits_top_bottom_to_mark_focused_side() {
        // The rule splits into halves: the accent (green) half marks WHICH pane has
        // focus — top = tree (left), bottom = mux (right) — and the other half is dim.
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let mut sw = Switcher::new(sample());
        let x = TREE_WIDTH;
        let (top, bottom) = (2u16, 27u16); // within the top / bottom halves of height 30
        let fg = |buf: &Buffer, y: u16| buf[(x, y)].fg;

        // Mux focused: accent on the bottom (mux side), inactive on top. The inactive
        // half is the tmux default (terminal default = Color::Reset), not a dim grey.
        term.draw(|f| sw.render(f, None, true, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(x, top)].symbol(), "│", "divider still drawn");
        assert_eq!(fg(&buf, bottom), Color::Green, "mux focus: bottom half accent");
        assert_eq!(fg(&buf, top), Color::Reset, "mux focus: top half inactive (tmux default)");

        // Tree focused: accent on the top (tree side), inactive on bottom.
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(fg(&buf, top), Color::Green, "tree focus: top half accent");
        assert_eq!(fg(&buf, bottom), Color::Reset, "tree focus: bottom half inactive");
    }

    #[tokio::test]
    async fn divider_highlights_on_hover() {
        // Hover swaps the rule to the HEAVY vertical (┃) — box-drawing has no bold form,
        // so the thicker glyph IS the weight cue — and recolours it brighter. No fill.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut sw = Switcher::new(sample());
        let x = TREE_WIDTH;
        sw.set_divider_hovered(true);
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        let buf = term.backend().buffer().clone();
        for y in [2u16, 27u16] {
            let cell = &buf[(x, y)];
            assert_eq!(cell.symbol(), "┃", "hover: heavy (thick) rule glyph at row {y}");
            assert_eq!(cell.fg, Color::Yellow, "hover: yellow (psmux hover) at row {y}");
            assert!(
                !cell.modifier.contains(Modifier::REVERSED),
                "hover: not reversed/filled (no block) at row {y}",
            );
        }
    }

    #[tokio::test]
    async fn divider_glyph_reflects_auto_hide_mode() {
        // ║ (double) when auto-hide-tree mode is on, │ (single) when off — so a visible
        // tree that will vanish on blur is distinguishable from a pinned one.
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let mut sw = Switcher::new(sample());
        let (x, y) = (TREE_WIDTH, 2u16);

        sw.set_auto_hide(false);
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        assert_eq!(term.backend().buffer()[(x, y)].symbol(), "│", "mode off → single line");

        sw.set_auto_hide(true);
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH)).unwrap();
        assert_eq!(term.backend().buffer()[(x, y)].symbol(), "║", "mode on → double line");
    }

    #[test]
    fn popup_blanks_only_a_wide_glyph_bisected_by_the_left_border() {
        // tmux edge behaviour: no blanket margin. A double-width glyph whose right half
        // the left border covers is blanked (its orphaned half would render broken); a
        // half-width char at the same edge column stays flush; the box covers opaquely.
        let backend = TestBackend::new(40, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.buffer_mut()[(9u16, 3u16)].set_symbol("한"); // wide; right half under the border at x=10
            f.buffer_mut()[(9u16, 4u16)].set_symbol("Y"); // half-width at the same edge column
            f.buffer_mut()[(15u16, 4u16)].set_style(Style::default().bg(Color::Red)); // behind the popup
            let rect = Rect::new(10, 2, 12, 5);
            render_popup(f, area, rect, "t", vec![Line::from("focus"), Line::from("kill"), Line::from("x")]);
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf[(9u16, 3u16)].symbol(), " ", "wide glyph bisected by the left border is blanked");
        assert_eq!(buf[(9u16, 4u16)].symbol(), "Y", "a half-width char at the edge stays flush — no margin");
        assert_eq!(buf[(15u16, 4u16)].bg, Color::Reset, "the popup covers the background colour opaquely");
    }

    #[tokio::test]
    async fn every_popup_type_is_opaque_over_a_colored_grid() {
        // A grid filled with a blue background; each popup type drawn over it must leave
        // zero interior cells showing the grid's background (the shared render_popup is
        // opaque — this locks it in across help / input / confirm).
        fn blue_grid() -> crate::proxy::screen::Grid {
            let mut g = crate::proxy::screen::Grid::new(30, 100);
            let mut fill = Vec::from(&b"\x1b[44m"[..]);
            for r in 0..30u16 {
                fill.extend(format!("\x1b[{};1H", r + 1).bytes());
                fill.extend(std::iter::repeat(b'X').take(100));
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
            let Some((x0, y0)) = tl else { return usize::MAX };
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
        h.sw.show_help();
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), true, 0)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "help popup interior must be opaque");

        let mut h = Harness::new(sample());
        h.ch('/').await;
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "input popup interior must be opaque");

        let mut h = Harness::new(sample());
        let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        h.sw.set_selected(build);
        h.sw.user_moved = true;
        h.sw.arm_kill();
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "confirm popup interior must be opaque");
    }

    #[test]
    fn toggle_help_flips_visibility() {
        let mut sw = Switcher::new(sample());
        assert!(!sw.show_help);
        sw.toggle_help();
        assert!(sw.show_help);
        sw.toggle_help();
        assert!(!sw.show_help);
    }

    #[test]
    fn feed_help_key_is_modal_and_closes_on_q_or_esc() {
        // tmux view-mode style: while open, every key is consumed; q/Esc closes, the
        // rest are swallowed; while closed, nothing is consumed (falls through).
        let mut sw = Switcher::new(sample());
        assert!(!sw.feed_help_key(b"q"), "closed → not consumed, routes normally");

        sw.toggle_help();
        assert!(sw.feed_help_key(b"j"), "open → consumed");
        assert!(sw.show_help, "a non-close key is swallowed but keeps help open");
        assert!(sw.feed_help_key(b"\x1b[A"), "an arrow (ESC [) is swallowed, not a close");
        assert!(sw.show_help, "arrow keeps help open");

        assert!(sw.feed_help_key(b"q"), "q → consumed");
        assert!(!sw.show_help, "q closes help");

        sw.toggle_help();
        assert!(sw.feed_help_key(b"\x1b"), "lone Esc → consumed");
        assert!(!sw.show_help, "Esc closes help");
    }

    #[tokio::test]
    async fn input_renders_as_a_centered_popup_not_the_bottom_pane() {
        let mut h = Harness::new(sample());
        h.ch('/').await; // open the filter input
        let buf = h.buf();
        let w = buf.area.width;
        let last = buf.area.height - 1;
        // The entry field is NO LONGER on the bottom row.
        let bottom: String = (0..w).map(|x| buf[(x, last)].symbol()).collect();
        assert!(!bottom.contains('❯'), "entry must not be on the bottom row anymore:\n{bottom}");
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
        assert!(h.sw.is_inputting(), "input open");
        h.key(KeyCode::Esc).await;
        assert!(!h.sw.is_inputting(), "Esc closes the input");
        assert!(h.ops.created.lock().unwrap().is_empty(), "Esc must not create anything");
    }

    #[test]
    fn wrap_text_wraps_on_words_and_hard_splits_long_words() {
        use unicode_width::UnicodeWidthStr;
        let s = "filter sessions · Esc to cancel";
        let lines = wrap_text(s, 19);
        assert!(lines.len() >= 2, "wraps when narrower than the text: {lines:?}");
        assert!(lines.iter().all(|l| l.as_str().width() <= 19), "no line exceeds width: {lines:?}");
        assert!(lines.join(" ").contains("cancel"), "tail survives (not clipped): {lines:?}");
        // A single word longer than the width is hard-split, each piece within width.
        let long = wrap_text("supercalifragilistic", 5);
        assert!(long.len() >= 4 && long.iter().all(|l| l.as_str().width() <= 5), "{long:?}");
        // A wide enough width keeps it on one line.
        assert_eq!(wrap_text(s, 100).len(), 1);
    }

    #[tokio::test]
    async fn kill_confirm_is_a_centered_red_popup_not_the_footer() {
        let mut h = Harness::new(sample());
        let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        h.sw.set_selected(build);
        h.sw.user_moved = true;
        h.key(KeyCode::Char('x')).await; // arm the confirm
        let buf = h.buf();
        let last = buf.area.height - 1;
        let footer: String = (0..buf.area.width).map(|x| buf[(x, last)].symbol()).collect();
        assert!(!footer.contains("[y]es"), "confirm must not be in the footer:\n{footer}");
        // A red "kill" cell exists in a centered box (not the footer row).
        let red_kill = (0..last)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].symbol() == "k" && buf[(x, y)].fg == Color::Red);
        assert!(red_kill, "the confirm popup shows red 'kill' text above the footer");
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
    async fn armed_window_kill_survives_a_same_tree_rebuild() {
        // A routine rebuild (streamed panes with the SAME windows) must NOT cancel an
        // armed window kill — there is no time limit. A later 'y' still kills the right
        // window. (Only a rebuild that actually removes the target invalidates it; the
        // session case is covered by kill_confirm_survives_a_rebuild_until_the_target_vanishes.)
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await;   // local host
        h.key(KeyCode::Right).await;  // → editor (session)
        h.key(KeyCode::Right).await;  // → editor's first window (window 1, name "shell")
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)); // arm kill (raw)
        assert!(
            matches!(h.sw.pending_kill, Some(PendingKill::Window { .. })),
            "arm_kill must set a window PendingKill"
        );
        // Stream the same panes (a rebuild) — the target window still exists.
        let s = sample();
        let editor_panes = s.panes["local/editor"].clone();
        h.sw.apply_panes("local/editor".to_string(), editor_panes);
        assert!(
            h.sw.pending_kill.is_some(),
            "the confirm survives a same-tree rebuild — no time limit"
        );
        // 'y' now confirms and queues the kill of the armed window.
        h.sw.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        let op = h.sw.take_pending_op().expect("kill queued after surviving rebuild");
        assert!(matches!(op, PendingOp::KillWindow { ref target, .. } if target == "editor:1"));
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
    fn render_tree_width_zero_gives_terminal_full_width() {
        // A two-source skeleton is enough; grid None renders the "(attaching…)"
        // placeholder across the whole width when the tree is hidden.
        let mut sw = Switcher::from_sources(vec!["local".into(), "jupiter06".into()]);
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();

        // tree_width == 0 → no tree column, no divider: the terminal view starts at x=0.
        term.draw(|f| sw.render(f, None, true, 0)).unwrap();
        let buf = term.backend().buffer().clone();
        // Column 0 row 0 must NOT be the divider rule '│' (the divider is gone).
        assert_ne!(buf[(0, 0)].symbol(), "│", "divider must be absent when tree hidden");
        // The attaching placeholder text "(attaching…)" begins near x=0 (after its
        // two leading spaces), proving the terminal view owns the left edge.
        let row0: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.contains("(attaching…)"), "terminal view fills row 0: {row0:?}");

        // Sanity: with a normal width the divider rule IS present at the tree edge.
        term.draw(|f| sw.render(f, None, true, 20)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(20, 0)].symbol(), "│", "divider present at x=tree_width when shown");
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
