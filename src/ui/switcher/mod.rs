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

use crate::model::{Action, Command, Node, ViewScreen};
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
pub(super) const CARD_INDENT: u16 = 1;

/// What a band column that continues a section writes after the repeated title on its
/// top row, saying the cards under it belong to a section begun in an earlier column.
pub(super) const CONTINUED: &str = " \u{2026}";

pub use crate::ui::chrome::ViewBorderColors;

pub use crate::model::{NavSize, ViewLayout};

/// The collapsed width of a side nav: exactly the resting prefix, which the collapsed
/// column keeps on its bottom line with no padding. The column exists only to keep the
/// prefix in view and to be clicked open, so every further cell would be taken from the
/// terminal view. Its view border shares the column's terminal-side edge.
pub(crate) fn collapsed_nav_width(ui_prefix: &str) -> u16 {
    UnicodeWidthStr::width(ui_prefix).min(u16::MAX as usize) as u16
}

/// The resting prefix with one cell either side: the chip a band's seam row carries while
/// the band is collapsed or its bar floats, and the label an expanded nav's indicator
/// needs room for.
pub(crate) fn prefix_chip_width(ui_prefix: &str) -> u16 {
    collapsed_nav_width(ui_prefix).saturating_add(2)
}

/// Whether the hint bar floats over the whole window instead of resting at the nav's
/// prefix indicator: for a refusal and the hint after a selection move. A live prefix
/// does not float the bar: its keys open in the key list instead. An open input does not
/// either: it says its keys on its popup's border.
pub(crate) fn hint_bar_floats(state: &crate::state::State) -> bool {
    !state.chrome.flash.is_empty()
        || (state.chrome.selection_hint.is_some() && !state.chrome.armed && !state.is_inputting())
}

/// Whether the prefix key list is open: a live prefix that no input popup or refusal
/// outranks.
pub(crate) fn key_list_open(state: &crate::state::State) -> bool {
    state.chrome.armed && !state.is_inputting() && state.chrome.flash.is_empty()
}

/// The auto band-layout tree height for a body of `body_rows` rows (before the hint bar row
/// is removed the caller passes `full_height - 1`). This is the seed a RELATIVE height resize
/// (prefix Ctrl-↑/↓ in a band) starts from while `nav_height` is still 0 (auto), so the first key
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
/// row alone. A collapsed side nav's view border takes no column of its own: it runs down
/// the column's terminal-side edge on every row above the prefix, so the prefix keeps
/// every one of its characters and the terminal view gains the column. `nav_width == 0` is the tree-hidden sentinel: the terminal owns the whole
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

