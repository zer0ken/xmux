//! The interactive session switcher: a two-pane navigator (a unified
//! Host·Session·Window·Pane tree on the left, a live preview on the right) with a
//! hidden input row and a hint_bar. ratatui is immediate-mode, so this owns its
//! state machine, a flattened row model, key/mouse handling, and a render pass
//! that draws to either the live terminal or a headless `TestBackend` (the
//! control channel's `dump`).

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{Action, Command};
use crate::session::{Session, WindowPanes};
use crate::ui::modal::{
    self, Input, InputMode, Menu, MenuItem, MenuOutcome, Modal, PendingKill, PopupGeometry,
};
use crate::ui::tree::{self, Group, Row, RowRef};

use crate::ui::ops::OpFollow;
pub use crate::ui::ops::{run_op, OpResult, Ops};

/// Tree pane width: border + 1-cell inner padding each side + content.
pub const TREE_WIDTH: u16 = 48;

// Per-level node colours, so the four tree levels read apart at a glance.
const COLOR_HOST: Color = Color::Yellow;
const COLOR_SESSION: Color = Color::Green;
const COLOR_WINDOW: Color = Color::Magenta;
const COLOR_PANE: Color = Color::Cyan;
/// Transient per-element status (scanning…, loading…, (empty), unreachable)
/// renders dim so pending state reads apart from settled content.
const COLOR_HINT: Color = Color::DarkGray;

pub use crate::ui::chrome::ViewBorderColors;

/// A fully-populated snapshot of the reachable environment.
#[derive(Clone, Default)]
pub struct Scan {
    pub groups: Vec<Group>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
}

/// Snapshot of the selection taken before a rebuild so `restore_focus` can
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

/// The switcher state machine.
pub struct Switcher {
    /// Set once the user explicitly moves the selection; while false, streaming
    /// results advance the preselect toward the most-recent session.
    user_moved: bool,
    /// Signals the event loop to (re)kick the streaming probes — set on the
    /// initial seed and on an `r` re-scan; the loop reads + clears it.
    rescan_kick: bool,
    /// Signals the event loop to re-attach the CURRENT display: tear the (possibly
    /// detached / dead) attachment down so the next attach re-creates a fresh client.
    /// Set on an `r` re-scan — explicit, on-demand recovery for the viewed session.
    reattach_kick: bool,

    rows: Vec<Row>,
    selected: usize,

    terminal_view_target: TerminalViewTarget,

    list_state: ListState,
    tree_inner: Rect,

    /// A pending re-scan reselect: the session address the selection was on when `r`
    /// was pressed. A re-scan clears every session, so the row briefly vanishes; this
    /// returns the selection to it the instant its host re-streams. Cleared once matched,
    /// or when the user navigates off the parked parent host during the skeleton phase.
    rescan_reselect: Option<String>,
    /// The whole frame area, captured each render so the menu box can be clamped to
    /// the screen at open time (mouse events arrive between renders).
    screen_area: Rect,
    /// The transient geometry of the active modal popup (drag offset / drawn rect /
    /// in-flight border drag). The drag behavior lives on [`PopupGeometry`].
    popup_geo: PopupGeometry,
}

mod render;

impl Switcher {
    fn blank() -> Self {
        Switcher {
            user_moved: false,
            rescan_kick: false,
            reattach_kick: false,
            rows: Vec::new(),
            selected: 0,
            terminal_view_target: TerminalViewTarget::default(),
            list_state: ListState::default(),
            tree_inner: Rect::default(),
            rescan_reselect: None,
            screen_area: Rect::default(),
            popup_geo: PopupGeometry::default(),
        }
    }

    /// Builds from a complete snapshot's inventory (carried on `state`): every host
    /// is resolved (reachable or unreachable per its `err`) and every session's panes
    /// are considered known. The caller seeds `state` via [`crate::state::State::from_scan`].
    pub fn new(state: &mut crate::state::State) -> Self {
        let mut s = Switcher::blank();
        s.rebuild(state);
        s
    }

    /// Seeds the switcher from the resolved source list alone — no probing — so
    /// the first frame paints host-skeleton rows, each in a scanning state, in
    /// tens of milliseconds. Streamed [`apply_source_result`]/[`apply_panes`]
    /// calls fill the tree in afterward. The caller seeds `state` via
    /// [`crate::state::State::from_sources`].
    pub fn from_sources(state: &mut crate::state::State) -> Self {
        let mut s = Switcher::blank();
        s.rescan_kick = true; // the event loop kicks the probes on the first frame
        s.rebuild(state);
        s
    }

    pub fn terminal_view_target(&self) -> TerminalViewTarget {
        self.terminal_view_target.clone()
    }

    /// Takes the pending rescan-kick flag (true once after seeding or an `r`
    /// re-scan) — the event loop spawns the streaming probes when it is set.
    pub fn take_rescan_kick(&mut self) -> bool {
        std::mem::take(&mut self.rescan_kick)
    }

    /// Consumes the re-attach kick (set by an `r` re-scan): the loop tears down the
    /// current display attachment so the next attach re-creates a fresh client.
    pub fn take_reattach_kick(&mut self) -> bool {
        std::mem::take(&mut self.reattach_kick)
    }

