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
        if terminal && !self.terminal_view {
            self.host_band_hidden = matches!(self.current_ref(), Some(RowRef::Session { .. }));
        } else if !terminal {
            self.host_band_hidden = false;
        }
        self.terminal_view = terminal;
    }

    /// Whether the paint leaves the host band out: hidden by the move into the terminal
    /// view from a session card. Prefix interactions preserve this decision, but a
    /// selection on a host card paints the band, since a selected card is always painted.
    fn band_unpainted(&self) -> bool {
        self.host_band_hidden && !matches!(self.current_ref(), Some(RowRef::Host { .. }))
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
        let prior = self.current_ref().cloned();
        let prior_index = self.selected;

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
        let target = self.resolve_selection(prior.as_ref(), &old_rows, prior_index, state);
        self.set_selected(target, state);
    }

    /// The row the selection takes after a rebuild, read from [`Interest`].
    ///
    /// A card that APPEARS takes the selection only when the interest names it: the first
    /// session card while nothing is chosen yet, or the awaited session. Anything else
    /// holds the prior card by identity, and a prior card that DISAPPEARED moves along
    /// its lineage ([`Switcher::lineage_row`]). No path picks a position of its own.
    fn resolve_selection(
        &mut self,
        prior: Option<&RowRef>,
        old_rows: &[Row],
        prior_index: usize,
        state: &crate::state::State,
    ) -> usize {
        let first_selectable = || self.rows.iter().position(Row::selectable).unwrap_or(0);
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
                        i
                    }
                    None => first_selectable(),
                }
            }
            Interest::Awaiting(address) => {
                if let Some(i) = self.row_of_session(&address) {
                    self.interest = Interest::Selected;
                    self.rescan_collapse = None;
                    return i;
                }
                if !state.scanning.contains(&address.source) {
                    self.interest = Interest::Selected;
                }
                self.lineage_row(prior, old_rows, prior_index)
                    .unwrap_or_else(first_selectable)
            }
            Interest::Selected => self
                .lineage_row(prior, old_rows, prior_index)
                .unwrap_or_else(first_selectable),
        }
    }

    /// Where the selection on `prior` goes on the rebuilt rows: `prior` itself when it
    /// survives, otherwise the nearest surviving card of its lineage.
    ///
    /// - a session goes to its source's card (the section title, or the source's
    ///   host-state card once it has no session to show);
    /// - a source goes to its machine's own card (the source named by the machine
    ///   alone), else to the machine's first source card in card order;
    /// - when nothing of the machine survives, to the card that now holds the vanished
    ///   card's place: the first card after it in the prior card order that survived,
    ///   else the last surviving card before it.
    ///
    /// A source keeps one identity whether it shows as a section title or as a
    /// host-state card, so a source that gains or loses its sessions is the same card.
    /// `None` only when `prior` is `None` or no prior card survives at all.
    fn lineage_row(
        &self,
        prior: Option<&RowRef>,
        old_rows: &[Row],
        prior_index: usize,
    ) -> Option<usize> {
        let prior = prior?;
        let source_row = |source: &str| {
            self.rows
                .iter()
                .position(|r| card_source(&r.reference) == Some(source))
        };
        if let Some(i) = self.row_matching(prior) {
            return Some(i);
        }
        let source = match prior {
            RowRef::Session { sess } => sess.source.as_str(),
            RowRef::Host { source, .. } | RowRef::Section { source } => source.as_str(),
        };
        let machine = crate::session::machine_of(source);
        if let Some(i) = source_row(source)
            .or_else(|| source_row(machine))
            .or_else(|| {
                self.rows.iter().position(|r| {
                    card_source(&r.reference)
                        .is_some_and(|s| crate::session::machine_of(s) == machine)
                })
            })
        {
            return Some(i);
        }
        let survivor = |r: &Row| {
            r.selectable()
                .then(|| self.row_matching(&r.reference))
                .flatten()
        };
        let at = prior_index.min(old_rows.len());
        old_rows
            .get(at + 1..)
            .unwrap_or_default()
            .iter()
            .find_map(survivor)
            .or_else(|| old_rows[..at].iter().rev().find_map(survivor))
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

    fn set_selected(&mut self, idx: usize, state: &crate::state::State) {
        if self.rows.is_empty() {
            return;
        }
        let idx = idx.min(self.rows.len() - 1);
        if self.rows.get(idx).and_then(|row| match &row.reference {
            RowRef::Host { source, .. } => Some(source.as_str()),
            _ => None,
        }) != self.login_target.as_deref()
        {
            self.login_target = None;
        }
        self.selected = idx;
        self.on_focus_changed(state);
    }

    /// Records a selection move the user or a caller of xmux made, as opposed to one a
    /// rebuild made. Such a move ends a full re-scan's collapse, so the scanning host
    /// card it lands on shows its own screen.
    fn note_user_move(&mut self) {
        self.interest = Interest::Selected;
        self.rescan_collapse = None;
    }

    fn move_selection(&mut self, delta: isize, state: &crate::state::State) {
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
    /// A category is a source that has sessions, the no-session group, or the disconnected group
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
            .and_then(|cat| heads.iter().position(|(c, _)| *c == cat))
            .unwrap_or(0) as isize;
        let n = heads.len() as isize;
        let next = ((here + delta) % n + n) % n;
        self.note_user_move();
        self.set_selected(heads[next as usize].1, state);
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

    fn move_to(&mut self, pos: isize, state: &crate::state::State) {
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
    /// (or a new session when it is empty) and a re-scan of that host, and states its state word with
    /// the reason behind it; a host still scanning offers the filter and says so. With
    /// `nav_focused` false the terminal view holds the focus, where a bare key goes to the
    /// pane, so only the prefix keys are offered.
    pub(crate) fn selection_hint(
        &self,
        state: &crate::state::State,
        nav_focused: bool,
    ) -> Option<(Vec<crate::state::chrome::HintKey>, String)> {
        use crate::model::keys::{entry_for, KeyCommand};
        let (commands, fact): (&[KeyCommand], String) = match self.current_ref()? {
            RowRef::Section { source } => {
                let count = state
                    .groups
                    .iter()
                    .find(|g| &g.source == source)
                    .map_or(0, |g| g.sessions.len());
                let method = state.refresh_words(source);
                (
                    &[KeyCommand::FocusTerminal, KeyCommand::RescanHost],
                    format!("{count} sessions, {method}"),
                )
            }
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
                    &[KeyCommand::FocusTerminal, KeyCommand::RescanHost]
                } else {
                    &[KeyCommand::NewSession, KeyCommand::RescanHost]
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

    pub(crate) fn current_source(&self) -> Option<String> {
        match self.current_ref()? {
            RowRef::Host { source, .. } | RowRef::Section { source, .. } => Some(source.clone()),
            RowRef::Session { sess } => Some(sess.source.clone()),
        }
    }

    pub(super) fn current_host_unreachable(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { unreachable, .. }) if *unreachable)
    }

    pub(crate) fn current_unreachable_screen(&self, state: &crate::state::State) -> bool {
        self.current_view_screen(state) == Some(ViewScreen::Unreachable)
    }

    /// True when the selected host failed in a way the user can answer from xmux. Its
    /// terminal-view panel carries the login pane, so a keystroke typed while the
    /// terminal view is focused drives that pane rather than reaching a session.
    pub(crate) fn current_host_blocked(&self) -> bool {
        matches!(self.current_ref(), Some(RowRef::Host { blocked: true, .. }))
            || matches!(self.current_ref(), Some(RowRef::Host { source, .. }) if self.login_target.as_deref() == Some(source))
    }

    /// Which screen the terminal view shows in place of the grid, or `None` for a session.
    pub(crate) fn current_view_screen(&self, state: &crate::state::State) -> Option<ViewScreen> {
        let selected_address = self.current_screen_address(state);
        let selected_source = match self.current_ref() {
            Some(RowRef::Host { source, .. } | RowRef::Section { source }) => Some(source.as_str()),
            _ => None,
        };
        let group = selected_source
            .and_then(|source| state.groups.iter().find(|group| group.source == source));
        if selected_source.is_some_and(|source| self.login_target.as_deref() == Some(source))
            && group.and_then(crate::model::Group::failure).is_some()
        {
            return Some(ViewScreen::Login);
        }
        let scanning = match self.current_ref() {
            Some(RowRef::Host { source, .. } | RowRef::Section { source }) => {
                state.scanning.contains(source)
            }
            None => !state.scanning.is_empty(),
            _ => false,
        };
        if let Some(source) = selected_source {
            if state
                .login
                .as_ref()
                .is_some_and(|draft| draft.source == source)
                && state
                    .login_reports
                    .get(crate::session::machine_of(source))
                    .and_then(crate::model::LoginFailure::of_login)
                    .is_some()
            {
                return Some(ViewScreen::Login);
            }
        }
        let displayed = (!state.displayed.source.is_empty() && !state.displayed.session.is_empty())
            .then(|| Address::new(&state.displayed.source, &state.displayed.session));
        crate::model::choose_view_screen(
            selected_source,
            selected_address.as_ref(),
            group.and_then(crate::model::Group::failure),
            scanning,
            group.is_some_and(|group| group.sessions.is_empty()),
            self.own_session.as_ref(),
            displayed.as_ref().map(|address| crate::model::ConfirmedDisplay {
                address,
                collapsed_into_selection: self.rescan_collapse.as_ref() == Some(address)
                    && matches!(self.current_ref(), Some(RowRef::Host { source, .. }) if *source == address.source),
            }),
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
                self.note_user_move();
                self.set_selected(i, state);
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
        let selected = match self.current_ref() {
            Some(RowRef::Session { sess }) => Some(sess.address()),
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

/// The card a number is kept for: a session by its address, a host-state card by its
/// source. A session that ends and later returns under the same address is the same card
/// and takes its number back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CardId {
    Session(String, String),
    Host(String),
}

/// The card `reference` names, or `None` for a section title, which carries no number.
fn card_id(reference: &RowRef) -> Option<CardId> {
    match reference {
        RowRef::Session { sess } => Some(CardId::Session(sess.source.clone(), sess.name.clone())),
        RowRef::Host { source, .. } => Some(CardId::Host(source.clone())),
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
        | RowRef::Host { scanning: true, .. } => NavCategory::Disconnected,
        RowRef::Host { .. } => NavCategory::NoSession,
        RowRef::Section { source, .. } => NavCategory::Source(source.clone()),
        RowRef::Session { sess } => NavCategory::Source(sess.source.clone()),
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

/// The source a source card names: a section title or a host-state card. A source shows
/// as exactly one of the two on any list, the title while it has sessions to show and
/// the host-state card otherwise, so both are the one card of that source.
fn card_source(reference: &RowRef) -> Option<&str> {
    match reference {
        RowRef::Host { source, .. } | RowRef::Section { source } => Some(source),
        RowRef::Session { .. } => None,
    }
}

/// Whether two row references name the same card across a rebuild: a session by its
/// address, a source by its id whether it shows as its section title or its host-state
/// card. The selection holds on that identity, so a source gaining or losing its
/// sessions keeps it.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (card_source(a), card_source(b)) {
        (Some(x), Some(y)) => x == y,
        (None, None) => session_addr_of(a) == session_addr_of(b),
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
mod tests_lineage;

#[cfg(test)]
mod tests_position;

#[cfg(test)]
pub(crate) mod tests_support;