/// The regions of a collapsed side nav: a column exactly `nav_width` wide at the nav's
/// side, its prefix on the bottom row, and the view border on the column's terminal-side
/// edge above that row. The prefix character on that edge stays readable because the
/// border stops short of it; the terminal view keeps everything beside the column.
fn collapsed_column(
    area: Rect,
    layout: ViewLayout,
    nav_width: u16,
    position: NavPosition,
) -> Regions {
    let w = nav_width.min(area.width);
    let (nav_x, edge_x, terminal_x) = if position == NavPosition::Left {
        (area.x, area.x + w.saturating_sub(1), area.x + w)
    } else {
        let nav_x = area.right() - w;
        (nav_x, nav_x, area.x)
    };
    let nav = Rect::new(nav_x, area.y, w, area.height);
    Regions {
        layout,
        tree: Rect::default(),
        view_border: if w == 0 {
            Rect::default()
        } else {
            Rect::new(edge_x, area.y, 1, area.height.saturating_sub(1))
        },
        terminal: Rect::new(terminal_x, area.y, area.width - w, area.height),
        hint_bar: collapsed_hint_bar(nav),
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
        NavPosition::Left | NavPosition::Right if nav.collapsed => {
            collapsed_column(area, layout, nav_width, nav.position)
        }
        NavPosition::Left => {
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
            let (tree, hint_bar) = split_nav(c[2], hint_bar_h);
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

/// What the user is interested in: the one value both selection rules read
/// (docs/adr/0007-context-follows-the-users-interest.md). A card that DISAPPEARS moves
/// the selection along its lineage; a card that APPEARS takes the selection only when
/// it is what this value names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Interest {
    /// Nothing is chosen yet: the launch asks to show a session, so the first session
    /// card to appear is the interest. Until one does, the selection rests on the first
    /// card, which is a placeholder rather than a choice.
    FirstSession,
    /// The card the selection is on, held by identity across every rebuild.
    Selected,
    /// A session the user asked for whose card is not on the list yet: the session
    /// `prefix n` created, or the session under the selection when a full re-scan
    /// cleared every session. The selection waits on that session's lineage card and
    /// moves to the session when its card appears. The interest ends when the user
    /// moves the selection or the session's source answers without it.
    Awaiting(Address),
}

/// Which part of a nav row a selection or the pointer is on. A card is one target. A
/// section title and a source card's `{host}/{mux}` each read as two: the host half names
/// the host and the rest names the source, so the title of `db-01/tmux` opens the screen
/// of `db-01` from one half and of `db-01/tmux` from the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// The whole card: a session, a source's card, or a host's card.
    Card,
    /// The host half of a section title or of a source card.
    Host,
    /// The source half of a section title.
    Source,
}

/// Where the hard selection stands: a row, the part of it, and a node deeper than any
/// nav target when a screen link or a step down opened one the nav has no card for. The
/// nav paints the row and part, which then name that node's nearest ancestor on the list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    row: usize,
    part: Part,
    deep: Option<Node>,
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
    /// What the user is on or asked for. [`Switcher::rebuild`] resolves the selection
    /// from it on every pass.
    interest: Interest,
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
    /// The part of the selected row the hard selection is on.
    part: Part,
    /// A node the hard selection names that has no nav target of its own; the selected
    /// row and part then stand for its nearest ancestor on the list.
    deep: Option<Node>,
    /// For each node the selection stepped up from, the child it left, so a step down
    /// returns to it.
    trail: std::collections::HashMap<Node, Node>,
    /// The soft selection in the nav: the target under the pointer while the nav holds
    /// the focus, as a row identity and the part of it. The terminal view shows its
    /// screen; nothing else follows it.
    hover: Option<(RowRef, Part)>,
    /// The hard-selected link on the shown host or source screen, by index and by the
    /// node it names, so a rebuild that adds or drops links keeps the same node selected.
    link: usize,
    link_node: Option<Node>,
    /// The link under the pointer on that screen while the terminal view holds the focus.
    link_hover: Option<usize>,
    /// Host whose login pane was opened explicitly from the check table or palette.
    login_target: Option<String>,

    terminal_view_target: TerminalViewTarget,
    /// The session xmux is ITSELF running in, when it is inside one. The
    /// one session the terminal view refuses: see [`Switcher::is_own_session`].
    own_session: Option<Address>,
    /// Whether the current sorted list receives contiguous numbers on each rebuild.
    renumbering: bool,
    /// Card numbers keyed by identity. The configured policy either deals them in the
    /// current sorted list order or keeps each card's number until the next full scan.
    numbers: std::collections::HashMap<CardId, usize>,
    next_number: usize,
    numbers_fixed: bool,
    /// Whether the full scan still waits on its roster answer, which can add hosts after
    /// every source on the list has answered. The numbers are not fixed while it does.
    numbers_held: bool,
    terminal_view: bool,
    /// Whether host cards are omitted after leaving nav from a session card.
    host_band_hidden: bool,

    /// The session whose card a full re-scan turned into its host card, held until the
    /// selection moves. While it holds, the scanning host card keeps that session's
    /// confirmed grid instead of its scanning screen.
    rescan_collapse: Option<Address>,
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
            interest: Interest::FirstSession,
            rescan_kick: false,
            reattach_kick: false,
            rows: Vec::new(),
            selected: 0,
            part: Part::Card,
            deep: None,
            trail: std::collections::HashMap::new(),
            hover: None,
            link: 0,
            link_node: None,
            link_hover: None,
            login_target: None,
            terminal_view_target: TerminalViewTarget::default(),
            own_session: None,
            renumbering: true,
            numbers: std::collections::HashMap::new(),
            next_number: 1,
            numbers_fixed: false,
            numbers_held: false,
            terminal_view: false,
            host_band_hidden: false,
            rescan_collapse: None,
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

    /// Applies the card-number policy to the current list and subsequent rebuilds.
    pub fn set_renumbering(&mut self, on: bool, state: &mut crate::state::State) {
        if self.renumbering == on {
            return;
        }
        self.renumbering = on;
        self.rebuild(state);
    }

    /// Tells the nav which view holds the focus, the one behind a modal included. The
    /// move from the nav into the terminal view decides whether the host band is hidden
    /// (see `host_band_hidden`); the move back into the nav shows it again.
    pub fn sync_view_focus(&mut self, terminal: bool) {
        // Each surface's soft selection lives only while that surface holds the focus.
        let hovered = self.hover.is_some() || self.link_hover.is_some();
        if terminal {
            self.hover = None;
        } else {
            self.link_hover = None;
        }
        if terminal && !self.terminal_view {
            self.host_band_hidden = matches!(self.current_ref(), Some(RowRef::Session { .. }));
        } else if !terminal {
            self.host_band_hidden = false;
        }
        self.terminal_view = terminal;
        if hovered {
            self.on_focus_changed();
        }
    }

    /// Whether the paint leaves the host band out: hidden by the move into the terminal
    /// view from a session card. Prefix interactions preserve this decision, but a
    /// selection on a host card paints the band, since a selected card is always painted.
    fn band_unpainted(&self) -> bool {
        self.host_band_hidden
            && !matches!(
                self.current_ref(),
                Some(RowRef::Host { .. } | RowRef::Machine { .. })
            )
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
        if self.login_target.as_ref().is_some_and(|source| {
            !state
                .groups
                .iter()
                .any(|group| group.source == *source && group.failure().is_some())
        }) {
            self.login_target = None;
        }
        let prior = Prior {
            node: self.selected_node(),
            row: self.current_ref().cloned(),
            deep: self.deep.is_some(),
            index: self.selected,
        };

        // The deterministic display order (groups local→WSL→remote then by source name,
        // sessions by name) is applied here, once, so every mutation path lands on it and
        // a routine poll reproduces the same order exactly - there is nothing to freeze.
        // Pure row generation lives in `tree::flatten`; rebuild orchestrates order →
        // flatten → the selection resolved from the interest around it.
        for g in state.groups.iter_mut() {
            tree::sort_by_name(&mut g.sessions);
        }
        state.groups = tree::order_groups(&state.groups);
        // The mux each card NAMES comes from one resolver, so a session card, its host's
        // card and the screen behind either cannot spell one mux three ways.
        let named_mux = |source: &str| state.chrome.source_mux(source).to_string();
        let rows = tree::flatten(&state.groups, &state.scanning, &state.filter, &named_mux);
        // While the numbers are dealt in list order, they are dealt over the list the
        // filter does not narrow, so a filter typed during a scan cannot renumber the cards
        // it hides.
        let unfiltered = (!self.renumbering && !self.numbers_fixed && !state.filter.is_empty())
            .then(|| tree::flatten(&state.groups, &state.scanning, "", &named_mux));

        let old_rows = std::mem::replace(&mut self.rows, rows);
        self.number_cards(unfiltered.as_deref(), state.scanning.is_empty());
        let target = self.resolve_selection(prior, &old_rows, state);
        if self
            .hover
            .as_ref()
            .is_some_and(|(r, _)| self.row_matching(r).is_none())
        {
            self.hover = None;
        }
        self.set_target(target);
        self.resolve_link(state);
    }

    /// Re-reads the selected link after the screen's links changed: the link naming the
    /// same node, else the link now at its place, within the links there are. A pointer
    /// left over a link that is gone names nothing.
    fn resolve_link(&mut self, state: &crate::state::State) {
        let links = self
            .selected_node()
            .map(|node| self.screen_links(&node, state))
            .unwrap_or_default();
        match self
            .link_node
            .as_ref()
            .and_then(|node| links.iter().position(|l| l.node == *node))
        {
            Some(i) => self.link = i,
            None => {
                self.link = self.link.min(links.len().saturating_sub(1));
                self.link_node = links.get(self.link).map(|l| l.node.clone());
            }
        }
        if self.link_hover.is_some_and(|i| i >= links.len()) {
            self.link_hover = None;
        }
    }

    /// The target the selection takes after a rebuild, read from [`Interest`].
    ///
    /// A card that APPEARS takes the selection only when the interest names it: the first
    /// session card while nothing is chosen yet, or the awaited session. Anything else
    /// holds the prior node, and a prior node that DISAPPEARED moves along its lineage
    /// ([`Switcher::lineage_target`]). No path picks a position of its own.
    fn resolve_selection(
        &mut self,
        prior: Prior,
        old_rows: &[Row],
        state: &crate::state::State,
    ) -> Target {
        let first_selectable = || Target {
            row: self.rows.iter().position(Row::selectable).unwrap_or(0),
            part: Part::Card,
            deep: None,
        };
        match self.interest.clone() {
            Interest::FirstSession => {
                // A session answering later than the first one does not take the cursor:
                // the interest is settled by the first, so the launch attaches one session
                // rather than one per answer.
                match self
                    .rows
                    .iter()
                    .position(|r| matches!(r.reference, RowRef::Session { .. }))
                {
                    Some(i) => {
                        self.interest = Interest::Selected;
                        Target::card(i)
                    }
                    None => first_selectable(),
                }
            }
            Interest::Awaiting(address) => {
                if let Some(i) = self.row_of_session(&address) {
                    self.interest = Interest::Selected;
                    self.rescan_collapse = None;
                    return Target::card(i);
                }
                // The interest ends only when the source answered without the session.
                // A session the filter hides is still in the answer, so its card appears
                // the moment the filter lets it through.
                let listed = state.groups.iter().any(|g| {
                    g.source == address.source && g.sessions.iter().any(|s| s.address() == address)
                });
                if !listed && !state.scanning.contains(&address.source) {
                    self.interest = Interest::Selected;
                }
                self.lineage_target(&prior, old_rows, state)
                    .unwrap_or_else(first_selectable)
            }
            Interest::Selected => self
                .lineage_target(&prior, old_rows, state)
                .unwrap_or_else(first_selectable),
        }
    }

    /// Where the selection on `prior` goes on the rebuilt rows: `prior` itself while it
    /// has a target, otherwise the nearest node up its lineage that has one.
    ///
    /// - a session goes to its source (its section title, or the source's card once it
    ///   has no session to show);
    /// - a source goes to its host (the host's card when the host is down, else the host
    ///   half of the row the source stood on, else of the host's first row);
    /// - a card that stood for the whole host (its card while it was down, or the card
    ///   of the source named by the host alone) that resolved into sources hands the
    ///   selection to the first of them by name;
    /// - when nothing of the host survives, the selection goes to the card that now holds
    ///   the vanished card's place: the first card after it in the prior card order that
    ///   survived, else the last surviving card before it.
    ///
    /// A node the selection reached with no nav target of its own (a screen link opened
    /// it) stays selected while the inventory still holds it. A node that HAD a target
    /// and lost it walks up instead, which is how a host going down gathers the selection
    /// from its sources and sessions onto its one card.
    fn lineage_target(
        &self,
        prior: &Prior,
        old_rows: &[Row],
        state: &crate::state::State,
    ) -> Option<Target> {
        let mut node = prior.node.clone()?;
        let near = prior.row.as_ref().and_then(row_source).map(str::to_owned);
        loop {
            if let Node::Host(machine) = &node {
                // A card that stood for the whole host: its card while it was down, or the
                // card of the source named by the host alone, which stands in for it until
                // its muxes are known.
                let card = |r: &RowRef| match r {
                    RowRef::Machine { machine: m, .. } => m == machine,
                    RowRef::Host { source, .. } => source == machine,
                    _ => false,
                };
                if prior.row.as_ref().is_some_and(card)
                    && !self.rows.iter().any(|r| card(&r.reference))
                {
                    let sources: std::collections::BTreeSet<&str> = state
                        .groups
                        .iter()
                        .map(|g| g.source.as_str())
                        .filter(|s| crate::session::machine_of(s) == machine)
                        .collect();
                    if let Some((row, part)) = sources
                        .into_iter()
                        .find_map(|source| self.target_of(&Node::Source(source.into()), None))
                    {
                        return Some(Target {
                            row,
                            part,
                            deep: None,
                        });
                    }
                }
            }
            if let Some((row, part)) = self.target_of(&node, near.as_deref()) {
                return Some(Target {
                    row,
                    part,
                    deep: None,
                });
            }
            if prior.deep && node_exists(&node, state) {
                return Some(self.deep_target(node));
            }
            match node.parent() {
                Some(parent) => node = parent,
                None => break,
            }
        }
        let survivor = |r: &Row| {
            r.selectable()
                .then(|| self.row_matching(&r.reference))
                .flatten()
        };
        let at = prior.index.min(old_rows.len());
        old_rows
            .get(at + 1..)
            .unwrap_or_default()
            .iter()
            .find_map(survivor)
            .or_else(|| old_rows[..at].iter().rev().find_map(survivor))
            .map(Target::card)
    }

    /// The nav target that stands for `node`: its own card or title half, `None` when the
    /// list has none. A host stands on its card while it is down, otherwise on the host
    /// half of one of its rows: the row of `near` (the source the selection comes from)
    /// when that is one of its, else its first row.
    fn target_of(&self, node: &Node, near: Option<&str>) -> Option<(usize, Part)> {
        match node {
            Node::Session(address) => self.row_of_session(address).map(|i| (i, Part::Card)),
            Node::Source(source) => {
                self.rows
                    .iter()
                    .enumerate()
                    .find_map(|(i, r)| match &r.reference {
                        RowRef::Section { source: s } if s == source => Some((i, Part::Source)),
                        RowRef::Host { source: s, .. } if s == source => Some((i, Part::Card)),
                        _ => None,
                    })
            }
            Node::Host(machine) => {
                if let Some(i) = self.rows.iter().position(
                    |r| matches!(&r.reference, RowRef::Machine { machine: m, .. } if m == machine),
                ) {
                    return Some((i, Part::Card));
                }
                let halved = |r: &Row, want: Option<&str>| match &r.reference {
                    RowRef::Section { source } | RowRef::Host { source, .. } => {
                        crate::session::machine_of(source) == machine
                            && want.is_none_or(|w| w == source)
                    }
                    _ => false,
                };
                near.and_then(|n| self.rows.iter().position(|r| halved(r, Some(n))))
                    .or_else(|| self.rows.iter().position(|r| halved(r, None)))
                    .map(|i| (i, Part::Host))
            }
        }
    }

    /// The target of a node the nav has no target for: the node itself, standing on its
    /// nearest ancestor's target, or on the first card when no ancestor is on the list.
    fn deep_target(&self, node: Node) -> Target {
        let mut up = node.parent();
        while let Some(ancestor) = up {
            if let Some((row, part)) = self.target_of(&ancestor, node.source()) {
                return Target {
                    row,
                    part,
                    deep: Some(node),
                };
            }
            up = ancestor.parent();
        }
        Target {
            row: self.rows.iter().position(Row::selectable).unwrap_or(0),
            part: Part::Card,
            deep: Some(node),
        }
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
    /// (its session cards hang under it), a source's card or a host's card. A session
    /// card hangs under its section and starts nothing.
    fn starts_run(&self, i: usize) -> bool {
        matches!(
            self.rows.get(i).map(|r| &r.reference),
            Some(RowRef::Section { .. } | RowRef::Host { .. } | RowRef::Machine { .. })
        )
    }

    /// Gives every card on the current list its number. Sorted numbering follows the
    /// visible list on every rebuild. Stable numbering holds identities until a full
    /// scan deals the cards again, including filtered-out cards during that scan.
    fn number_cards(&mut self, unfiltered: Option<&[Row]>, settled: bool) {
        if self.renumbering {
            self.numbers.clear();
            self.next_number = 1;
            for id in self.rows.iter().filter_map(|r| card_id(&r.reference)) {
                self.numbers.entry(id).or_insert_with(|| {
                    self.next_number += 1;
                    self.next_number - 1
                });
            }
            self.numbers_fixed = settled && !self.numbers_held;
            return;
        }
        if !self.numbers_fixed {
            self.numbers.clear();
            self.next_number = 1;
            let order: Vec<CardId> = unfiltered
                .unwrap_or(&self.rows)
                .iter()
                .filter_map(|r| card_id(&r.reference))
                .collect();
            for id in order {
                self.numbers.entry(id).or_insert_with(|| {
                    self.next_number += 1;
                    self.next_number - 1
                });
            }
            self.numbers_fixed = settled && !self.numbers_held;
        }
        let missing: Vec<CardId> = self
            .rows
            .iter()
            .filter_map(|r| card_id(&r.reference))
            .filter(|id| !self.numbers.contains_key(id))
            .collect();
        for id in missing {
            self.numbers.insert(id, self.next_number);
            self.next_number += 1;
        }
    }

    /// Opens the numbering to be dealt again in list order, for a full scan.
    fn reopen_numbers(&mut self) {
        self.numbers_fixed = false;
    }

    /// Holds the numbering open until the full scan's roster has answered (`true`), or
    /// releases it (`false`). A release fixes the numbers at once when every source has
    /// already answered, since the last rebuild dealt them in list order.
    pub fn hold_numbers(&mut self, held: bool, state: &crate::state::State) {
        self.numbers_held = held;
        if held {
            self.reopen_numbers();
        } else if state.scanning.is_empty() {
            self.numbers_fixed = true;
        }
    }

    /// The highest number a card on the list carries, 0 for an empty list.
    fn highest_number(&self) -> usize {
        (0..self.rows.len())
            .filter(|&i| self.rows[i].selectable())
            .map(|i| self.card_number(i))
            .max()
            .unwrap_or(0)
    }

    /// The number card `i` carries under the configured policy. A section title has no
    /// number and is never a jump target.
    fn card_number(&self, i: usize) -> usize {
        card_id(&self.rows[i].reference)
            .and_then(|id| self.numbers.get(&id).copied())
            .unwrap_or(0)
    }

    /// Where session cards end: the first host-state card, the flatten having sunk
    /// every host with no session to show to the end of the list. `None` when no host card
    /// is on the list at all; whether a boundary actually parts anything (both bands need
    /// a card) is [`side::place`]'s to judge.
    fn band_boundary(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r.reference, RowRef::Host { .. } | RowRef::Machine { .. }))
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

    fn painted_boundaries(&self) -> Vec<usize> {
        if self.band_unpainted() {
            return Vec::new();
        }
        let mut boundaries = Vec::new();
        if let Some(first) = self.band_boundary() {
            boundaries.push(first);
            if let Some(disconnected) = self.rows.iter().position(|row| {
                matches!(
                    row.reference,
                    RowRef::Host {
                        unreachable: true,
                        ..
                    } | RowRef::Host {
                        list_failed: true,
                        ..
                    } | RowRef::Host { scanning: true, .. }
                        | RowRef::Machine { .. }
                )
            }) {
                if disconnected != first {
                    boundaries.push(disconnected);
                }
            }
        }
        boundaries
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

    /// Puts the hard selection on card `idx` as a whole (the source half of a section
    /// title, which is no card).
    fn set_selected(&mut self, idx: usize) {
        self.set_target(Target::card(idx));
    }

    /// Puts the hard selection on `target`. A section title is never selected whole: its
    /// source half stands for it. The login a host's screen was opened for ends when the
    /// selection leaves that host, and the screen's link selection starts over when the
    /// selection names another node.
    fn set_target(&mut self, target: Target) {
        if self.rows.is_empty() {
            self.deep = target.deep;
            return;
        }
        let before = self.selected_node();
        let row = target.row.min(self.rows.len() - 1);
        let part = match (&self.rows[row].reference, target.part) {
            (RowRef::Section { .. }, Part::Card) => Part::Source,
            (RowRef::Section { .. }, part) => part,
            (RowRef::Host { .. }, Part::Host) => Part::Host,
            _ => Part::Card,
        };
        self.selected = row;
        self.part = part;
        self.deep = target.deep;
        let after = self.selected_node();
        let host = match &after {
            Some(Node::Host(machine)) => Some(machine.as_str()),
            _ => None,
        };
        if self
            .login_target
            .as_deref()
            .is_some_and(|source| Some(crate::session::machine_of(source)) != host)
        {
            self.login_target = None;
        }
        if before != after {
            self.link = 0;
            self.link_node = None;
        }
        self.on_focus_changed();
    }

    /// The node the hard selection names.
    pub(crate) fn selected_node(&self) -> Option<Node> {
        self.deep.clone().or_else(|| {
            self.rows
                .get(self.selected)
                .map(|r| node_of(&r.reference, self.part))
        })
    }

    /// The node whose screen the terminal view shows: the soft selection while the pointer
    /// is on a nav target, else the hard selection.
    pub(crate) fn shown_node(&self) -> Option<Node> {
        match &self.hover {
            Some((reference, part)) => Some(node_of(reference, *part)),
            None => self.selected_node(),
        }
    }

    /// Moves the hard selection to `node`: onto its nav target, or onto the nearest
    /// ancestor's target as a node the nav has no target for. A host keeps the row the
    /// selection leaves when that row is one of its. A move from a node to its parent
    /// records the child, so a step down returns to it.
    fn select_node(&mut self, node: Node) {
        let before = self.selected_node();
        let near = self.current_ref().and_then(row_source).map(str::to_owned);
        if let Some(child) = before.filter(|b| b.parent().as_ref() == Some(&node)) {
            self.trail.insert(node.clone(), child);
        }
        let target = match self.target_of(&node, near.as_deref()) {
            Some((row, part)) => Target {
                row,
                part,
                deep: None,
            },
            None => self.deep_target(node),
        };
        self.set_target(target);
    }

    /// `Ctrl+↑`: the selection walks up a level, session to source to host.
    fn ascend(&mut self) {
        let Some(parent) = self.selected_node().and_then(|node| node.parent()) else {
            return;
        };
        self.note_user_move();
        self.select_node(parent);
    }

    /// `Ctrl+↓`: the selection walks down a level, to the child it last came up from
    /// while that is still a child, else to the first child: a host's sources by name, a
    /// source's sessions in card order.
    fn descend(&mut self, state: &crate::state::State) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let children = node_children(&node, state);
        let child = self
            .trail
            .get(&node)
            .filter(|child| children.contains(child))
            .or_else(|| children.first())
            .cloned();
        if let Some(child) = child {
            self.note_user_move();
            self.select_node(child);
        }
    }

    /// Records a selection move the user or a caller of xmux made, as opposed to one a
    /// rebuild made. Such a move ends a full re-scan's collapse, so the scanning host
    /// card it lands on shows its own screen.
    fn note_user_move(&mut self) {
        self.interest = Interest::Selected;
        self.rescan_collapse = None;
    }

    fn move_selection(&mut self, delta: isize) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.note_user_move();
        // A selected section title is not a card of the step, so the step starts between
        // the cards around it: forward reaches the first card under it, backward the last
        // card above it.
        let cur = match sel.iter().position(|&i| i == self.selected) {
            Some(p) => p as isize,
            None => {
                let before = sel.iter().filter(|&&i| i < self.selected).count() as isize;
                if delta > 0 {
                    before - 1
                } else {
                    before
                }
            }
        };
        let n = sel.len() as isize;
        let next = ((cur + delta) % n + n) % n;
        self.set_selected(sel[next as usize]);
    }

    /// Vertical navigation shared by ↑/↓, k/j, AND the plain scroll wheel, so the wheel
    /// moves the selection exactly as the arrows do: prev/next card linearly across the
    /// whole flat list (wraps). The flat card list has no levels, so this is a plain
    /// linear step - the same as [`Switcher::move_selection`].
    fn nav_vertical(&mut self, delta: isize) {
        self.move_selection(delta);
    }

    /// Horizontal navigation (←/→): the selection lands on the first card of the
    /// previous/next CATEGORY. It and the vertical step name the two things the list is
    /// made of - one walks the cards, the other walks the categories - so a list of many
    /// hosts is crossed without stepping over every session between them.
    /// Wraps at both ends, as the vertical step does.
    ///
    /// A category is a source that has sessions, the no-session group, or the disconnected group
    /// ([`category_of_row`]). Landing is always on the category's first card: its first
    /// session, or the band's first host card. Leaving is from ANY card of it, so a
    /// selection deep inside the band steps straight out.
    ///
    /// Neither step is defined by where a card sits on screen, so both mean the same
    /// thing in the side column and in the portrait band, whose cards flow down a column
    /// and then right.
    fn nav_horizontal(&mut self, delta: isize) {
        let heads = self.category_heads();
        if heads.is_empty() {
            return;
        }
        let here = self
            .rows
            .get(self.selected)
            .map(|r| category_of_row(&r.reference))
            .and_then(|cat| heads.iter().position(|(c, _)| *c == cat))
            .unwrap_or(0) as isize;
        let n = heads.len() as isize;
        let next = ((here + delta) % n + n) % n;
        self.note_user_move();
        self.set_selected(heads[next as usize].1);
    }

    /// Each category in list order paired with its first selectable card - the landing
    /// points of a horizontal step. The cards of one category are contiguous (the
    /// flatten emits a section and its sessions together, and sinks every source with
    /// nothing to show to the host band at the end), so one entry per category is one
    /// place to land.
    fn category_heads(&self) -> Vec<(NavCategory, usize)> {
        let mut heads: Vec<(NavCategory, usize)> = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            if !r.selectable() {
                continue;
            }
            let cat = category_of_row(&r.reference);
            if !heads.iter().any(|(c, _)| *c == cat) {
                heads.push((cat, i));
            }
        }
        heads
    }

    fn move_to(&mut self, pos: isize) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.note_user_move();
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

    /// The row of the session card `source/name`.
    #[cfg(test)]
    pub(crate) fn session_row(&self, source: &str, name: &str) -> Option<usize> {
        self.row_of_session(&Address::new(source, name))
    }

    /// The card the selection is on, as an identity a later look can compare with: the
    /// same card across a rebuild that moved its row.
    #[cfg(test)]
    pub(crate) fn selected_card(&self) -> Option<RowRef> {
        self.current_ref().cloned()
    }

    /// Whether the selection names a different node than `before`.
    pub(crate) fn selection_moved_from(&self, before: &Option<Node>) -> bool {
        self.selected_node() != *before
    }

    /// What the hint bar offers about the selected node after a selection move: its most
    /// relevant keys, read from the key table, and one fact about it. A session offers its
    /// terminal and a sibling session and states its windows; a source offers its screen
    /// (or a new session when it is empty) and a re-scan of its host, and states its
    /// sessions or its state word with the reason behind it; a host offers its screen and
    /// a re-scan and states its state word; anything still scanning offers the filter and
    /// says so. With `nav_focused` false the terminal view holds the focus, where a bare key
    /// goes to the pane, so only the prefix keys are offered.
    pub(crate) fn selection_hint(
        &self,
        state: &crate::state::State,
        nav_focused: bool,
    ) -> Option<(Vec<crate::state::chrome::HintKey>, String)> {
        use crate::model::keys::{entry_for, KeyCommand};
        let first_line = |source: &str| {
            state
                .groups
                .iter()
                .find(|g| g.source == source)
                .and_then(|g| g.err.as_deref())
                .and_then(|e| e.lines().map(str::trim).find(|l| !l.is_empty()))
                .unwrap_or_default()
                .to_string()
        };
        let with_reason = |word: &str, reason: String| {
            if reason.is_empty() {
                word.to_string()
            } else {
                format!("{word}: {reason}")
            }
        };
        let (commands, fact): (&[KeyCommand], String) = match self.selected_node()? {
            Node::Session(address) => {
                let sess = state
                    .groups
                    .iter()
                    .find(|g| g.source == address.source)
                    .and_then(|g| g.sessions.iter().find(|s| s.name == address.session));
                (
                    &[KeyCommand::FocusTerminal, KeyCommand::NewSession],
                    sess.map(session_facts).unwrap_or_default(),
                )
            }
            Node::Source(source) => {
                let group = state.groups.iter().find(|g| g.source == source);
                if state.scanning.contains(&source) {
                    (&[KeyCommand::Filter], "scanning".into())
                } else if let Some(kind) = group.and_then(crate::model::Group::failure) {
                    use crate::model::FailureKind;
                    let word = tree::host_state_word(
                        false,
                        kind == FailureKind::Blocked,
                        kind == FailureKind::ListFailed,
                        true,
                    );
                    (
                        &[KeyCommand::FocusTerminal, KeyCommand::RescanHost],
                        with_reason(word, first_line(&source)),
                    )
                } else {
                    let count = group.map_or(0, |g| g.sessions.len());
                    if count == 0 {
                        (
                            &[KeyCommand::NewSession, KeyCommand::RescanHost],
                            tree::host_state_word(false, false, false, false).into(),
                        )
                    } else {
                        let method = state.refresh_words(&source);
                        (
                            &[KeyCommand::FocusTerminal, KeyCommand::RescanHost],
                            format!("{count} sessions, {method}"),
                        )
                    }
                }
            }
            Node::Host(machine) => {
                let fact = match host_failure(state, &machine) {
                    Some(kind) => {
                        let source = self.current_source().unwrap_or_default();
                        let word = tree::host_state_word(
                            false,
                            kind == crate::model::FailureKind::Blocked,
                            false,
                            true,
                        );
                        with_reason(word, first_line(&source))
                    }
                    None if host_scanning(state, &machine) => "scanning".into(),
                    None => {
                        let n = state
                            .groups
                            .iter()
                            .filter(|g| crate::session::machine_of(&g.source) == machine)
                            .count();
                        let s = if n == 1 { "" } else { "s" };
                        format!("{}, {n} source{s}", tree::HOST_REACHABLE)
                    }
                };
                (&[KeyCommand::FocusTerminal, KeyCommand::RescanHost], fact)
            }
        };
        let keys = commands
            .iter()
            .filter_map(|c| entry_for(*c))
            .filter(|e| nav_focused || e.prefixed())
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

    /// The source the selection acts on. A session's and a source's own; for a host, the
    /// source the row standing for it names (its card's login source, or the source of
    /// the title or card whose host half is selected), which is what a login, a host
    /// re-scan and a logout address. `None` for a host with no row on the list.
    pub(crate) fn current_source(&self) -> Option<String> {
        match self.selected_node()? {
            Node::Session(address) => Some(address.source),
            Node::Source(source) => Some(source),
            Node::Host(machine) => self
                .current_ref()
                .and_then(row_source)
                .filter(|source| crate::session::machine_of(source) == machine)
                .map(str::to_owned),
        }
    }

    pub(crate) fn current_unreachable_screen(&self, state: &crate::state::State) -> bool {
        self.current_view_screen(state) == Some(ViewScreen::Unreachable)
    }

    /// True when the selected host's screen carries the login pane: the host is down and
    /// a login answers it, or its pane was opened from the hosts to check. A keystroke
    /// typed while the terminal view is focused then drives that pane rather than
    /// reaching a session.
    pub(crate) fn current_host_blocked(&self) -> bool {
        let Some(Node::Host(machine)) = self.selected_node() else {
            return false;
        };
        matches!(self.current_ref(), Some(RowRef::Machine { blocked: true, machine: m, .. }) if *m == machine)
            || self
                .login_target
                .as_deref()
                .is_some_and(|source| crate::session::machine_of(source) == machine)
    }

    /// Whether the selected host's screen carries the login pane, which then takes the keys
    /// typed while the terminal view holds the focus. Only a host's screen carries it: a
    /// source refused until a login states its failure and leaves the login to its host.
    pub(crate) fn login_pane_shown(&self, state: &crate::state::State) -> bool {
        matches!(self.selected_node(), Some(Node::Host(_)))
            && (self.current_host_blocked()
                || self.current_view_screen(state) == Some(ViewScreen::Login))
    }

    /// Which screen the terminal view shows in place of the grid, or `None` for a session.
    /// It is the screen of the shown node: the soft selection's while the pointer is on a
    /// nav target, else the hard selection's.
    pub(crate) fn current_view_screen(&self, state: &crate::state::State) -> Option<ViewScreen> {
        let displayed = (!state.displayed.source.is_empty() && !state.displayed.session.is_empty())
            .then(|| Address::new(&state.displayed.source, &state.displayed.session));
        let node = self.shown_node();
        if let Some(Node::Host(machine)) = &node {
            let login_open = self
                .login_target
                .as_deref()
                .is_some_and(|source| crate::session::machine_of(source) == machine)
                && state.groups.iter().any(|g| {
                    crate::session::machine_of(&g.source) == machine && g.failure().is_some()
                });
            let login_reported = state
                .login
                .as_ref()
                .is_some_and(|draft| crate::session::machine_of(&draft.source) == machine)
                && state
                    .login_reports
                    .get(machine.as_str())
                    .and_then(crate::model::LoginFailure::of_login)
                    .is_some();
            if login_open || login_reported {
                return Some(ViewScreen::Login);
            }
            return Some(crate::model::choose_host_screen(
                host_failure(state, machine),
                host_scanning(state, machine),
            ));
        }
        let selected_source = match &node {
            Some(Node::Source(source)) => Some(source.as_str()),
            _ => None,
        };
        let selected_address = match &node {
            Some(Node::Session(address)) => Some(address.clone()),
            _ => None,
        };
        let group = selected_source
            .and_then(|source| state.groups.iter().find(|group| group.source == source));
        let scanning = match &node {
            Some(Node::Source(source)) => state.scanning.contains(source),
            None => !state.scanning.is_empty(),
            _ => false,
        };
        crate::model::choose_view_screen(
            selected_source,
            selected_address.as_ref(),
            group.and_then(crate::model::Group::failure),
            scanning,
            group.is_some_and(|group| group.sessions.is_empty()),
            self.own_session.as_ref(),
            displayed
                .as_ref()
                .map(|address| crate::model::ConfirmedDisplay {
                    address,
                    collapsed_into_selection: self.rescan_collapse.as_ref() == Some(address)
                        && selected_source == Some(address.source.as_str())
                        && matches!(self.current_ref(), Some(RowRef::Host { .. })),
                }),
        )
    }

    /// The node a view screen of `kind` is about, and the address it is reached by: the
    /// session for the self-session state, a source with an empty session half for a
    /// source's states, and for a host the source its login and probes address. `None`
    /// before anything is selected.
    pub(crate) fn view_subject(&self, kind: ViewScreen) -> Option<(Node, Address)> {
        let node = self.shown_node()?;
        let address = match (&node, kind) {
            (Node::Session(address), _) => address.clone(),
            (Node::Source(source), _) => Address::new(source, ""),
            (Node::Host(machine), _) => {
                let source = match &self.hover {
                    Some((reference, _)) => row_source(reference).map(str::to_owned),
                    None => self.current_source(),
                };
                Address::new(source.unwrap_or_else(|| machine.clone()), "")
            }
        };
        Some((node, address))
    }

    /// The links the screen of `node` offers, in the order the arrow keys walk them: a
    /// host's sources by name, each with its session count or state; a source's host and
    /// then its sessions in card order. A session's grid offers none.
    pub(crate) fn screen_links(
        &self,
        node: &Node,
        state: &crate::state::State,
    ) -> Vec<crate::ui::chrome::ScreenLink> {
        use crate::ui::chrome::ScreenLink;
        match node {
            Node::Host(machine) => state
                .groups
                .iter()
                .filter(|g| crate::session::machine_of(&g.source) == machine)
                .map(|g| {
                    // A source is named by its mux, and only by a mux an answer
                    // confirmed, as its card and its own screen name it: otherwise by
                    // its id.
                    let answered = g.err.is_none() && !state.scanning.contains(&g.source);
                    let mux = state.chrome.source_mux(&g.source);
                    let label = if mux.is_empty()
                        || !crate::session::mux_may_be_named(&g.source, answered)
                    {
                        g.source.clone()
                    } else {
                        mux.to_string()
                    };
                    let value = if state.scanning.contains(&g.source) {
                        "scanning".to_string()
                    } else if let Some(kind) = g.failure() {
                        use crate::model::FailureKind;
                        tree::host_state_word(
                            false,
                            kind == FailureKind::Blocked,
                            kind == FailureKind::ListFailed,
                            true,
                        )
                        .to_string()
                    } else {
                        match g.sessions.len() {
                            0 => tree::host_state_word(false, false, false, false).to_string(),
                            1 => "1 session".to_string(),
                            n => format!("{n} sessions"),
                        }
                    };
                    ScreenLink {
                        node: Node::Source(g.source.clone()),
                        label,
                        value,
                    }
                })
                .collect(),
            Node::Source(source) => {
                let machine = crate::session::machine_of(source);
                let mut links = vec![ScreenLink {
                    node: Node::Host(machine.to_string()),
                    label: machine.to_string(),
                    value: String::new(),
                }];
                if let Some(g) = state
                    .groups
                    .iter()
                    .find(|g| g.source == *source && g.err.is_none())
                {
                    links.extend(g.sessions.iter().map(|sess| ScreenLink {
                        node: Node::Session(sess.address()),
                        label: sess.name.clone(),
                        value: session_facts(sess),
                    }));
                }
                links
            }
            Node::Session(_) => Vec::new(),
        }
    }

    /// The links of the shown screen, empty while it is a session's grid.
    pub(crate) fn shown_links(
        &self,
        state: &crate::state::State,
    ) -> Vec<crate::ui::chrome::ScreenLink> {
        match self.shown_node() {
            Some(node) if self.current_view_screen(state).is_some() => {
                self.screen_links(&node, state)
            }
            _ => Vec::new(),
        }
    }

    /// Which link of the shown screen is hard-selected and which is under the pointer.
    /// Both belong to the terminal view, so neither is drawn while the nav holds the focus
    /// or while the nav's soft selection is showing another screen there.
    pub(crate) fn link_marks(&self) -> (Option<usize>, Option<usize>) {
        if !self.terminal_view || self.hover.is_some() {
            return (None, None);
        }
        (Some(self.link), self.link_hover)
    }

    /// Where the soft selections stand, as the paint draws them: the nav row and part
    /// under the pointer, and the screen link under it.
    pub(crate) fn soft_marks(&self) -> (Option<(usize, Part)>, Option<usize>) {
        let nav = self
            .hover
            .as_ref()
            .and_then(|(reference, part)| self.row_matching(reference).map(|i| (i, *part)));
        (nav, self.link_hover)
    }

    /// The arrow keys on a host's or a source's screen while the terminal view holds the
    /// focus: they walk its links, stopping at both ends.
    pub(crate) fn step_link(&mut self, delta: isize, state: &crate::state::State) {
        let n = self.shown_links(state).len();
        if n == 0 {
            return;
        }
        self.link = (self.link as isize + delta).clamp(0, n as isize - 1) as usize;
        self.link_node = self
            .shown_links(state)
            .get(self.link)
            .map(|l| l.node.clone());
    }

    /// Executes link `index` of the shown screen: the node it names becomes the hard
    /// selection and its screen opens. The link standing for the node just left is
    /// selected on the new screen, so a step back is one Enter away.
    pub(crate) fn open_link(&mut self, index: usize, state: &crate::state::State) -> bool {
        let Some(link) = self.shown_links(state).into_iter().nth(index) else {
            return false;
        };
        let before = self.selected_node();
        self.note_user_move();
        self.link_hover = None;
        self.select_node(link.node);
        if let Some(before) = before {
            if let Some(node) = self.selected_node() {
                if let Some(i) = self
                    .screen_links(&node, state)
                    .iter()
                    .position(|l| l.node == before)
                {
                    self.link = i;
                    self.link_node = Some(before);
                }
            }
        }
        true
    }

    /// Opens the hard-selected link of the shown screen (Enter in the terminal view).
    pub(crate) fn open_selected_link(&mut self, state: &crate::state::State) -> bool {
        self.open_link(self.link, state)
    }

    // --- preview ------------------------------------------------------------

    fn on_focus_changed(&mut self) {
        // The shown node's session, never xmux's OWN session. Emptying the target here is
        // what makes the refusal total: the target is the one value the display reconcile,
        // the attach, and the mux-side switch all read, so none of them can reach this
        // session by another path.
        self.terminal_view_target = match self.shown_node() {
            Some(Node::Session(address))
                if !self.is_own_session(&address.source, &address.session) =>
            {
                TerminalViewTarget {
                    source: address.source,
                    target: address.session,
                }
            }
            _ => TerminalViewTarget::default(),
        };
    }

    /// The session the shown node attaches to, `None` for a host or a source.
    pub fn current_attach_target(
        &self,
        _state: &crate::state::State,
    ) -> Option<TerminalViewTarget> {
        let target = self.terminal_view_target.clone();
        (!target.target.is_empty()).then_some(target)
    }

    /// The source the selection is on, whose control-mode client the app keeps connected
    /// on every selection move, so its `list-sessions` populates the nav even before any
    /// session is selected.
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
    /// or the nav following the session the mux moved its own display client onto. Both
    /// name a card and move to it, and nothing downstream tells them apart, so they share
    /// one entry point. Neither waits for a card that is not on the list yet.
    pub fn select_address(&mut self, address: &Address) -> bool {
        match self.row_of_session(address) {
            Some(i) if self.selected_node() != Some(Node::Session(address.clone())) => {
                self.note_user_move();
                self.set_selected(i);
                true
            }
            _ => false,
        }
    }

    // --- refresh ------------------------------------------------------------

    /// Resets every host to its scanning skeleton and signals the event loop to
    /// re-kick the streaming probes (the `r` re-scan) - sessions and panes stream
    /// back in exactly as on first launch. The selection does not drift: the session
    /// under it becomes the awaited [`Interest`], so the selection rests on that
    /// session's source card through the skeleton phase (the lineage of a vanished
    /// session) and returns to the session the instant its source re-streams it.
    pub fn request_rescan(&mut self, state: &mut crate::state::State) {
        let selected = match self.selected_node() {
            Some(Node::Session(address)) => Some(address),
            _ => None,
        };
        self.rescan_collapse = selected.clone();
        if let Some(address) = selected {
            self.interest = Interest::Awaiting(address);
        }
        self.reopen_numbers();
        state.scanning = state.groups.iter().map(|g| g.source.clone()).collect();
        for g in state.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        self.rescan_kick = true;
        self.reattach_kick = true;
        self.rebuild(state);
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
            // The card is the same card under its new name, so it keeps its number.
            let old = CardId::Session(source.clone(), from.clone());
            if let Some(n) = self.numbers.remove(&old) {
                self.numbers
                    .insert(CardId::Session(source.clone(), to.clone()), n);
            }
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
        state.scanning.remove(&source);
        state.scan_deadlines.remove(&source);
        // The failure run, counted where every result lands so no path can skip it: a
        // result that failed lengthens it, one that answered clears it. It is shown, not
        // acted on - see `State::failure_runs`.
        match &err {
            Some(_) => *state.failure_runs.entry(source.clone()).or_insert(0) += 1,
            None => {
                state.failure_runs.remove(&source);
                state
                    .last_reached
                    .insert(source.clone(), std::time::SystemTime::now());
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
        state.scanning.insert(source.clone());
        state.scan_deadlines.remove(&source);
        state.groups.push(Group {
            source,
            err: None,
            sessions: Vec::new(),
        });
        self.rebuild(state);
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
        g.err = None;
        state.scanning.insert(source.to_string());
        state.scan_deadlines.remove(source);
        self.rebuild(state);
    }

    /// Puts every source `machine` serves in flight for a re-scan of that machine alone.
    /// Each card spins or keeps the sessions it lists until its answer lands, so the
    /// list and its numbers hold still while the machine is asked again.
    pub fn mark_machine_scanning(&mut self, machine: &str, state: &mut crate::state::State) {
        for g in state.groups.iter_mut() {
            if crate::session::machine_of(&g.source) == machine {
                g.err = None;
                state.scanning.insert(g.source.clone());
                state.scan_deadlines.remove(&g.source);
            }
        }
        self.rebuild(state);
    }

    /// Drops a source whose MACHINE the roster no longer names, and everything the nav
    /// held for it. Idempotent: a source the nav does not show is left alone.
    ///
    /// A selection on the dropped card moves along its lineage, as on every rebuild.
    pub fn remove_source(&mut self, source: &str, state: &mut crate::state::State) {
        if !state.groups.iter().any(|g| g.source == source) {
            return;
        }
        state.groups.retain(|g| g.source != source);
        state.scanning.remove(source);
        state.scan_deadlines.remove(source);
        state.failure_runs.remove(source);
        state.last_reached.remove(source);
        state.live_sources.remove(source);
        state.host_details.remove(source);
        self.rebuild(state);
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

/// One row of the table of the hosts to check: a host in a problem state, the cause, the
/// reason its last answer gave, and whether the hiding leaves it without a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckEntry {
    pub(crate) source: String,
    /// The host as its card names it.
    pub(crate) label: String,
    pub(crate) kind: crate::model::FailureKind,
    pub(crate) reason: String,
}

/// The card a number is kept for: a session by its address, a source's card by its
/// source, a host's card by its host. A session that ends and later returns under the
/// same address is the same card and takes its number back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CardId {
    Session(String, String),
    Host(String),
    Machine(String),
}

/// The card `reference` names, or `None` for a section title, which carries no number.
fn card_id(reference: &RowRef) -> Option<CardId> {
    match reference {
        RowRef::Session { sess } => Some(CardId::Session(sess.source.clone(), sess.name.clone())),
        RowRef::Host { source, .. } => Some(CardId::Host(source.clone())),
        RowRef::Machine { machine, .. } => Some(CardId::Machine(machine.clone())),
        RowRef::Section { .. } => None,
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
        RowRef::Machine { machine, .. } => (machine, "", ""),
        RowRef::Session { sess } => (
            crate::session::machine_of(&sess.source),
            &row.mux,
            &sess.name,
        ),
    }
}

/// The category reached by a horizontal step. Vertical steps visit every card.
#[derive(PartialEq, Eq)]
enum NavCategory {
    Source(String),
    NoSession,
    Disconnected,
}

fn category_of_row(reference: &RowRef) -> NavCategory {
    match reference {
        RowRef::Host {
            unreachable: true, ..
        }
        | RowRef::Host {
            list_failed: true, ..
        }
        | RowRef::Host { scanning: true, .. }
        | RowRef::Machine { .. } => NavCategory::Disconnected,
        RowRef::Host { .. } => NavCategory::NoSession,
        RowRef::Section { source, .. } => NavCategory::Source(source.clone()),
        RowRef::Session { sess } => NavCategory::Source(sess.source.clone()),
    }
}

/// The session a card belongs to (a session card), or `None` for any other row. Lets
/// selection tracking, kill-confirm survival, and `select_address` treat a session card
/// as that session.
fn session_addr_of(reference: &RowRef) -> Option<Address> {
    match reference {
        RowRef::Session { sess } => Some(sess.address()),
        RowRef::Host { .. } | RowRef::Section { .. } | RowRef::Machine { .. } => None,
    }
}

/// The source a row stands on: a session's, a title's or a source card's own, and the
/// source a host's card logs in through.
fn row_source(reference: &RowRef) -> Option<&str> {
    match reference {
        RowRef::Host { source, .. }
        | RowRef::Section { source }
        | RowRef::Machine { source, .. } => Some(source),
        RowRef::Session { sess } => Some(&sess.source),
    }
}

/// Whether two row references name the same row across a rebuild: a session by its
/// address, a source by its id whether it shows as its section title or its host-state
/// card, a host's card by its host. The selection holds on that identity, so a source
/// gaining or losing its sessions keeps it.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Session { .. }, RowRef::Session { .. }) => {
            session_addr_of(a) == session_addr_of(b)
        }
        (RowRef::Machine { machine: x, .. }, RowRef::Machine { machine: y, .. }) => x == y,
        (
            RowRef::Section { source: x } | RowRef::Host { source: x, .. },
            RowRef::Section { source: y } | RowRef::Host { source: y, .. },
        ) => x == y,
        _ => false,
    }
}