    // --- tree model ---------------------------------------------------------

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

    fn rebuild(&mut self, state: &mut crate::state::State) {
        // Once the user has moved the selection, hold their current session/window selection
        // across this rebuild when it survives (matched by identity) — a routine rebuild
        // (local poll, remote %-event refetch) must NOT snap the selection back to the top
        // row, which would yank the displayed session out from under the user on every
        // poll (the selection thrash). The user_moved gate at the target below preserves the
        // launch behavior: an untouched selection preselects the top row (the local host,
        // index 0).
        let keep = self
            .rows
            .get(self.selected)
            .and_then(|r| match &r.reference {
                RowRef::Session(_) | RowRef::Window { .. } => Some(r.reference.clone()),
                _ => None,
            });

        // Pure row generation lives in `tree::flatten`; rebuild orchestrates
        // capture → flatten → preselect → restore around it.
        let rows = tree::flatten(
            &state.groups,
            &state.panes,
            &state.panes_loaded,
            &state.scanning,
            &state.filter,
        );

        self.rows = rows;
        // Keep an armed kill confirm across this rebuild as long as its target still
        // EXISTS (matched by identity, not row position). Only a tree change that
        // actually removed the target invalidates it — routine rebuilds (the local
        // poll, a remote %-event) must NOT silently cancel it, or answering y/n has a
        // surprise time limit. resolve_kill consumes it; set_selected does not touch it.
        if matches!(&state.modal, Some(Modal::Kill(k)) if !self.kill_target_present(k)) {
            state.modal = None;
        }
        let target = self
            .user_moved
            .then(|| {
                keep.as_ref()
                    .and_then(|k| self.rows.iter().position(|r| same_node(&r.reference, k)))
            })
            .flatten()
            .or_else(|| self.rows.iter().position(Row::selectable))
            .unwrap_or(0);
        self.set_selected(target, state);
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

    fn set_selected(&mut self, idx: usize, state: &crate::state::State) {
        if self.rows.is_empty() {
            return;
        }
        let idx = idx.min(self.rows.len() - 1);
        self.selected = idx;
        self.list_state.select(Some(idx));
        self.on_focus_changed(state);
    }

    fn move_selection(&mut self, delta: isize, state: &crate::state::State) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.user_moved = true;
        let cur = sel.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        let n = sel.len() as isize;
        let next = ((cur + delta) % n + n) % n;
        self.set_selected(sel[next as usize], state);
    }

    /// `→`: descend to the FIRST child of the selected node (host→its first session,
    /// session→its first window). The flattened rows place children immediately after
    /// their parent at a deeper indent, so the child is the next row when its indent
    /// is greater. A no-op when the only children are non-selectable (a window's
    /// panes) or there are none.
    fn descend(&mut self, state: &crate::state::State) {
        let cur = self.selected;
        let Some(cur_indent) = self.rows.get(cur).map(|r| r.indent) else {
            return;
        };
        if let Some(child) = self.rows.get(cur + 1) {
            if child.indent > cur_indent && child.selectable() {
                self.user_moved = true;
                self.set_selected(cur + 1, state);
            }
        }
    }

