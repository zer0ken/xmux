//! The interactive session switcher: a two-region navigator (a flat nav list of
//! session cards in deterministic local→WSL→remote, name-sorted order on one side,
//! the selected session's live terminal view on the other), parted by one seam that
//! also carries the nav's overflow cues. ratatui is
//! immediate-mode, so this owns
//! its state machine, the flattened card model, key/mouse handling, and a render pass
//! that draws to either the live terminal or a headless `TestBackend` (the control
//! channel's `dump`).

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Position, Rect};
#[cfg(test)]
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{Action, Command, ViewScreen};
use crate::session::{Address, Session};
use crate::ui::modal::{self, Input, InputMode, Modal, PopupGeometry};
use crate::ui::tree::{self, Group, Row, RowRef};

use crate::state::OpFollow;
pub use crate::ui::ops::{run_login_follow_ups, run_op, OpResult, Ops};

/// Tree pane width: border + 1-cell inner padding each side + content.
pub const NAV_WIDTH: u16 = 48;

/// Blank columns between two card columns in the band's column flow. One is enough to
/// part them: every card opens with its address column, so a gutter reads as a gap
/// between a name and the next number rather than two names running together.
pub(super) const COL_GUTTER: u16 = 1;

/// The glyph the side list's band rule is drawn from, repeated across the nav. A light
/// box-drawing line, so it parts the bands without reading as a border around either.
pub(super) const BAND_RULE: &str = "\u{2500}";

/// The columns a session card is indented by under its section title, at every nav
/// position. The indent and the dim title are the whole of what marks a group: no rule
/// and no connector is painted for it. The indent lies outside the card's rect, so the
/// selection's inversion of that rect starts where the card does. A band one row tall
/// runs its titles and cards along one line, where an indent would mark nothing, so it
/// indents nothing.
pub(super) const CARD_INDENT: u16 = 2;

/// What a band column that continues a section writes after the repeated title on its
/// top row, saying the cards under it belong to a section begun in an earlier column.
pub(super) const CONTINUED: &str = " \u{2026}";

pub use crate::ui::chrome::ViewBorderColors;

pub use crate::model::{NavSize, ViewLayout};

/// The collapsed width of a side nav: the resting prefix with one cell either side,
/// which is the prefix indicator the collapsed column keeps on its bottom line.
pub(crate) fn collapsed_nav_width(ui_prefix: &str) -> u16 {
    UnicodeWidthStr::width(ui_prefix)
        .saturating_add(2)
        .min(u16::MAX as usize) as u16
}

/// Whether the hint bar floats over the whole window instead of resting at the nav's
/// prefix indicator: for an input line, a refusal, and the hint after a selection move.
/// A live prefix does not float the bar: its keys open in the key list instead.
pub(crate) fn hint_bar_floats(state: &crate::state::State) -> bool {
    state.is_inputting()
        || !state.chrome.flash.is_empty()
        || (state.chrome.selection_hint.is_some() && !state.chrome.armed)
}

/// Whether the prefix key list is open: a live prefix that no input line or refusal
/// outranks.
pub(crate) fn key_list_open(state: &crate::state::State) -> bool {
    state.chrome.armed && !state.is_inputting() && state.chrome.flash.is_empty()
}

/// The auto band-layout tree height for a body of `body_rows` rows (before the hint bar row
/// is removed the caller passes `full_height - 1`). This is the seed a RELATIVE height resize
/// (prefix h/l in a band) starts from while `nav_height` is still 0 (auto), so the first key
/// adjusts the height the user actually sees.
pub fn default_nav_height(body_rows: u16) -> u16 {
    top_nav_height(body_rows)
}

/// The tree region's height in the band layout: ~40% of the body, at least a few rows, but
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
/// tree and terminal split the whole area side by side (`Column`, sized by `nav_width`)
/// or stacked (`Band`, sized by `nav_height`), parted by the one-cell view border, the
/// seam. The hint bar is where the prefix indicator rests: the BOTTOM row of a column's
/// nav region, and the seam row itself in a band, so every row a band takes holds cards
/// and the terminal view keeps every row it owns.
/// A collapsed nav gives the cards no region: a side nav keeps a column as wide as its
/// collapsed width with the prefix on its bottom row, a top or bottom nav keeps the seam
/// row alone. `nav_width == 0` is the tree-hidden sentinel: the terminal owns the whole
/// area (and there is no nav to carry a hint bar). `nav_height == 0` means the band height
/// is auto (~40% of the area).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Regions {
    pub layout: ViewLayout,
    pub tree: Rect,
    pub view_border: Rect,
    pub terminal: Rect,
    pub hint_bar: Rect,
}

