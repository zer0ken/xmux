//! The interactive session switcher: a two-region navigator (a flat, MRU-ordered
//! nav list of session cards on one side, the selected session's live terminal view
//! on the other), the nav carrying its own status line along its bottom. ratatui is
//! immediate-mode, so this owns
//! its state machine, the flattened card model, key/mouse handling, and a render pass
//! that draws to either the live terminal or a headless `TestBackend` (the control
//! channel's `dump`).

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, ListState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{Action, Command};
use crate::session::Session;
use crate::ui::chrome::ViewScreen;
use crate::ui::modal::{self, Input, InputMode, Modal, PopupGeometry};
use crate::ui::tree::{self, Group, Row, RowRef};

use crate::ui::ops::OpFollow;
pub use crate::ui::ops::{run_op, OpResult, Ops};

/// Tree pane width: border + 1-cell inner padding each side + content.
pub const NAV_WIDTH: u16 = 48;

/// A navigation card's EXPANDED height: two screen rows (a context line over a
/// detail line). A collapsed card (see [`Switcher::card_collapsed`]) drops the
/// context line and is one row tall; the layout is measured from
/// [`Switcher::card_height`] alone, and the paint records the rect it gave each card, so
/// the screen-row-to-card mapping has one answer.
pub(super) const CARD_H: u16 = 2;

/// How much taller than wide a terminal cell is. A row is about two half-width columns
/// high in every font a terminal ships with, so an aspect measured in CELLS is not the
/// aspect the user sees: 60 columns over 30 rows is a square window, not a landscape one.
/// Every shape test multiplies the rows by this and compares real proportions.
pub(super) const CELL_ASPECT: u32 = 2;

/// Blank columns between two card columns in the portrait `Top` flow. One is enough to
/// part them: every card opens with its address column, so a gutter reads as a gap
/// between a name and the next number rather than two names running together.
pub(super) const COL_GUTTER: u16 = 1;

/// The glyph the side list's band rule is drawn from, repeated across the nav. A light
/// box-drawing line, so it parts the bands without reading as a border around either.
pub(super) const BAND_RULE: &str = "\u{2500}";

// The one colour every card's text reads in (separators and the accent target excepted),
// so the levels read as one neutral block with a single highlighted element. A function,
// not a const: the active palette (dark / light) is picked at runtime from the terminal
// background.
fn color_text() -> Color {
    crate::ui::palette::get().text
}
/// The card's number in the address column.
fn color_number() -> Color {
    crate::ui::palette::get().number
}
/// The `/` separator between a card line's parts.
fn color_separator() -> Color {
    crate::ui::palette::get().separator
}
/// The box-drawing connector (`├`/`└`) hanging a detail line under its context.
fn color_connector() -> Color {
    crate::ui::palette::get().connector
}
pub use crate::ui::chrome::ViewBorderColors;

/// Which way the two views stack. `Side` (default) puts the tree in a left column;
/// `Top` stacks the tree above the terminal once a side column would leave the terminal
/// no wider than it is tall, so a narrow phone-shaped terminal stays usable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewLayout {
    Side,
    Top,
}

/// The nav's live size, as one value: what the user set, and what is on screen this
/// frame. Both are settable while xmux runs (`prefix h`/`l` and a border drag set the
/// width, `prefix Ctrl+arrow` and a drag the `Top` height, and auto-hide takes the width
/// away entirely), so every consumer reads them from here rather than deriving either.
///
/// `natural` and `width` differ only while the nav is HIDDEN, and keeping both is the
/// point: the layout turnover is measured from `natural`, so hiding the nav cannot flip
/// the layout under the very keys that resize it, while the regions are cut from `width`,
/// which is what is actually on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavSize {
    /// The width the user set: the saved pref, `prefix h`/`l`, or a border drag.
    pub natural: u16,
    /// The width on screen this frame: `natural`, or 0 while the nav is hidden.
    pub width: u16,
    /// The `Top` band's height the user set; 0 means auto (~40% of the body).
    pub height: u16,
}

impl NavSize {
    /// The nav on screen at the width the user set.
    pub fn visible(natural: u16) -> Self {
        NavSize {
            natural,
            width: natural,
            height: 0,
        }
    }