    /// `←`: ascend to the PARENT of the selected node (window→its session, session→its
    /// host) — the nearest preceding row at a shallower indent. A no-op on a host row.
    fn ascend(&mut self, state: &crate::state::State) {
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
                self.set_selected(i, state);
                return;
            }
        }
    }

    /// ↑/↓ (or k/j): move to the next/prev sibling at the current tree level — the
    /// next selectable row at the SAME indent level (e.g. session→next session,
    /// skipping windows/panes nested under it). Wraps like `move_selection`.
    fn move_sibling(&mut self, delta: isize, state: &crate::state::State) {
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
        let pos = siblings
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0) as isize;
        let n = siblings.len() as isize;
        let next = ((pos + delta) % n + n) % n;
        self.set_selected(siblings[next as usize], state);
    }

    fn move_to(&mut self, pos: isize, state: &crate::state::State) {
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
        self.set_selected(sel[idx], state);
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

    /// The session the mux is DISPLAYING for the selection's row: the selection's own session
    /// (session/window row) or, on a host row, the host's recent session — the same
    /// resolution `target_for` uses. Lets the terminal-view follow descend from a host.
    fn displayed_session(&self, state: &crate::state::State) -> Option<Session> {
        match self.current_ref()? {
            RowRef::Session(s) => Some(s.clone()),
            RowRef::Window { sess, .. } => Some(sess.clone()),
            RowRef::Host { source, .. } => state
                .groups
                .iter()
                .find(|g| &g.source == source)
                .and_then(|g| tree::first_visible_session(g, &state.filter)),
            _ => None,
        }
    }

    fn current_host_unreachable(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { unreachable, .. }) if *unreachable)
    }

    // --- preview ------------------------------------------------------------

    fn on_focus_changed(&mut self, state: &crate::state::State) {
        self.terminal_view_target = match self.current_ref() {
            Some(r) => {
                let (source, target) = tree::target_for(r, &state.groups, &state.filter);
                TerminalViewTarget { source, target }
            }
            None => TerminalViewTarget::default(),
        };
    }

    /// The session/window the selection is currently on, used by the app to
    /// `switch-client` on every selection move (`select = attach`). Returns `Some`
    /// only for session, window, or host-with-session rows; `None` for pane,
    /// loading, and empty-host rows.
    pub fn current_attach_target(&self, state: &crate::state::State) -> Option<TerminalViewTarget> {
        let r = self.current_ref()?;
        let (source, target) = tree::target_for(r, &state.groups, &state.filter);
        if target.is_empty() {
            None
        } else {
            Some(TerminalViewTarget { source, target })
        }
    }

    /// The host (source alias) the selection is on, or `None` on a pane/loading row.
    /// The app ensures this host's control-mode client is connected on every
    /// selection move, so the host's `list-sessions` populates the tree even before
    /// any session is selected (a control-mode client is the only session source).
    pub fn current_host(&self) -> Option<String> {
        self.current_source()
    }

    /// Moves the tree selection to window `window` of `source`/`session` when the
    /// selection is currently within THAT session's subtree — on its session row OR any
    /// of its window rows. Used to follow the displayed session's active-window
    /// change; the app gates this on TERMINAL focus, where the user is no longer
    /// driving the tree selection (stdin goes to the PTY), so following from the session
    /// row mirrors the mux without yanking a tree-navigating user. A no-op when the
    /// selection is on a different host/session. Returns whether it moved.
    pub fn select_window(
        &mut self,
        source: &str,
        session: &str,
        window: i64,
        state: &crate::state::State,
    ) -> bool {
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
                self.set_selected(i, state);
                true
            }
            _ => false,
        }
    }

    /// Moves the tree selection to the session row whose address (`source/session`)
    /// is `address`. The semantic target of `Action::Switch` — addresses a row by
    /// identity, not a screen position or a relative step, so an agent driving ctl
    /// lands on the right session regardless of how the tree is currently ordered.
    /// A no-op (returns false) when no such row exists or the selection is already there.
    pub fn select_address(&mut self, address: &str, state: &crate::state::State) -> bool {
        let target = self
            .rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == address));
        match target {
            Some(i) if i != self.selected => {
                self.user_moved = true;
                self.set_selected(i, state);
                true
            }
            _ => false,
        }
    }

    /// Moves the selection to the ACTIVE window row of the DISPLAYED session (read from
    /// cached pane data) — from a session row, a window row, OR a host row (which
    /// descends into the host's recent session). Used when focus moves to the terminal
    /// so the tree view mirrors the window the mux is showing (#3). A no-op when the
    /// displayed session or its active window is unknown (e.g. panes not yet loaded, or
    /// an unreachable host). Returns whether it moved.
    pub fn select_active_window(&mut self, state: &mut crate::state::State) -> bool {
        let Some(sess) = self.displayed_session(state) else {
            return false;
        };
        let addr = sess.address();
        let Some(window) = state
            .panes
            .get(&addr)
            .and_then(|ws| ws.iter().find(|w| w.active))
            .map(|w| w.index)
        else {
            return false;
        };
        self.select_window(&sess.source, &sess.name, window, state)
    }

    /// Marks `window` as the active window of `source`/`session` in the cached
    /// pane data, flipping the bold/italic marker WITHOUT a full inventory refetch
    /// (the control-client probe resolves an external `%session-window-changed` to
    /// the new active window; a blanket refetch per change would storm the loop).
    /// Returns whether the active window actually changed.
    pub fn set_active_window(
        &mut self,
        source: &str,
        session: &str,
        window: i64,
        state: &mut crate::state::State,
    ) -> bool {
        let addr = crate::session::address_of(source, session);
        let Some(windows) = state.panes.get_mut(&addr) else {
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
            self.rebuild(state);
            self.restore_focus(prior, state);
        }
        changed
    }

    // --- key handling -------------------------------------------------------

    /// Open the modal keys help modal. In tree focus any key then dismisses it (see
    /// `handle_key`); [`toggle_help`] is the focus-independent open/close entry point.
    pub fn show_help(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        state.modal = Some(Modal::Help);
    }

    /// Toggle the keys help modal. Driven by `prefix ?` in EITHER focus so help opens
    /// and closes the same way regardless of which pane holds focus.
    pub fn toggle_help(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Help)) {
            state.modal = None;
        } else {
            self.dismiss_modals(state);
            state.modal = Some(Modal::Help);
        }
    }

    /// Closes any open modal and resets the popup drag position. The single `popup`
    /// Option already makes the modals mutually exclusive (opening one drops the rest);
    /// this is the explicit close + drag reset used by every opener and on dismissal.
    fn dismiss_modals(&mut self, state: &mut crate::state::State) {
        state.modal = None;
        self.popup_geo.reset();
    }

    /// True while a modal popup is being border-dragged; the app routes every
    /// mouse event here until release, like the view border drag / menu hold.
    pub fn popup_drag_active(&self) -> bool {
        self.popup_geo.drag_active()
    }

    /// A left press on the active modal popup's border begins a move-drag. Returns
    /// true iff it grabbed (so the app consumes the event).
    pub fn begin_popup_drag(&mut self, col: u16, row: u16, state: &crate::state::State) -> bool {
        self.popup_geo
            .begin_drag(col, row, state.is_modal_popup_open())
    }

    /// Updates the popup offset from the selection while a border-drag is active.
    pub fn drag_popup(&mut self, col: u16, row: u16) {
        self.popup_geo.drag(col, row);
    }

    /// Ends a border-drag.
    pub fn end_popup_drag(&mut self) {
        self.popup_geo.end_drag();
    }

    /// Modal help input, tmux view-mode style. While the modal is open it captures
    /// the whole key read (returns true ⇒ consumed — nothing reaches the tree or the
    /// terminal view); `q` or Esc closes it, every other key is swallowed. Returns false
    /// when help is closed, so the read falls through to normal routing. The single
    /// owner of help dismissal — the app calls it above the tree/terminal split, so the
    /// behavior is identical in both focuses.
    pub fn feed_help_key(&mut self, bytes: &[u8], state: &mut crate::state::State) -> bool {
        modal::feed_help(&mut state.modal, bytes)
    }

    /// Handles one key against the switcher. Navigation/modal-open keys mutate the
    /// switcher's own view state and `state.modal` directly and return no command;
    /// the keys that COMMIT a slow mux action (Enter on an input, `y` on a kill
    /// confirm) return the [`Command`]s `State::apply` produced for the run loop to
    /// dispatch (off-loop `run_op`). The caller dispatches the returned commands; an
    /// empty vec means there was no effect.
    pub fn handle_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        if matches!(state.modal, Some(Modal::Input(_))) {
            return self.handle_input_key(ev, state);
        }
        if matches!(state.modal, Some(Modal::Kill(_))) {
            return self.resolve_kill(ev, state);
        }
        // A flash is a transient error/message — it lives only until the next key. Clear
        // it here so navigation (or any key) restores the normal help
        // hint_bar; actions below may set a fresh one, which survives because this runs first.
        state.chrome.flash.clear();
        match ev.code {
            KeyCode::Enter => {}
            // ↑/↓ (and k/j) move between SIBLINGS at the current tree level (next/prev
            // node at the same depth); →/← (and l/h) change level — → descends to the
            // first child, ← ascends to the parent. hjkl mirror the arrows. (#1, #2)
            KeyCode::Up | KeyCode::Char('k') => self.move_sibling(-1, state),
            KeyCode::Down | KeyCode::Char('j') => self.move_sibling(1, state),
            KeyCode::Right | KeyCode::Char('l') => self.descend(state),
            KeyCode::Left | KeyCode::Char('h') => self.ascend(state),
            KeyCode::PageUp => self.move_selection(-10, state),
            KeyCode::PageDown => self.move_selection(10, state),
            KeyCode::Home => self.move_to(0, state),
            KeyCode::End => self.move_to(-1, state),
            KeyCode::Char(c) => match c {
                '/' => self.open_input(InputMode::Filter, state),
                'n' => self.open_new(state),
                'R' => self.open_input(InputMode::Rename, state),
                'x' => self.arm_kill(state),
                'r' => self.request_rescan(state),
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    // --- input row ----------------------------------------------------------

    fn open_input(&mut self, mode: InputMode, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        match mode {
            InputMode::Filter => {
                state.modal = Some(Modal::Input(Input {
                    mode,
                    label: " filter sessions".into(),
                    buffer: state.filter.clone(),
                    source: None,
                    sess: None,
                    target: None,
                }));
            }
            InputMode::Rename => match self.current_ref().cloned() {
                Some(RowRef::Host { .. }) => {
                    state.flash("cannot rename a host");
                }
                Some(RowRef::Session(sess)) => {
                    state.modal = Some(Modal::Input(Input {
                        mode,
                        label: " rename session".into(),
                        buffer: sess.name.clone(),
                        source: None,
                        sess: Some(sess),
                        target: None,
                    }));
                }
                Some(RowRef::Window { sess, window }) => {
                    let win_name = self
                        .window_name(&sess.address(), window, state)
                        .unwrap_or_default();
                    let target = crate::mux::window_target(&sess.name, window);
                    state.modal = Some(Modal::Input(Input {
                        mode,
                        label: " rename window".into(),
                        buffer: win_name,
                        source: Some(sess.source.clone()),
                        sess: Some(sess),
                        target: Some(target),
                    }));
                }
                _ => {}
            },
            // New/NewWindow/SplitWindow are opened by `open_new` (level-aware).
            InputMode::New | InputMode::NewWindow | InputMode::SplitWindow => {}
        }
    }

    /// The level-aware `n` action: a new SESSION on a host row, a new WINDOW on a
    /// session row, or a new PANE (split) on a window row (prompting the split
    /// direction). The prompt context is captured up front so a streamed selection
    /// move cannot retarget it.
    fn open_new(&mut self, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        if self.current_host_unreachable() {
            state.flash("host unreachable — cannot create here");
            return;
        }
        let Some(reference) = self.current_ref().cloned() else {
            return;
        };
        state.modal = match reference {
            RowRef::Host { source, .. } => Some(Modal::Input(Input {
                mode: InputMode::New,
                label: " new session name (empty = auto)".into(),
                buffer: String::new(),
                source: Some(source),
                sess: None,
                target: None,
            })),
            RowRef::Session(sess) => Some(Modal::Input(Input {
                mode: InputMode::NewWindow,
                label: format!(" new window in {} (name optional)", sess.name),
                buffer: String::new(),
                source: Some(sess.source.clone()),
                sess: Some(sess),
                target: None,
            })),
            RowRef::Window { sess, window } => {
                let target = crate::mux::window_target(&sess.name, window);
                Some(Modal::Input(Input {
                    mode: InputMode::SplitWindow,
                    label: " split [v]ertical / [h]orizontal (default v)".into(),
                    buffer: String::new(),
                    source: Some(sess.source.clone()),
                    sess: Some(sess),
                    target: Some(target),
                }))
            }
            RowRef::Pane | RowRef::Loading => None,
        };
    }

    fn close_input(&mut self, state: &mut crate::state::State) {
        state.modal = None;
    }

    fn handle_input_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        match ev.code {
            KeyCode::Enter => {
                let (mode, val, source, sess, target) = {
                    let Some(Modal::Input(input)) = &state.modal else {
                        return Vec::new();
                    };
                    (
                        input.mode,
                        input.buffer.trim().to_string(),
                        input.source.clone(),
                        input.sess.clone(),
                        input.target.clone(),
                    )
                };
                // Close the input first so a queue helper that early-returns on a
                // validation failure (empty/unchanged name) still dismisses the modal.
                self.close_input(state);
                match mode {
                    InputMode::Filter => {
                        state.filter = val;
                        self.rebuild(state);
                        Vec::new()
                    }
                    InputMode::New => self.queue_create(source, &val, state),
                    InputMode::NewWindow => self.queue_new_window(source, sess, &val, state),
                    InputMode::SplitWindow => self.queue_split(source, sess, target, &val, state),
                    InputMode::Rename => {
                        if target.is_some() {
                            self.queue_rename_window(source, sess, target, &val, state)
                        } else {
                            self.queue_rename(sess, &val, state)
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.close_input(state);
                Vec::new()
            }
            KeyCode::Backspace => {
                if let Some(Modal::Input(input)) = state.modal.as_mut() {
                    input.buffer.pop();
                }
                Vec::new()
            }
            KeyCode::Char(c) => {
                if let Some(Modal::Input(input)) = state.modal.as_mut() {
                    input.buffer.push(c);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Test/host hook: set the active input buffer directly.
    pub fn set_input_text(&mut self, text: &str, state: &mut crate::state::State) {
        if let Some(Modal::Input(input)) = state.modal.as_mut() {
            input.buffer = text.to_string();
        }
    }

    /// Resolves a create into an [`Action::CreateSession`] and folds it through
    /// `State::apply`, returning the resulting [`Command`] (a `RunOp`) for the run
    /// loop to dispatch off-loop. The network call is NOT made here, so the
    /// key-handling path never blocks on an ssh round-trip; [`run_op`] performs it
    /// off-loop and [`Switcher::apply_op_result`] folds the result in.
    fn queue_create(
        &mut self,
        source: Option<String>,
        name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let Some(source) = source else {
            return Vec::new();
        };
        state.apply(Action::CreateSession {
            source,
            name: name.to_string(),
        })
    }

    /// Resolves a new-window in the captured session (the `n` action on a session
    /// row) into an [`Action::NewWindow`]. An empty name lets the mux auto-name it.
    fn queue_new_window(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess)) = (source, sess) else {
            return Vec::new();
        };
        state.apply(Action::NewWindow {
            source,
            session: sess.name,
            name: name.to_string(),
        })
    }

    /// Resolves a split of the captured window target (the `n` action on a window
    /// row) into an [`Action::SplitWindow`]. The direction defaults to vertical
    /// unless the buffer starts with `h`.
    fn queue_split(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        dir: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return Vec::new();
        };
        let vertical = !dir.trim().eq_ignore_ascii_case("h");
        state.apply(Action::SplitWindow {
            source,
            target,
            session: sess.name,
            vertical,
        })
    }

    /// The current name of window `index` under the session at `sess_addr`, if its panes are loaded.
    fn window_name(
        &self,
        sess_addr: &str,
        index: i64,
        state: &crate::state::State,
    ) -> Option<String> {
        state
            .panes
            .get(sess_addr)
            .and_then(|ws| ws.iter().find(|w| w.index == index))
            .map(|w| w.name.clone())
    }

    /// Resolves a rename into an [`Action::RenameSession`] after the synchronous
    /// validation that needs no network. See [`Switcher::queue_create`] for why the
    /// op is deferred off-loop.
    fn queue_rename(
        &mut self,
        sess: Option<Session>,
        new_name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let Some(sess) = sess else {
            return Vec::new();
        };
        if new_name.is_empty() || new_name == sess.name {
            return Vec::new();
        }
        if new_name.starts_with('-') {
            // the mux silently no-ops a '-'-leading name (getopt eats it) — refuse.
            state.flash("rename: name cannot start with '-'");
            return Vec::new();
        }
        state.apply(Action::RenameSession {
            sess,
            new_name: new_name.to_string(),
        })
    }

    fn queue_rename_window(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        new_name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return Vec::new();
        };
        let cur = target
            .rsplit(':')
            .next()
            .and_then(|i| i.parse::<i64>().ok())
            .and_then(|idx| self.window_name(&sess.address(), idx, state));
        if new_name.is_empty() || cur.as_deref() == Some(new_name) {
            return Vec::new();
        }
        if new_name.starts_with('-') {
            state.flash("rename: name cannot start with '-'");
            return Vec::new();
        }
        state.apply(Action::RenameWindow {
            source,
            session: sess.name,
            target,
            new_name: new_name.to_string(),
        })
    }

    /// Applies a completed [`MuxOp`](crate::model::MuxOp)'s [`OpResult`] to the
    /// in-memory tree. The result is applied on the event loop after `run_op`
    /// returns off-loop, so a slow ssh round-trip never blocks rendering. State
    /// owns the inventory fold ([`State::fold_op_result`](crate::state::State::fold_op_result));
    /// the switcher only rebuilds its rows + restores the cursor per the returned
    /// [`OpFollow`].
    pub fn apply_op_result(&mut self, result: OpResult, state: &mut crate::state::State) {
        match state.fold_op_result(result) {
            OpFollow::Reselect(addr) => {
                self.rebuild(state);
                if let Some(i) = self.row_of_session(&addr) {
                    self.user_moved = true;
                    self.set_selected(i, state);
                }
            }
            OpFollow::Rebuild => self.rebuild(state),
            OpFollow::RebuildPreservingFocus => {
                let prior = self.capture_focus();
                self.rebuild(state);
                self.restore_focus(prior, state);
            }
            OpFollow::Flash(message) => {
                state.flash(message);
            }
        }
    }

    fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == address))
    }

    // --- kill (confirm popup) -----------------------------------------------

    fn arm_kill(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        match self.current_ref().cloned() {
            Some(RowRef::Host { .. }) => {
                state.flash("cannot kill a host");
            }
            Some(RowRef::Session(sess)) => {
                state.modal = Some(Modal::Kill(PendingKill::Session(sess)));
            }
            Some(RowRef::Window { sess, window }) => {
                let target = crate::mux::window_target(&sess.name, window);
                state.modal = Some(Modal::Kill(PendingKill::Window {
                    source: sess.source.clone(),
                    session: sess.name.clone(),
                    target,
                }));
            }
            _ => {}
        }
    }

    fn resolve_kill(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        // tmux confirm-before semantics: only y/Y confirms; any other key — n, Esc, or
        // anything else — cancels (the pending confirm is taken either way).
        let confirmed = matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        let Some(Modal::Kill(armed)) = state.modal.take() else {
            return Vec::new();
        };
        if !confirmed {
            return Vec::new();
        }
        let action = match armed {
            PendingKill::Session(sess) => Action::KillSession { sess },
            PendingKill::Window {
                source,
                session,
                target,
            } => Action::KillWindow {
                source,
                session,
                target,
            },
        };
        state.apply(action)
    }

    // --- refresh ------------------------------------------------------------

    /// Resets every host to its scanning skeleton and signals the event loop to
    /// re-kick the streaming probes (the `r` re-scan) — sessions and panes stream
    /// back in exactly as on first launch. The selection does not drift: the selection
    /// parks on the focused node's parent host for the skeleton phase (every session
    /// row just vanished) and `rescan_reselect` returns it to the exact session the
    /// instant that host re-streams.
    pub fn request_rescan(&mut self, state: &mut crate::state::State) {
        let (reselect, parent) = match self.current_ref() {
            Some(RowRef::Session(s)) => (Some(s.address()), Some(s.source.clone())),
            Some(RowRef::Window { sess, .. }) => (Some(sess.address()), Some(sess.source.clone())),
            Some(RowRef::Host { source, .. }) => (None, Some(source.clone())),
            _ => (None, None),
        };
        self.rescan_reselect = reselect;
        state.scanning = state.groups.iter().map(|g| g.source.clone()).collect();
        for g in state.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        state.panes.clear();
        state.panes_loaded.clear();
        self.rescan_kick = true;
        self.reattach_kick = true;
        self.rebuild(state);
        // Park on the parent host, whose row survives the clear — not the last-host
        // landing a removal-fallback would pick when every session vanishes at once.
        if let Some(src) = parent {
            if let Some(i) = self
                .rows
                .iter()
                .position(|r| matches!(&r.reference, RowRef::Host { source, .. } if *source == src))
            {
                self.set_selected(i, state);
            }
        }
    }

    /// Streams in one source's `list-sessions` outcome: clears its scanning
    /// state and replaces that host's sessions (reachable) or records its failure
    /// (unreachable). The host authoritatively owns its session list.
    pub fn apply_source_result(
        &mut self,
        source: String,
        mut sessions: Vec<Session>,
        err: Option<String>,
        state: &mut crate::state::State,
    ) {
        let prior = self.capture_focus();
        // Recency ordering is applied ONLY to a scan-driven result (launch or the `r`
        // re-scan — the source is still in `state.scanning`). A routine poll / %-event
        // refetch preserves the established order instead, so the tree does not
        // re-sort under the user: xmux pre-attaches every session, which churns the
        // mux-reported `last_attached`, and re-sorting on it would reshuffle the tree
        // on every ~1.5s poll.
        let was_scanning = state.scanning.remove(&source);
        let existing = state.groups.iter().position(|g| g.source == source);
        if was_scanning {
            tree::sort_by_recency(&mut sessions);
        } else if let Some(i) = existing {
            sessions = tree::reorder_preserving(sessions, &state.groups[i].sessions);
        }
        match existing {
            Some(i) => {
                state.groups[i].err = err;
                state.groups[i].sessions = sessions;
            }
            None => state.groups.push(Group {
                source,
                err,
                sessions,
            }),
        }
        // A scan-driven result also re-establishes the host-group order (local pinned
        // first, then remotes by recency), materialised into `state.groups`; a routine
        // poll leaves the group order frozen.
        if was_scanning {
            state.groups = tree::order_groups(&state.groups);
        }
        self.rebuild(state);
        self.restore_focus(prior, state);
    }

    /// Streams in one session's `list-panes` outcome, clearing its loading
    /// placeholder. An empty `panes` (a failed/timed-out fetch) still resolves the
    /// session — it shows no children rather than spinning forever.
    pub fn apply_panes(
        &mut self,
        address: String,
        panes: Vec<WindowPanes>,
        state: &mut crate::state::State,
    ) {
        let prior = self.capture_focus();
        state.panes_loaded.insert(address.clone());
        state.panes.insert(address, panes);
        self.rebuild(state);
        self.restore_focus(prior, state);
    }

    /// Captures the selection state needed to restore or gracefully redirect focus
    /// after a rebuild.
    fn capture_focus(&self) -> PriorFocus {
        PriorFocus {
            reference: self.current_ref().cloned(),
            selected: self.selected,
            indent: self.rows.get(self.selected).map(|r| r.indent).unwrap_or(0),
        }
    }

    /// After a streamed update rebuilds the rows: if the user has driven the
    /// selection, keep it on the focused node when it survives; if the node
    /// vanished (killed/removed), land on the previous sibling at the same
    /// indent or, when there is none, the parent. An untouched selection follows
    /// the rebuild's recency preselect.
    fn restore_focus(&mut self, prior: PriorFocus, state: &crate::state::State) {
        // A pending re-scan reselect returns the selection to its session the instant that
        // session re-streams — but only while the selection still sits where the re-scan
        // parked it (that session or its parent host). If the user has navigated
        // elsewhere in the skeleton meanwhile, the pending reselect is dropped so it
        // never yanks them back.
        if let Some(addr) = self.rescan_reselect.clone() {
            let parked = match prior.reference.as_ref() {
                Some(RowRef::Host { source, .. }) => {
                    crate::session::source_of(&addr) == source.as_str()
                }
                Some(RowRef::Session(s)) => s.address() == addr,
                Some(RowRef::Window { sess, .. }) => sess.address() == addr,
                _ => false,
            };
            if parked {
                if let Some(i) = self
                    .rows
                    .iter()
                    .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == addr))
                {
                    self.rescan_reselect = None;
                    self.set_selected(i, state);
                    return;
                }
            } else {
                self.rescan_reselect = None;
            }
        }
        if !self.user_moved {
            return;
        }
        let Some(focus) = prior.reference.as_ref() else {
            return;
        };
        if let Some(i) = self.row_matching(focus) {
            self.set_selected(i, state);
            return;
        }
        // The focused node vanished (killed/removed): land on the previous sibling
        // at its level, else the parent.
        if let Some(i) = self.fallback_after_removal(prior.indent, prior.selected) {
            self.set_selected(i, state);
        }
    }

    /// The row to land on after the focused node vanished: the previous selectable
    /// sibling at `indent`, else the nearest preceding selectable row at a shallower
    /// indent (the parent). Operates on the freshly rebuilt `self.rows`.
    fn fallback_after_removal(&self, indent: usize, prior_selected: usize) -> Option<usize> {
        let prior = &self.rows[..prior_selected.min(self.rows.len())];
        let prev_sibling = prior
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| r.indent == indent && r.selectable())
            .map(|(i, _)| i);
        prev_sibling.or_else(|| {
            prior
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.indent < indent && r.selectable())
                .map(|(i, _)| i)
        })
    }

    /// The row index targeting the same node as `focus`, if it survives a
    /// rebuild — so a re-scan keeps the selection in place rather than snapping to
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

    /// Single click: move the selection to the clicked row (select; never attach).
    pub fn mouse_select(&mut self, col: u16, row: u16, state: &crate::state::State) {
        if !self.in_tree(col, row) {
            return;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
        if self.rows.get(idx).is_some_and(Row::selectable) {
            self.user_moved = true;
            self.set_selected(idx, state);
        }
    }

    /// Double click: selects the clicked row (the preceding single click already
    /// moved the selection; with select=attach there is no separate attach action).
    pub fn mouse_attach(&mut self, col: u16, row: u16, state: &crate::state::State) {
        self.mouse_select(col, row, state);
    }

    /// Scroll wheel: move the selection (panes skipped) in the given direction.
    pub fn mouse_scroll(&mut self, down: bool, state: &crate::state::State) {
        self.move_selection(if down { 1 } else { -1 }, state);
    }

    // --- context menu -------------------------------------------------------

    /// Right-button press at 0-based screen (col,row): opens that tree row's menu if
    /// it lands on a selectable row that has items. Does NOT move the tree selection —
    /// the gesture only remembers the target, so no background attach fires mid-hold.
    /// Returns true iff a menu opened (so the app knows to consume the event).
    pub fn menu_open(&mut self, col: u16, row: u16, state: &mut crate::state::State) -> bool {
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
        let items = modal::menu_items(&target);
        if items.is_empty() {
            return false;
        }
        let title = modal::menu_title(&target);
        let rect = modal::menu_rect(col, row, &items, &title, self.screen_area);
        // No item is pre-highlighted, and the box opens just below the pointer (see
        // menu_rect) — so an accidental right-click that releases without dragging onto
        // an item does nothing. Selecting is a deliberate move down onto an item.
        state.modal = Some(Modal::Menu(Menu {
            target,
            title,
            rect,
            items,
            hovered: None,
        }));
        true
    }

    /// Mouse moved while the menu is held: highlight the item under the selection. Over the
    /// box but off an item (the title border) keeps the current highlight; only dragging
    /// fully OUTSIDE the box clears it, so releasing there cancels.
    pub fn menu_hover(&mut self, col: u16, row: u16, state: &mut crate::state::State) {
        modal::menu_hover(&mut state.modal, col, row);
    }

    /// Right-button up: act on the hovered item against the (re-located) target row,
    /// then close the menu. Released off-menu (no hovered item) cancels. The target is
    /// re-found by identity so a rebuild during the hold can't act on a stale node.
    pub fn menu_release(&mut self, state: &mut crate::state::State) -> MenuOutcome {
        let Some(Modal::Menu(menu)) = state.modal.take() else {
            return MenuOutcome::None;
        };
        let Some(i) = menu.hovered else {
            return MenuOutcome::None;
        };
        let item = menu.items[i];
        let Some(idx) = self
            .rows
            .iter()
            .position(|r| same_node(&r.reference, &menu.target))
        else {
            return MenuOutcome::None;
        };
        // The delegated methods act on the current selection, so land it on the target,
        // run the action (which CAPTURES the target by value), then for everything
        // EXCEPT focus restore the selection. A lingering selection move would change the
        // selection → trigger an attach and the events it spawns → rebuild the tree,
        // which clears an armed kill confirm (pending_kill) before the user can answer
        // y/n, and needlessly switches the displayed session. focus is the one item
        // that intends to move there.
        let prior = self.capture_focus();
        self.user_moved = true;
        self.set_selected(idx, state);
        match item {
            MenuItem::Focus => {
                // For a window, optimistically mark it active in the cache. Otherwise the
                // terminal-view selection-follow (`select_active_window`, run before the attach's
                // select-window lands) would yank the selection back to the session's previous
                // active window — so focusing a different window of the already-displayed
                // session did nothing. The real select-window follows from the selection.
                if let RowRef::Window { sess, window } = &menu.target {
                    self.set_active_window(&sess.source, &sess.name, *window, state);
                }
                MenuOutcome::FocusTerminal
            }
            MenuItem::NewSession | MenuItem::NewWindow => {
                self.open_new(state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
            MenuItem::Rename => {
                self.open_input(InputMode::Rename, state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
            MenuItem::Kill => {
                self.arm_kill(state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
        }
    }

    /// Close the menu without acting (app watchdog: a keystroke ends the gesture).
    /// Only a menu is cleared — a centered popup, if somehow open, is left intact.
    pub fn menu_cancel(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Menu(_))) {
            state.modal = None;
        }
    }
}

/// Picks the first (longest) candidate whose width fits `width`, falling back
/// to the last (shortest) when even that does not fit.
pub(crate) fn fit(candidates: &[String], width: u16) -> String {
    let w = width as usize;
    candidates
        .iter()
        .find(|c| UnicodeWidthStr::width(c.as_str()) <= w)
        .cloned()
        .unwrap_or_else(|| candidates.last().cloned().unwrap_or_default())
}

/// Adds a space each side so the reverse-video selection has breathing room.
fn pad_label(s: &str) -> String {
    format!(" {s} ")
}

/// Whether two row references target the same selectable node (host by source,
/// session/window by address), used to keep the selection across a re-scan.
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

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod tests_support;