impl Default for Regions {
    fn default() -> Self {
        Self {
            layout: ViewLayout::Column,
            tree: Rect::default(),
            view_border: Rect::default(),
            terminal: Rect::default(),
            hint_bar: Rect::default(),
        }
    }
}

/// The band-layout tree height: a user-set `nav_height` (dragged border) clamped so both
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

fn collapsed_hint_bar(nav: Rect) -> Rect {
    if nav.height == 0 {
        Rect::default()
    } else {
        Rect::new(nav.x, nav.y + nav.height - 1, nav.width, 1)
    }
}

fn split_nav_for_state(nav: Rect, hint_bar_h: u16, collapsed: bool) -> (Rect, Rect) {
    if collapsed {
        (Rect::default(), collapsed_hint_bar(nav))
    } else {
        split_nav(nav, hint_bar_h)
    }
}

pub fn compute_regions(area: Rect, nav: NavSize, hint_bar_h: u16) -> Regions {
    // The layout follows the attachment position: a left or right placement is a column,
    // a top or bottom one a band. The position travels with the hidden nav unchanged, so
    // hiding it cannot flip the layout; the hidden sentinel below still gives the whole
    // area to the terminal.
    let layout = nav.position.layout();
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
    match nav.position {
        NavPosition::Left => {
            let c = Layout::horizontal([
                Constraint::Length(nav_width),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            let (tree, hint_bar) = split_nav_for_state(c[0], hint_bar_h, nav.collapsed);
            Regions {
                layout,
                tree,
                view_border: c[1],
                terminal: c[2],
                hint_bar,
            }
        }
        NavPosition::Right => {
            // The left column mirrored: the terminal keeps the remainder, the border and
            // the tree follow on the right. The tree region is the left column's, so the
            // in-region layout (card flow, hint bar) is identical at both placements.
            let c = Layout::horizontal([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(nav_width),
            ])
            .split(area);
            let (tree, hint_bar) = split_nav_for_state(c[2], hint_bar_h, nav.collapsed);
            Regions {
                layout,
                tree,
                view_border: c[1],
                terminal: c[0],
                hint_bar,
            }
        }
        NavPosition::Top => {
            let th = if nav.collapsed {
                0
            } else {
                top_nav_height_for(area.height, nav_height)
            };
            let r = Layout::vertical([
                Constraint::Length(th),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            Regions {
                layout,
                tree: r[0],
                view_border: r[1],
                terminal: r[2],
                hint_bar: r[1],
            }
        }
        NavPosition::Bottom => {
            // The top band mirrored: the seam is the row ABOVE the band, and the prefix
            // rests on it as it does above the top band's cards.
            let th = if nav.collapsed {
                0
            } else {
                top_nav_height_for(area.height, nav_height)
            };
            let r = Layout::vertical([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(th),
            ])
            .split(area);
            Regions {
                layout,
                tree: r[2],
                view_border: r[1],
                terminal: r[0],
                hint_bar: r[1],
            }
        }
    }
}

pub use crate::state::Scan;

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
    palette: crate::ui::palette::Palette,
    /// Set once the selection has been moved deliberately: a key, a click, or an
    /// address the app was told to select. [`Switcher::restore_focus`] reads it to
    /// decide whether a vanished card falls back to its neighbour or to the rebuild's
    /// own preselect.
    user_moved: bool,
    /// Set by [`Switcher::request_rescan`] (the `r` key and the ctl `rescan` verb) and
    /// taken by the update step that turns the rescan command into a runtime effect, so
    /// the runtime starts discovery only for a rescan that cleared the nav.
    rescan_kick: bool,
    /// Signals the event loop to re-attach the CURRENT display: tear the (possibly
    /// detached / dead) attachment down so the next attach re-creates a fresh client.
    /// Set on an `r` re-scan - explicit, on-demand recovery for the viewed session.
    reattach_kick: bool,

    rows: Vec<Row>,
    selected: usize,

    terminal_view_target: TerminalViewTarget,
    /// The session xmux is ITSELF running in, when it is inside one. The
    /// one session the terminal view refuses: see [`Switcher::is_own_session`].
    own_session: Option<Address>,
    /// Whether the nav hides the settled unreachable hosts' cards (`[ui]
    /// hide-unreachable`). The app threads it in at construction; there is no live
    /// toggle. The filter naming a hidden host keeps its card, which is the
    /// unreachable screen's one entry point.
    hide_unreachable: bool,
    /// Whether the terminal view held the focus at the last [`Switcher::sync_view_focus`],
    /// so the move from the nav into the terminal view is seen as the one edge it is.
    terminal_view: bool,
    /// Whether the nav leaves its host band unpainted. Decided on the move into the
    /// terminal view: a session card selected then hides the band, since what the user
    /// went to look at is a session and the hosts with nothing to show are only noise
    /// beside it; a host card selected keeps it, since the screen beside the nav is that
    /// host's own. Cleared on the move back into the nav.
    host_band_hidden: bool,
    /// Whether a prefix interaction is live. The hint bar it raises offers a jump to any
    /// card by number, so every card it can reach is painted while it lasts; the hidden
    /// band returns to hidden when the prefix ends.
    prefix_active: bool,

    /// A pending re-scan reselect: the session the selection was on when `r`
    /// was pressed. A re-scan clears every session, so the row briefly vanishes; this
    /// returns the selection to it the instant its host re-streams. Cleared once matched,
    /// or when the user navigates off the parked parent host during the skeleton phase.
    rescan_reselect: Option<Address>,
    /// The transient offset and in-flight border drag of the active modal popup. Its
    /// frame geometry belongs to the render plan shared with mouse input.
    popup_geo: PopupGeometry,
}

mod columns;
mod input;
mod mouse;
mod render;
#[cfg(test)]
pub(crate) use render::MIDDLE_ELLIPSIS;
pub(crate) use render::SELECTED_MARK;
mod side;

pub use render::RenderPlan;

pub use crate::model::{step_nav_position, NavPosition};

impl Switcher {
    fn blank() -> Self {
        Switcher {
            palette: crate::ui::palette::Palette::default(),
            user_moved: false,
            rescan_kick: false,
            reattach_kick: false,
            rows: Vec::new(),
            selected: 0,
            terminal_view_target: TerminalViewTarget::default(),
            own_session: None,
            hide_unreachable: false,
            terminal_view: false,
            host_band_hidden: false,
            prefix_active: false,
            rescan_reselect: None,
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
        s.rebuild(state);
        s
    }

    #[cfg(test)]
    pub(crate) fn palette(&self) -> &crate::ui::palette::Palette {
        &self.palette
    }

    pub(crate) fn set_palette(&mut self, palette: crate::ui::palette::Palette) {
        self.palette = palette;
    }

    pub fn terminal_view_target(&self) -> TerminalViewTarget {
        self.terminal_view_target.clone()
    }

    /// Names the session xmux is running in, so the terminal view can refuse it. The app
    /// calls this once at startup; outside a mux, and where the session could not be
    /// named, it is never called and nothing is refused.
    pub fn set_own_session(&mut self, address: Option<Address>) {
        self.own_session = address;
    }

    /// Sets whether the nav hides the settled unreachable hosts' cards, rebuilding the
    /// rows since the setting decides which groups render. A no-op when the value is
    /// unchanged. The app threads the config value in once at startup.
    pub fn set_hide_unreachable(&mut self, on: bool, state: &mut crate::state::State) {
        if self.hide_unreachable == on {
            return;
        }
        self.hide_unreachable = on;
        self.rebuild(state);
    }

    /// Tells the nav which view holds the focus, the one behind a modal included. The
    /// move from the nav into the terminal view decides whether the host band is hidden
    /// (see `host_band_hidden`); the move back into the nav shows it again.
    pub fn sync_view_focus(&mut self, terminal: bool) {
        if terminal && !self.terminal_view {
            self.host_band_hidden = matches!(self.current_ref(), Some(RowRef::Session { .. }));
        } else if !terminal {
            self.host_band_hidden = false;
        }
        self.terminal_view = terminal;
    }

    /// Tells the nav whether a prefix interaction is live (see `prefix_active`).
    pub fn sync_prefix(&mut self, active: bool) {
        self.prefix_active = active;
    }

    /// Whether the paint leaves the host band out: hidden by the move into the terminal
    /// view, and not overridden by a live prefix.
    fn band_unpainted(&self) -> bool {
        self.host_band_hidden && !self.prefix_active
    }

    /// Whether `(source, target)` addresses the session xmux is ITSELF running in.
    ///
    /// That session has a live grid like any other, and showing it is still refused:
    /// attaching to it puts a second client on the session that holds xmux, which moves
    /// the user's own client and paints xmux inside itself.
    fn is_own_session(&self, source: &str, target: &str) -> bool {
        match &self.own_session {
            Some(own) => !target.is_empty() && own.source == source && own.session == target,
            None => false,
        }
    }

    /// Takes the pending rescan-kick flag that [`Switcher::request_rescan`] sets. The
    /// update step takes it when it turns the rescan command into a runtime effect.
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
        // Hold the selection on its session across this rebuild whenever that session
        // survives (matched by identity) - a rebuild re-derives the whole row list, so a
        // routine one (local poll, remote %-event refetch) must NOT snap the selection
        // back to the top row, which would yank the displayed session out from under
        // whoever is watching (the selection thrash).
        //
        // It holds from the FIRST session the selection ever lands on, the user having
        // moved it or not. During the scan the hosts answer in whatever order they
        // happen to and each answer re-derives the rows, so a preselect that re-picked
        // the top card would walk from host to host as they arrive, attaching a session
        // per step. The session that answered first is the one already on screen, and it
        // keeps the selection until the user or the mux moves it.
        let keep = self
            .rows
            .get(self.selected)
            .and_then(|r| match &r.reference {
                RowRef::Session { .. } => Some(r.reference.clone()),
                RowRef::Host { .. } | RowRef::Section { .. } => None,
            });

        // The deterministic display order (groups local→WSL→remote then by source name,
        // sessions by name) is applied here, once, so every mutation path lands on it and
        // a routine poll reproduces the same order exactly - there is nothing to freeze.
        // Pure row generation lives in `tree::flatten`; rebuild orchestrates order →
        // flatten → preselect → restore around it.
        for g in state.groups.iter_mut() {
            tree::sort_by_name(&mut g.sessions);
        }
        state.groups = tree::order_groups(&state.groups);
        // The mux each card NAMES comes from one resolver, so a session card, its host's
        // card and the screen behind either cannot spell one mux three ways.
        let named_mux = |source: &str| state.chrome.source_mux(source).to_string();
        let rows = tree::flatten(
            &state.groups,
            &state.scanning,
            &state.logged_in,
            &state.filter,
            self.hide_unreachable,
            &named_mux,
        );

        self.rows = rows;
        let target = keep
            .as_ref()
            .and_then(|k| self.rows.iter().position(|r| same_node(&r.reference, k)))
            .or_else(|| self.rows.iter().position(Row::selectable))
            .unwrap_or(0);
        self.set_selected(target, state);
        self.update_filter_label(state);
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

    /// Whether card `i` opens a new unit in the portrait column flow: a section title
    /// (its session cards hang under it) or a host-state card. A session card hangs
    /// under its section and starts nothing.
    fn starts_run(&self, i: usize) -> bool {
        matches!(
            self.rows.get(i).map(|r| &r.reference),
            Some(RowRef::Section { .. }) | Some(RowRef::Host { .. })
        )
    }

    /// The selectable count: the number of cards the numbering and the jump address,
    /// section titles excepted. The cards are numbered by their rank among the
    /// selectable rows, so a section title never takes a number from the cards under
    /// it.
    fn selectable_count(&self) -> usize {
        self.rows.iter().filter(|r| r.selectable()).count()
    }

    /// The number card `i` addresses: its 1-based position among the selectable
    /// cards, the first card being 1 and the last the selectable count. A section
    /// title has no number; it is never the selection and never a jump target.
    fn card_number(&self, i: usize) -> usize {
        self.rows[..i].iter().filter(|r| r.selectable()).count() + 1
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

    /// How many rows the nav paints: every row, or only the rows above the host band
    /// while it is hidden. The rows themselves stay whole, so the card numbers, the
    /// selection and the keys that walk the list are the same whether the band shows.
    fn painted_rows(&self) -> usize {
        if self.band_unpainted() {
            self.band_boundary().unwrap_or(self.rows.len())
        } else {
            self.rows.len()
        }
    }

    /// The band boundary as the paint sees it: none while the host band is hidden, since
    /// there is no second band on screen to part from the first.
    fn painted_boundary(&self) -> Option<usize> {
        if self.band_unpainted() {
            None
        } else {
            self.band_boundary()
        }
    }

    /// The index of the section title the SELECTED row hangs under: the Section row
    /// directly above a selected session card, `None` when the selection is a host-state
    /// card or no section heads it. A section title is the row its group of cards reads
    /// its `{host}/{mux}` from, and it scrolls off the top edge with the cards it heads;
    /// the side placement pulls the list back to show a title when the card under it and
    /// the title fit on screen together.
    fn selected_section_title(&self) -> Option<usize> {
        let sel = self.selected;
        let r = self.rows.get(sel)?;
        if !matches!(r.reference, RowRef::Session { .. }) {
            return None;
        }
        self.rows[..sel]
            .iter()
            .rposition(|r| matches!(r.reference, RowRef::Section { .. }))
    }

    fn set_selected(&mut self, idx: usize, state: &crate::state::State) {
        if self.rows.is_empty() {
            // No row is a session row, so the host band has nothing to stay hidden for.
            self.host_band_hidden = false;
            return;
        }
        let idx = idx.min(self.rows.len() - 1);
        self.selected = idx;
        if !matches!(self.current_ref(), Some(RowRef::Session { .. })) {
            self.host_band_hidden = false;
        }
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

    /// Horizontal navigation (←/→): the selection lands on the first card of the
    /// previous/next CATEGORY. It and the vertical step name the two things the list is
    /// made of - one walks the cards, the other walks the categories - so a list of many
    /// hosts is crossed without stepping over every session between them.
    /// Wraps at both ends, as the vertical step does.
    ///
    /// A category is a source that has sessions to show, or the whole host band at once
    /// ([`category_of_row`]). Landing is always on the category's first card: its first
    /// session, or the band's first host card. Leaving is from ANY card of it, so a
    /// selection deep inside the band steps straight out.
    ///
    /// Neither step is defined by where a card sits on screen, so both mean the same
    /// thing in the side column and in the portrait band, whose cards flow down a column
    /// and then right.
    fn nav_horizontal(&mut self, delta: isize, state: &crate::state::State) {
        let heads = self.category_heads();
        if heads.is_empty() {
            return;
        }
        let here = self
            .rows
            .get(self.selected)
            .map(|r| category_of_row(&r.reference))
            .and_then(|cat| heads.iter().position(|(c, _)| c.as_deref() == cat))
            .unwrap_or(0) as isize;
        let n = heads.len() as isize;
        let next = ((here + delta) % n + n) % n;
        self.user_moved = true;
        self.set_selected(heads[next as usize].1, state);
    }

    /// Each category in list order paired with its first selectable card - the landing
    /// points of a horizontal step. The cards of one category are contiguous (the
    /// flatten emits a section and its sessions together, and sinks every source with
    /// nothing to show to the host band at the end), so one entry per category is one
    /// place to land.
    fn category_heads(&self) -> Vec<(Option<String>, usize)> {
        let mut heads: Vec<(Option<String>, usize)> = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            if !r.selectable() {
                continue;
            }
            let cat = category_of_row(&r.reference).map(str::to_string);
            if !heads.iter().any(|(c, _)| *c == cat) {
                heads.push((cat, i));
            }
        }
        heads
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

    /// The card the selection is on, as an identity a later look can compare with: the
    /// same card across a rebuild that moved its row.
    pub(crate) fn selected_card(&self) -> Option<RowRef> {
        self.current_ref().cloned()
    }

    /// Whether the selection is on a different card than `before`.
    pub(crate) fn selection_moved_from(&self, before: &Option<RowRef>) -> bool {
        match (before, self.current_ref()) {
            (Some(a), Some(b)) => !same_node(a, b),
            (None, None) => false,
            _ => true,
        }
    }

    /// What the hint bar offers about the selected card after a selection move: its most
    /// relevant keys, read from the key table, and one fact about it. A session offers its terminal and a sibling
    /// session and states its windows; a settled host offers the screen that explains it
    /// (or a new session when it is empty) and a re-scan, and states its state word with
    /// the reason behind it; a host still scanning offers the filter and says so.
    pub(crate) fn selection_hint(
        &self,
        state: &crate::state::State,
    ) -> Option<(Vec<crate::state::chrome::HintKey>, String)> {
        use crate::model::keys::{entry_for, KeyCommand};
        let (commands, fact): (&[KeyCommand], String) = match self.current_ref()? {
            RowRef::Section { .. } => return None,
            RowRef::Session { sess } => {
                let mut facts = Vec::new();
                if sess.windows > 0 {
                    let s = if sess.windows == 1 { "" } else { "s" };
                    facts.push(format!("{} window{s}", sess.windows));
                }
                if sess.attached {
                    facts.push("attached".to_string());
                }
                (
                    &[KeyCommand::FocusTerminal, KeyCommand::NewSession],
                    facts.join(", "),
                )
            }
            RowRef::Host { scanning: true, .. } => (&[KeyCommand::Filter], "scanning".into()),
            RowRef::Host {
                source,
                unreachable,
                blocked,
                list_failed,
                ..
            } => {
                let word = tree::host_state_word(false, *blocked, *list_failed, *unreachable);
                let reason = state
                    .groups
                    .iter()
                    .find(|g| &g.source == source)
                    .and_then(|g| g.err.as_deref())
                    .and_then(|e| e.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or_default();
                let commands: &[KeyCommand] = if *unreachable || *blocked || *list_failed {
                    &[KeyCommand::FocusTerminal, KeyCommand::Rescan]
                } else {
                    &[KeyCommand::NewSession, KeyCommand::Rescan]
                };
                let fact = if reason.is_empty() {
                    word.to_string()
                } else {
                    format!("{word}: {reason}")
                };
                (commands, fact)
            }
        };
        let keys = commands
            .iter()
            .filter_map(|c| entry_for(*c))
            .map(|e| {
                (
                    e.full_label(&state.chrome.ui_prefix, state.chrome.nav_position),
                    e.long.to_string(),
                    e.short.to_string(),
                )
            })
            .collect();
        Some((keys, fact))
    }

    pub(crate) fn current_source(&self) -> Option<String> {
        match self.current_ref()? {
            RowRef::Host { source, .. } | RowRef::Section { source, .. } => Some(source.clone()),
            RowRef::Session { sess } => Some(sess.source.clone()),
        }
    }

    pub(super) fn current_host_unreachable(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { unreachable, .. }) if *unreachable)
    }

    /// True when the selected host failed in a way the user can answer from xmux. Its
    /// terminal-view panel carries the login pane, so a keystroke typed while the
    /// terminal view is focused drives that pane rather than reaching a session.
    pub(crate) fn current_host_blocked(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { blocked, .. }) if *blocked)
    }

    /// Which host screen the terminal view shows in place of the grid, or `None` when it
    /// shows the grid. Only a selected HOST card earns one, and only once it has settled:
    /// unreachable names why it failed, empty names what to press. A host still scanning
    /// gets neither, because an in-flight state is the nav's to show (its card spins) and
    /// the view keeps the grid it already has.
    fn current_view_screen(&self, state: &crate::state::State) -> Option<ViewScreen> {
        let selected_address = self.current_screen_address(state);
        let selected_source = match self.current_ref() {
            Some(RowRef::Host { source, .. }) => Some(source.as_str()),
            _ => None,
        };
        let group = selected_source
            .and_then(|source| state.groups.iter().find(|group| group.source == source));
        crate::model::choose_view_screen(
            selected_source,
            selected_address.as_ref(),
            group.and_then(crate::model::Group::failure),
            selected_source.is_some_and(|source| state.scanning.contains(source)),
            group.is_some_and(|group| group.sessions.is_empty()),
            self.own_session.as_ref(),
        )
    }

    /// The session the selected card would show, or `None` when it would show nothing.
    /// The pair is what a refusal is keyed to, and what the screen writes as its headline.
    fn current_screen_address(&self, state: &crate::state::State) -> Option<Address> {
        let r = self.current_ref()?;
        let (source, target) = tree::target_for(r, &state.groups, &state.filter);
        (!target.is_empty()).then(|| Address::new(&source, &target))
    }

    /// What the view screen is about: the [`Address`] for the
    /// self-session state, whose subject is one session, and the host (with an empty
    /// session half) for the two host states, whose subject is the host.
    pub(crate) fn view_screen_address(
        &self,
        state: &crate::state::State,
        kind: ViewScreen,
    ) -> Address {
        match kind {
            ViewScreen::SelfSession => self.current_screen_address(state).unwrap_or_default(),
            _ => Address::new(self.current_source().unwrap_or_default(), ""),
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
    ///
    /// The one mover for a selection xmux is TOLD to make, whoever asked: a ctl `switch`,
    /// a create landing on its new card, or the nav following the session the mux moved
    /// its own display client onto. All three name a card and move to it, and nothing
    /// downstream tells them apart, so they share one entry point.
    pub fn select_address(&mut self, address: &Address, state: &crate::state::State) -> bool {
        match self.row_of_session(address) {
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
            Some(RowRef::Host { source, .. }) | Some(RowRef::Section { source, .. }) => {
                (None, Some(source.clone()))
            }
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
    /// (unreachable). The host authoritatively owns its session list. Ordering is
    /// not this function's concern: `rebuild` applies the deterministic display
    /// order, which a scan result and a routine poll reproduce exactly.
    ///
    /// A result that RENAMED one session ([`tree::renamed_session`]) carries the selection
    /// and the displayed record across to the new name, so the card the user is on stays
    /// the card they are on and nothing reads the rename as a move to another session.
    /// The rename is returned so the loop can carry its own display record across too.
    pub fn apply_source_result(
        &mut self,
        source: String,
        sessions: Vec<Session>,
        err: Option<String>,
        state: &mut crate::state::State,
    ) -> Option<(String, String)> {
        let renamed = state
            .groups
            .iter()
            .find(|g| g.source == source)
            .filter(|_| err.is_none())
            .and_then(|g| tree::renamed_session(&g.sessions, &sessions));
        if let Some((from, to)) = &renamed {
            for row in self.rows.iter_mut() {
                if let RowRef::Session { sess } = &mut row.reference {
                    if sess.source == source && sess.name == *from {
                        sess.name = to.clone();
                    }
                }
            }
            for sel in [&mut state.selection, &mut state.displayed] {
                if sel.source == source && sel.session == *from {
                    sel.session = to.clone();
                }
            }
        }
        let prior = self.capture_focus();
        state.scanning.remove(&source);
        // The failure run, counted where every result lands so no path can skip it: a
        // result that failed lengthens it, one that answered clears it. It is shown, not
        // acted on - see `State::failure_runs`.
        match &err {
            Some(_) => *state.failure_runs.entry(source.clone()).or_insert(0) += 1,
            None => {
                state.failure_runs.remove(&source);
            }
        }
        // The mux search a working login started ends with the first source answer after
        // the login's own probe, whichever way that answer went.
        let answer = match &err {
            None => crate::model::MuxAnswer::Found,
            Some(reason) => crate::model::MuxAnswer::Failed(reason.clone()),
        };
        state.login_mux_answered(crate::session::machine_of(&source), &answer);
        let existing = state.groups.iter().position(|g| g.source == source);
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
        self.rebuild(state);
        self.restore_focus(prior, state);
        renamed
    }

    /// Adds a source that was not there at launch (a mux discovery answered) as a
    /// SCANNING host card, so it appears the moment it is found instead of at the next
    /// run. Idempotent: a source already in the nav is left exactly as it is.
    ///
    /// It APPENDS the new host to `state.groups`; `rebuild` then places it in the
    /// deterministic order.
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

    /// Puts the card of `source` back in flight: it spins and carries no failure, for an
    /// answer that is on its way. Idempotent, and a source the nav does not show is left
    /// alone.
    pub fn mark_scanning(&mut self, source: &str, state: &mut crate::state::State) {
        let Some(g) = state.groups.iter_mut().find(|g| g.source == source) else {
            return;
        };
        if g.err.is_none() && state.scanning.contains(source) {
            return;
        }
        let prior = self.capture_focus();
        g.err = None;
        state.scanning.insert(source.to_string());
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
    /// vanished (killed/removed), land on the previous card. An untouched selection is
    /// left exactly where the rebuild put it - on its own session where that survived,
    /// on the first card otherwise.
    fn restore_focus(&mut self, prior: PriorFocus, state: &crate::state::State) {
        // A pending re-scan reselect returns the selection to its session the instant that
        // session re-streams - but only while the selection still sits where the re-scan
        // parked it (that session or its parent host). If the user has navigated
        // elsewhere in the skeleton meanwhile, the pending reselect is dropped so it
        // never yanks them back.
        if let Some(addr) = self.rescan_reselect.clone() {
            let parked = match prior.reference.as_ref() {
                Some(RowRef::Host { source, .. }) => addr.source == *source,
                Some(RowRef::Session { sess }) => sess.address() == addr,
                // A section title is never the selection, so it is never where a
                // re-scan parked; the arm exists to keep the match total.
                Some(RowRef::Section { .. }) => false,
                None => false,
            };
            if parked {
                if let Some(i) = self
                    .rows
                    .iter()
                    .position(|r| session_addr_of(&r.reference).as_ref() == Some(&addr))
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
    /// previous selectable card, or the first selectable when none precedes it.
    /// Section titles are never landed on - they are not cards, so the fallback walks
    /// past them to the nearest card. Operates on the freshly rebuilt `self.rows`.
    fn fallback_after_removal(&self, prior_selected: usize) -> Option<usize> {
        self.rows[..prior_selected.min(self.rows.len())]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .or_else(|| self.rows.iter().position(Row::selectable))
    }

    /// The row index targeting the same node as `focus`, if it survives a
    /// rebuild - so a re-scan keeps the selection in place rather than snapping to
    /// the first card.
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

/// The context parts of a row: `(host, mux, session)`. A host-state card and a
/// section title carry only their host; a session card names its session's host, mux
/// kind (empty when not yet known), and session name.
fn context_of(row: &Row) -> (&str, &str, &str) {
    // The MACHINE half, never the whole source id: a source id already carries the mux
    // when its machine serves several, and the card renders the mux as its own span, so
    // returning the id whole would read `local:zellij/zellij`. The mux comes off the row
    // itself, resolved once when the row was built, so every row on one source names its
    // mux the same way whatever each of them had to read it from.
    match &row.reference {
        RowRef::Host { source, .. } | RowRef::Section { source, .. } => {
            (crate::session::machine_of(source), &row.mux, "")
        }
        RowRef::Session { sess } => (
            crate::session::machine_of(&sess.source),
            &row.mux,
            &sess.name,
        ),
    }
}

/// The category a card belongs to on the horizontal step: its source where the card
/// names a session, and the host band as a whole (`None`) for the cards of the sources
/// with nothing to show.
///
/// The band is ONE category because its cards are one machine each with nothing running
/// on it, and a list of them is a single thing to reach past rather than a run of places
/// to be carried into one at a time. Every one of them is still a card, so the vertical
/// step walks them like any other.
fn category_of_row(reference: &RowRef) -> Option<&str> {
    match reference {
        RowRef::Host { .. } => None,
        RowRef::Section { source, .. } => Some(source),
        RowRef::Session { sess } => Some(&sess.source),
    }
}

/// The session a card belongs to (a session card), or `None` for a
/// host-state card and a section title. Lets selection tracking, kill-confirm
/// survival, and `select_address` treat a session card as that session.
fn session_addr_of(reference: &RowRef) -> Option<Address> {
    match reference {
        RowRef::Session { sess } => Some(sess.address()),
        RowRef::Host { .. } | RowRef::Section { .. } => None,
    }
}

/// Whether two row references target the same row across a rebuild (host by source,
/// section by source, session by address), so the selection stays put on a poll /
/// re-scan. A section title is a source's header; it matches only itself, and it is
/// never the selection.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Host { source: x, .. }, RowRef::Host { source: y, .. }) => x == y,
        (RowRef::Section { source: x, .. }, RowRef::Section { source: y, .. }) => x == y,
        (RowRef::Host { .. }, _) | (_, RowRef::Host { .. }) => false,
        (RowRef::Section { .. }, _) | (_, RowRef::Section { .. }) => false,
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
mod tests_position;

#[cfg(test)]
pub(crate) mod tests_support;