/// The node `part` of a row names: the host half of a title or a source card names the
/// host, a title's other half and a source card the source, a host's card the host, and a
/// session card the session.
fn node_of(reference: &RowRef, part: Part) -> Node {
    match reference {
        RowRef::Session { sess } => Node::Session(sess.address()),
        RowRef::Section { source } | RowRef::Host { source, .. } if part == Part::Host => {
            Node::Host(crate::session::machine_of(source).to_string())
        }
        RowRef::Section { source } | RowRef::Host { source, .. } => Node::Source(source.clone()),
        RowRef::Machine { machine, .. } => Node::Host(machine.clone()),
    }
}

/// Whether the inventory still holds `node`: a host while any source of it is listed, a
/// source while it is listed, a session while its source lists it.
fn node_exists(node: &Node, state: &crate::state::State) -> bool {
    match node {
        Node::Host(machine) => state
            .groups
            .iter()
            .any(|g| crate::session::machine_of(&g.source) == machine),
        Node::Source(source) => state.groups.iter().any(|g| g.source == *source),
        Node::Session(address) => state.groups.iter().any(|g| {
            g.source == address.source
                && g.err.is_none()
                && g.sessions.iter().any(|s| s.name == address.session)
        }),
    }
}

/// The nodes one level below `node`, in the order a step down picks from: a host's
/// sources by name, a source's sessions in card order.
fn node_children(node: &Node, state: &crate::state::State) -> Vec<Node> {
    match node {
        Node::Host(machine) => {
            let mut sources: Vec<&str> = state
                .groups
                .iter()
                .map(|g| g.source.as_str())
                .filter(|s| crate::session::machine_of(s) == machine)
                .collect();
            sources.sort_unstable();
            sources
                .into_iter()
                .map(|s| Node::Source(s.to_string()))
                .collect()
        }
        Node::Source(source) => state
            .groups
            .iter()
            .filter(|g| g.source == *source && g.err.is_none())
            .flat_map(|g| g.sessions.iter().map(|s| Node::Session(s.address())))
            .collect(),
        Node::Session(_) => Vec::new(),
    }
}