    /// The nav hidden (auto-hide plus terminal focus). The width the user set travels with
    /// it, because the layout is measured from that width whether the nav is showing or not.
    pub fn hidden(natural: u16) -> Self {
        NavSize {
            natural,
            width: 0,
            height: 0,
        }
    }

    /// The same nav with the `Top` band height the user set (0 = auto).
    pub fn with_height(self, height: u16) -> Self {
        NavSize { height, ..self }
    }
}

/// Picks the layout from the TERMINAL VIEW's aspect, not the whole screen's: putting the
/// tree in a side column costs the terminal `nav_width + 1` columns, and if that would
/// leave the terminal view no wider than it is tall, the tree stacks on `Top` instead so
/// the terminal keeps full width. So a screen that is landscape overall can still go `Top`
/// once the tree squeezes the terminal into a square-or-taller shape. `nav_width` is the
/// width the tree would occupy in `Side` (the natural/unhidden width).
///
/// The aspect is the one the user SEES, not the one the cell counts state: a row is about
/// two columns tall ([`CELL_ASPECT`]), so 60 columns over 30 rows is square. Comparing the
/// counts directly would call that window landscape and keep the side column until the
/// terminal was half as wide as it looked.
///
/// The aspect is always measured AS IF the tree were in its side column, never from the
/// terminal's live size: going `Top` gives the terminal back the tree's columns and takes
/// the band's rows instead, so measuring the result would make the test flip its own input
/// and the layout oscillate at the boundary. One side of the comparison, one answer: the
/// side terminal is wider than tall (`x > y`) or it is not (`x <= y`).
pub fn view_layout(area: Rect, nav_width: u16) -> ViewLayout {
    let side_term_w = area.width.saturating_sub(nav_width.saturating_add(1)) as u32;
    let side_term_h = area.height as u32 * CELL_ASPECT;
    if side_term_w <= side_term_h {
        ViewLayout::Top
    } else {
        ViewLayout::Side
    }
}

/// The auto `Top`-layout tree height for a body of `body_rows` rows (before the hint bar row
/// is removed the caller passes `full_height - 1`). This is the seed a RELATIVE height resize
/// (prefix h/l in Top) starts from while `nav_height` is still 0 (auto), so the first key
/// adjusts the height the user actually sees.
pub fn default_nav_height(body_rows: u16) -> u16 {
    top_nav_height(body_rows)
}

/// The tree region's height in the `Top` layout: ~40% of the body, at least a few rows, but
/// never so tall the terminal loses its last rows. Composed with min/max (not `clamp`) so a
/// tiny body - where the floor would exceed the ceiling and `clamp` would panic - just yields
/// the small floor instead.
fn top_nav_height(body_h: u16) -> u16 {
    let want = (body_h as u32 * 2 / 5) as u16;
    let ceil = body_h.saturating_sub(3).max(1);
    want.max(3).min(ceil).max(1)
}

/// The screen regions the switcher draws into, derived ONCE per frame so the renderer,
/// the PTY sizing, and mouse hit-testing all agree (one geometry, no divergence). The
/// tree and terminal split the whole area horizontally (`Side`, sized by `nav_width`)
/// or vertically (`Top`, sized by `nav_height`), parted by the one-cell view border;
/// the hint bar is the BOTTOM of the nav region, not a full-width strip, so it reads
/// as the nav's own status line and the terminal view keeps every row it owns.
/// `nav_width == 0` is the tree-hidden sentinel: the terminal owns the whole area (and
/// there is no nav to carry a hint bar). `nav_height == 0` means the `Top` height is
/// auto (~40% of the area).
pub struct Regions {
    pub layout: ViewLayout,
    pub tree: Rect,
    pub view_border: Rect,
    pub terminal: Rect,
    pub hint_bar: Rect,
}

/// The Top-layout tree height: a user-set `nav_height` (dragged border) clamped so both
/// views keep room, or the auto ~40% when `nav_height == 0`. min/max (not `clamp`) so a
/// tiny body cannot panic on inverted bounds.
fn top_nav_height_for(body_h: u16, nav_height: u16) -> u16 {
    if nav_height == 0 {
        top_nav_height(body_h)
    } else {
        nav_height.min(body_h.saturating_sub(2)).max(1)
    }
}

/// Splits a nav region into `(card list, hint bar)`: the hint bar takes the bottom
/// `hint_bar_h` rows, and the cards keep the rest. A nav too short to hold both gives
/// the whole region to the cards and no hint bar, so a tiny terminal still navigates.
fn split_nav(nav: Rect, hint_bar_h: u16) -> (Rect, Rect) {
    if nav.height <= hint_bar_h {
        return (nav, Rect::default());
    }
    let r = Layout::vertical([Constraint::Min(0), Constraint::Length(hint_bar_h)]).split(nav);
    (r[0], r[1])
}

pub fn compute_regions(area: Rect, nav: NavSize, hint_bar_h: u16) -> Regions {
    // The layout is decided from the width the user SET, never from the width on screen, so
    // hiding the nav cannot flip it; the hidden sentinel below still gives the whole area
    // to the terminal.
    let layout = view_layout(area, nav.natural);
    let (nav_width, nav_height) = (nav.width, nav.height);
    if nav_width == 0 {
        return Regions {
            layout,
            tree: Rect::default(),
            view_border: Rect::default(),
            terminal: area,
            hint_bar: Rect::default(),
        };
    }
    match layout {
        ViewLayout::Side => {
            let c = Layout::horizontal([
                Constraint::Length(nav_width),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            let (tree, hint_bar) = split_nav(c[0], hint_bar_h);
            Regions {
                layout,
                tree,
                view_border: c[1],
                terminal: c[2],
                hint_bar,
            }
        }
        ViewLayout::Top => {
            let th = top_nav_height_for(area.height, nav_height);
            let r = Layout::vertical([
                Constraint::Length(th),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            let (tree, hint_bar) = split_nav(r[0], hint_bar_h);
            Regions {
                layout,
                tree,
                view_border: r[1],
                terminal: r[2],
                hint_bar,
            }
        }
    }
}

/// A fully-populated snapshot of the reachable environment.
#[derive(Clone, Default)]
pub struct Scan {
    pub groups: Vec<Group>,
}

/// Snapshot of the selection taken before a rebuild so `restore_focus` can
/// recover or gracefully redirect it afterward.
struct PriorFocus {
    reference: Option<RowRef>,
    selected: usize,
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
    /// Signals the event loop to (re)kick the streaming probes - set on the
    /// initial seed and on an `r` re-scan; the loop reads + clears it.
    rescan_kick: bool,
    /// Signals the event loop to re-attach the CURRENT display: tear the (possibly
    /// detached / dead) attachment down so the next attach re-creates a fresh client.
    /// Set on an `r` re-scan - explicit, on-demand recovery for the viewed session.
    reattach_kick: bool,

    rows: Vec<Row>,
    selected: usize,
    /// The frozen MRU order of session addresses the flat card list follows. Rebuilt
    /// from global recency while any host is still scanning, then held so a routine
    /// poll never reshuffles cards under the user (xmux pre-attaches sessions, which
    /// churns `last_attached`); newly-appeared sessions append by recency. See
    /// [`Switcher::reorder`].
    nav_order: Vec<String>,

    terminal_view_target: TerminalViewTarget,
    /// The address of the session xmux is ITSELF running in, when it is inside one. The
    /// one address the terminal view refuses: see [`Switcher::is_own_session`].
    own_session: Option<String>,

    list_state: ListState,
    nav_inner: Rect,
    /// The card rects of the last paint, in either layout: mouse hit-testing reads them
    /// so a click lands on the card the user sees, whatever column it flowed into or
    /// whichever band it sits in. The paint is the only thing that decides a card's rect,
    /// so a click cannot land on a card the renderer put elsewhere.
    nav_cells: Vec<(usize, Rect)>,
    /// The leftmost drawn column of the `Top` column flow: the horizontal scroll
    /// position, moved only as far as keeping the selected card visible requires.
    nav_col_offset: usize,
    /// The view stacking as of the last render (Side vs Top), cached so key handling can
    /// route the arrows to match what is on screen without re-deriving the geometry. Set
    /// each frame by `render` from [`view_layout`].
    layout: ViewLayout,

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

mod columns;
mod input;
mod mouse;
mod render;
mod side;

impl Switcher {
    fn blank() -> Self {
        Switcher {
            user_moved: false,
            rescan_kick: false,
            reattach_kick: false,
            rows: Vec::new(),
            selected: 0,
            nav_order: Vec::new(),
            terminal_view_target: TerminalViewTarget::default(),
            own_session: None,
            list_state: ListState::default(),
            nav_inner: Rect::default(),
            nav_cells: Vec::new(),
            nav_col_offset: 0,
            layout: ViewLayout::Side,
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

    /// Seeds the switcher from the resolved source list alone - no probing - so
    /// the first frame paints host-skeleton rows, each in a scanning state, in
    /// tens of milliseconds. Streamed [`apply_source_result`]
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

    /// Names the session xmux is running in, so the terminal view can refuse it. The app
    /// calls this once at startup; outside a mux, and where the session could not be
    /// named, it is never called and nothing is refused.
    pub fn set_own_session(&mut self, address: Option<String>) {
        self.own_session = address;
    }

    /// Whether `(source, target)` addresses the session xmux is ITSELF running in.
    ///
    /// That session has a live grid like any other, and showing it is still refused:
    /// attaching to it puts a second client on the session that holds xmux, which moves
    /// the user's own client and paints xmux inside itself.
    fn is_own_session(&self, source: &str, target: &str) -> bool {
        match &self.own_session {
            Some(own) => !target.is_empty() && *own == crate::session::address_of(source, target),
            None => false,
        }
    }

    /// The view stacking as of the last render (Side vs Top). Lets the app route the
    /// tree-resize keys to the dimension the current layout resizes: WIDTH in Side, HEIGHT
    /// in the portrait Top layout.
    pub fn layout(&self) -> ViewLayout {
        self.layout
    }

    /// Takes the pending rescan-kick flag (true once after seeding or an `r`
    /// re-scan) - the event loop spawns the streaming probes when it is set.
    pub fn take_rescan_kick(&mut self) -> bool {
        std::mem::take(&mut self.rescan_kick)
    }

    /// Consumes the re-attach kick (set by an `r` re-scan): the loop tears down the
    /// current display attachment so the next attach re-creates a fresh client.
    pub fn take_reattach_kick(&mut self) -> bool {
        std::mem::take(&mut self.reattach_kick)
    }

    // --- tree model ---------------------------------------------------------

    fn rebuild(&mut self, state: &mut crate::state::State) {
        // Once the user has moved the selection, hold their current session selection
        // across this rebuild when it survives (matched by identity) - a routine rebuild
        // (local poll, remote %-event refetch) must NOT snap the selection back to the top
        // row, which would yank the displayed session out from under the user on every
        // poll (the selection thrash). The user_moved gate at the target below preserves the
        // launch behavior: an untouched selection preselects the top row (the local host,
        // index 0).
        let keep = self
            .rows
            .get(self.selected)
            .and_then(|r| match &r.reference {
                RowRef::Session { .. } => Some(r.reference.clone()),
                RowRef::Host { .. } => None,
            });

        // Refresh the frozen MRU order, then flatten follows it. Pure row generation
        // lives in `tree::flatten`; rebuild orchestrates capture → order → flatten →
        // preselect → restore around it.
        self.reorder(state);
        // The mux each card NAMES comes from one resolver, so a session card, its host's
        // card and the screen behind either cannot spell one mux three ways.
        let named_mux = |source: &str| state.chrome.source_mux(source).to_string();
        let rows = tree::flatten(
            &state.groups,
            &state.scanning,
            &state.filter,
            &self.nav_order,
            &named_mux,
        );

        self.rows = rows;
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

    /// Refreshes the frozen order (`nav_order`) the flat card list follows.
    ///
    /// Recency applies to the SOURCE, not to the session alone: a source's cards are
    /// contiguous, the sources run most-recently-used first, and a source's own sessions
    /// run most-recently-used first inside it. Global session recency would split a
    /// source, restating its `{host}/{mux}` context in two places, and the connector
    /// under a context line claims the cards below belong to it.
    ///
    /// One insertion rule produces all of that: a session lands after the last card of
    /// its own source, or at the end when its source has none yet. Fed addresses in
    /// recency order that yields the grouping directly, and it is also what keeps a
    /// session discovered later inside its group instead of stranded at the bottom.
    ///
    /// While any host is still scanning the order is rebuilt each time; once every host
    /// has settled it is held - entries still present keep their positions, so a routine
    /// poll never reshuffles cards under the user (xmux pre-attaches sessions, churning
    /// `last_attached`).
    fn reorder(&mut self, state: &crate::state::State) {
        if !state.scanning.is_empty() {
            self.nav_order.clear();
        }
        let mut recency: HashMap<String, i64> = HashMap::new();
        let mut source_of: HashMap<String, String> = HashMap::new();
        for g in &state.groups {
            if g.err.is_some() {
                continue;
            }
            for s in &g.sessions {
                let addr = s.address();
                recency.insert(addr.clone(), s.last_attached);
                source_of.insert(addr, s.source.clone());
            }
        }
        // Entries still present keep their positions (no churn on poll).
        let mut next: Vec<String> = self
            .nav_order
            .iter()
            .filter(|a| recency.contains_key(*a))
            .cloned()
            .collect();
        // Sessions new since the freeze, most recent first (ties by address).
        let mut newcomers: Vec<String> = recency
            .keys()
            .filter(|a| !next.contains(*a))
            .cloned()
            .collect();
        newcomers.sort_by(|a, b| recency[b].cmp(&recency[a]).then_with(|| a.cmp(b)));
        for addr in newcomers {
            let source = source_of.get(&addr);
            let after = next
                .iter()
                .rposition(|a| source_of.get(a) == source)
                .map(|i| i + 1)
                .unwrap_or(next.len());
            next.insert(after, addr);
        }
        self.nav_order = next;
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

    /// Whether card `i` renders collapsed to its detail line alone: a session /
    /// loading card whose `{host}/{mux}` context repeats the previous card's, so a
    /// run of sessions on one server reads grouped without restating the context.
    /// The SELECTED card never collapses - focus expands it to the full two-row
    /// card, so its context is always readable in place - and a host-state card
    /// never collapses (its host name IS the content).
    fn card_collapsed(&self, i: usize) -> bool {
        if self.list_state.selected() == Some(i) {
            return false;
        }
        self.hangs_under_prev(i)
    }

    /// Whether card `i` shares the previous card's `{host}/{mux}` context, so it CAN
    /// hang under it instead of restating it. The context test alone: whether it
    /// actually collapses also depends on the layout (the selected card expands in the
    /// `Side` list; a column's first card always states its context in the `Top` flow),
    /// so each layout applies its own rule over this one.
    fn hangs_under_prev(&self, i: usize) -> bool {
        let (Some(row), Some(prev)) = (
            self.rows.get(i),
            i.checked_sub(1).and_then(|p| self.rows.get(p)),
        ) else {
            return false;
        };
        if matches!(row.reference, RowRef::Host { .. })
            || matches!(prev.reference, RowRef::Host { .. })
        {
            return false;
        }
        let (host, mux, _) = context_of(row);
        let (prev_host, prev_mux, _) = context_of(prev);
        host == prev_host && mux == prev_mux
    }

    /// Whether card `i` is a SETTLED host-state card - a reachable empty host or an
    /// unreachable one - rendered as a single host row: a settled host has no session
    /// and no row of its own below the host name (the unreachable mark rides on the
    /// host row itself), so the card reads one line tall. A scanning host keeps its
    /// two rows, because its spinner has to stand somewhere the settled card's text
    /// stood.
    fn is_one_line_host(&self, i: usize) -> bool {
        matches!(
            self.rows.get(i).map(|r| &r.reference),
            Some(RowRef::Host {
                scanning: false,
                ..
            })
        )
    }

    /// The screen rows card `i` occupies: [`CARD_H`] expanded, one when collapsed or
    /// when the card is a single host row (a reachable empty host, see
    /// [`Switcher::is_one_line_host`]).
    fn card_height(&self, i: usize) -> u16 {
        if self.card_collapsed(i) || self.is_one_line_host(i) {
            1
        } else {
            CARD_H
        }
    }

    /// Where the nav's two bands meet: the first host-state card, the flatten having sunk
    /// every host with no session to show to the end of the list. `None` when no host card
    /// is on the list at all; whether a boundary actually parts anything (both bands need
    /// a card) is [`side::place`]'s to judge.
    fn band_boundary(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r.reference, RowRef::Host { .. }))
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

    /// Vertical navigation shared by ↑/↓, k/j, AND the plain scroll wheel, so the wheel
    /// moves the selection exactly as the arrows do: prev/next card linearly across the
    /// whole flat list (wraps). The flat card list has no levels, so this is a plain
    /// linear step - the same as [`Switcher::move_selection`].
    fn nav_vertical(&mut self, delta: isize, state: &crate::state::State) {
        self.move_selection(delta, state);
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
            RowRef::Session { sess } => Some(sess.source.clone()),
        }
    }

    fn current_host_unreachable(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { unreachable, .. }) if *unreachable)
    }

    /// Which host screen the terminal view shows in place of the grid, or `None` when it
    /// shows the grid. Only a selected HOST card earns one, and only once it has settled:
    /// unreachable names why it failed, empty names what to press. A host still scanning
    /// gets neither, because an in-flight state is the nav's to show (its card spins) and
    /// the view keeps the grid it already has.
    fn current_view_screen(&self, state: &crate::state::State) -> Option<ViewScreen> {
        // The session xmux runs in comes first: it is the one card with a grid the view
        // still refuses, and the screen is what stands in place of it.
        if let Some(addr) = self.current_screen_address(state) {
            if self.own_session.as_deref() == Some(addr.as_str()) {
                return Some(ViewScreen::SelfSession);
            }
        }
        let Some(RowRef::Host {
            source,
            unreachable,
            ..
        }) = self.current_ref()
        else {
            return None;
        };
        if *unreachable {
            return Some(ViewScreen::Unreachable);
        }
        if state.scanning.contains(source) {
            return None;
        }
        state
            .groups
            .iter()
            .any(|g| &g.source == source && g.sessions.is_empty())
            .then_some(ViewScreen::Empty)
    }

    /// The address the selected card would show, or `None` when it would show nothing.
    /// The address is what a refusal is keyed to, and it is what the screen writes as its
    /// headline, so both read the same value.
    fn current_screen_address(&self, state: &crate::state::State) -> Option<String> {
        let r = self.current_ref()?;
        let (source, target) = tree::target_for(r, &state.groups, &state.filter);
        (!target.is_empty()).then(|| crate::session::address_of(&source, &target))
    }

    /// What the view screen writes as its headline: the session ADDRESS for the
    /// self-session state, whose subject is one session, and the host for the two host
    /// states, whose subject is the host.
    pub(crate) fn view_screen_headline(
        &self,
        state: &crate::state::State,
        kind: ViewScreen,
    ) -> String {
        match kind {
            ViewScreen::SelfSession => self.current_screen_address(state).unwrap_or_default(),
            _ => self.current_source().unwrap_or_default(),
        }
    }

    // --- preview ------------------------------------------------------------

    fn on_focus_changed(&mut self, state: &crate::state::State) {
        self.terminal_view_target = match self.current_ref() {
            Some(r) => {
                let (source, target) = tree::target_for(r, &state.groups, &state.filter);
                // xmux's OWN session is not a terminal-view target. Emptying it here is
                // what makes the refusal total: the target is the one value the display
                // reconcile, the attach, and the mux-side switch all read, so none of
                // them can reach this session by another path.
                if self.is_own_session(&source, &target) {
                    TerminalViewTarget::default()
                } else {
                    TerminalViewTarget { source, target }
                }
            }
            None => TerminalViewTarget::default(),
        };
    }

    /// The session the selection is currently on, used by the app to
    /// `switch-client` on every selection move (`select = attach`). Returns `Some`
    /// for session, loading, and host-with-session rows; `None` for empty-host rows.
    pub fn current_attach_target(&self, state: &crate::state::State) -> Option<TerminalViewTarget> {
        let r = self.current_ref()?;
        let (source, target) = tree::target_for(r, &state.groups, &state.filter);
        if target.is_empty() || self.is_own_session(&source, &target) {
            None
        } else {
            Some(TerminalViewTarget { source, target })
        }
    }

    /// The host (source alias) the selection is on.
    /// The app ensures this host's control-mode client is connected on every
    /// selection move, so the host's `list-sessions` populates the tree even before
    /// any session is selected (a control-mode client is the only session source).
    pub fn current_host(&self) -> Option<String> {
        self.current_source()
    }

    /// Moves the tree selection to the session row whose address (`source/session`)
    /// is `address`. The semantic target of `Action::Switch` - addresses a row by
    /// identity, not a screen position or a relative step, so an agent driving ctl
    /// lands on the right session regardless of how the tree is currently ordered.
    /// A no-op (returns false) when no such row exists or the selection is already there.
    pub fn select_address(&mut self, address: &str, state: &crate::state::State) -> bool {
        let target = self
            .rows
            .iter()
            .position(|r| session_addr_of(&r.reference).as_deref() == Some(address));
        match target {
            Some(i) if i != self.selected => {
                self.user_moved = true;
                self.set_selected(i, state);
                true
            }
            _ => false,
        }
    }

    // --- refresh ------------------------------------------------------------

    /// Resets every host to its scanning skeleton and signals the event loop to
    /// re-kick the streaming probes (the `r` re-scan) - sessions and panes stream
    /// back in exactly as on first launch. The selection does not drift: the selection
    /// parks on the focused node's parent host for the skeleton phase (every session
    /// row just vanished) and `rescan_reselect` returns it to the exact session the
    /// instant that host re-streams.
    pub fn request_rescan(&mut self, state: &mut crate::state::State) {
        let (reselect, parent) = match self.current_ref() {
            Some(RowRef::Session { sess }) => (Some(sess.address()), Some(sess.source.clone())),
            Some(RowRef::Host { source, .. }) => (None, Some(source.clone())),
            None => (None, None),
        };
        self.rescan_reselect = reselect;
        state.scanning = state.groups.iter().map(|g| g.source.clone()).collect();
        for g in state.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        self.rescan_kick = true;
        self.reattach_kick = true;
        self.rebuild(state);
        // Park on the parent host, whose row survives the clear - not the last-host
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
        // re-scan - the source is still in `state.scanning`). A routine poll / %-event
        // refetch preserves the established order instead, so the tree does not
        // re-sort under the user: xmux pre-attaches every session, which churns the
        // mux-reported `last_attached`, and re-sorting on it would reshuffle the tree
        // on every ~1.5s poll.
        let was_scanning = state.scanning.remove(&source);
        // The failure run, counted where every result lands so no path can skip it: a
        // result that failed lengthens it, one that answered clears it. It is shown, not
        // acted on - see `State::failure_runs`.
        match &err {
            Some(_) => *state.failure_runs.entry(source.clone()).or_insert(0) += 1,
            None => {
                state.failure_runs.remove(&source);
            }
        }
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

    /// Adds a source that was not there at launch (a mux discovery answered) as a
    /// SCANNING host card, so it appears the moment it is found instead of at the next
    /// run. Idempotent: a source already in the nav is left exactly as it is.
    ///
    /// It APPENDS. The group order is frozen after launch, and a card the user is looking
    /// at must not move because another machine answered - the new card takes the bottom
    /// and its own first scan result sorts its sessions.
    pub fn add_source(&mut self, source: String, state: &mut crate::state::State) {
        if state.groups.iter().any(|g| g.source == source) {
            return;
        }
        let prior = self.capture_focus();
        state.scanning.insert(source.clone());
        state.groups.push(Group {
            source,
            err: None,
            sessions: Vec::new(),
        });
        self.rebuild(state);
        self.restore_focus(prior, state);
    }

    /// Drops a source whose MACHINE the roster no longer names, and everything the nav
    /// held for it. Idempotent: a source the nav does not show is left alone.
    ///
    /// Focus is restored exactly as a streamed rebuild restores it, so a selection
    /// sitting on the dropped card lands on the previous card instead of vanishing.
    pub fn remove_source(&mut self, source: &str, state: &mut crate::state::State) {
        if !state.groups.iter().any(|g| g.source == source) {
            return;
        }
        let prior = self.capture_focus();
        state.groups.retain(|g| g.source != source);
        state.scanning.remove(source);
        state.failure_runs.remove(source);
        self.rebuild(state);
        self.restore_focus(prior, state);
    }

    /// Captures the selection state needed to restore or gracefully redirect focus
    /// after a rebuild.
    fn capture_focus(&self) -> PriorFocus {
        PriorFocus {
            reference: self.current_ref().cloned(),
            selected: self.selected,
        }
    }

    /// After a streamed update rebuilds the cards: if the user has driven the
    /// selection, keep it on the focused card when it survives; if the card
    /// vanished (killed/removed), land on the previous card. An untouched selection
    /// follows the rebuild's recency preselect.
    fn restore_focus(&mut self, prior: PriorFocus, state: &crate::state::State) {
        // A pending re-scan reselect returns the selection to its session the instant that
        // session re-streams - but only while the selection still sits where the re-scan
        // parked it (that session or its parent host). If the user has navigated
        // elsewhere in the skeleton meanwhile, the pending reselect is dropped so it
        // never yanks them back.
        if let Some(addr) = self.rescan_reselect.clone() {
            let parked = match prior.reference.as_ref() {
                Some(RowRef::Host { source, .. }) => {
                    crate::session::source_of(&addr) == source.as_str()
                }
                Some(RowRef::Session { sess }) => sess.address() == addr,
                None => false,
            };
            if parked {
                if let Some(i) = self
                    .rows
                    .iter()
                    .position(|r| session_addr_of(&r.reference).as_deref() == Some(addr.as_str()))
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
        // The focused card vanished (killed/removed): land on the previous card.
        if let Some(i) = self.fallback_after_removal(prior.selected) {
            self.set_selected(i, state);
        }
    }

    /// The card to land on after the selected card vanished (killed/removed): the
    /// previous card, clamped into range. Operates on the freshly rebuilt `self.rows`.
    fn fallback_after_removal(&self, prior_selected: usize) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        Some(prior_selected.saturating_sub(1).min(self.rows.len() - 1))
    }

    /// The row index targeting the same node as `focus`, if it survives a
    /// rebuild - so a re-scan keeps the selection in place rather than snapping to
    /// the recency preselect.
    fn row_matching(&self, focus: &RowRef) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| same_node(&r.reference, focus))
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

/// The context parts of a card: `(host, mux, session)`. A host-state card
/// carries only its host; a session card names its session's host, mux
/// kind (empty when not yet known), and session name.
fn context_of(row: &Row) -> (&str, &str, &str) {
    // The MACHINE half, never the whole source id: a source id already carries the mux
    // when its machine serves several, and the card renders the mux as its own span, so
    // returning the id whole would read `local:zellij/zellij`. The mux comes off the card
    // itself, resolved once when the card was built, so every card on one source names its
    // mux the same way whatever each of them had to read it from.
    match &row.reference {
        RowRef::Host { source, .. } => (crate::session::machine_of(source), &row.mux, ""),
        RowRef::Session { sess } => (
            crate::session::machine_of(&sess.source),
            &row.mux,
            &sess.name,
        ),
    }
}

/// The session address a card belongs to (a session card), or `None` for a
/// host-state card. Lets selection tracking, kill-confirm survival, and
/// `select_address` treat a session card as that session.
fn session_addr_of(reference: &RowRef) -> Option<String> {
    match reference {
        RowRef::Session { sess } => Some(sess.address()),
        RowRef::Host { .. } => None,
    }
}

/// Whether two card references target the same card across a rebuild (host by
/// source, session by address), so the selection stays put on a poll / re-scan.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Host { source: x, .. }, RowRef::Host { source: y, .. }) => x == y,
        (RowRef::Host { .. }, _) | (_, RowRef::Host { .. }) => false,
        _ => session_addr_of(a) == session_addr_of(b),
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