/// The failure a host as a whole is in: the failure every one of its sources shares, a
/// login one when any of them needs a login. `None` while any source answered or is still
/// waiting on its answer, since a host is down only when none of its sources connected.
pub(crate) fn host_failure(
    state: &crate::state::State,
    machine: &str,
) -> Option<crate::model::FailureKind> {
    use crate::model::FailureKind;
    let mut blocked = false;
    let mut any = false;
    for g in state
        .groups
        .iter()
        .filter(|g| crate::session::machine_of(&g.source) == machine)
    {
        any = true;
        match g.failure() {
            _ if state.scanning.contains(&g.source) => return None,
            Some(FailureKind::Blocked) => blocked = true,
            Some(FailureKind::Unreachable) => {}
            _ => return None,
        }
    }
    any.then_some(if blocked {
        FailureKind::Blocked
    } else {
        FailureKind::Unreachable
    })
}

/// Whether every source of a host is still waiting on its answer.
pub(crate) fn host_scanning(state: &crate::state::State, machine: &str) -> bool {
    let mut sources = state
        .groups
        .iter()
        .filter(|g| crate::session::machine_of(&g.source) == machine)
        .peekable();
    sources.peek().is_some() && sources.all(|g| state.scanning.contains(&g.source))
}

/// What a session's link and hint say about it: its windows and whether a client is on it.
fn session_facts(sess: &Session) -> String {
    let mut facts = Vec::new();
    if sess.windows > 0 {
        let s = if sess.windows == 1 { "" } else { "s" };
        facts.push(format!("{} window{s}", sess.windows));
    }
    if sess.attached {
        facts.push("attached".to_string());
    }
    facts.join(", ")
}

/// The hard selection as a rebuild found it, before the rows are re-derived.
struct Prior {
    node: Option<Node>,
    row: Option<RowRef>,
    /// Whether the node had no nav target of its own.
    deep: bool,
    index: usize,
}

impl Target {
    fn card(row: usize) -> Self {
        Target {
            row,
            part: Part::Card,
            deep: None,
        }
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
mod tests_hierarchy;

#[cfg(test)]
mod tests_lineage;

#[cfg(test)]
mod tests_position;

#[cfg(test)]
pub(crate) mod tests_support;
