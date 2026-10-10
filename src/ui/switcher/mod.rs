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
use crate::ui::cards::{self, Group, Row, RowRef};
use crate::ui::modal::{self, Input, InputMode, Modal, PopupGeometry};

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
/// position. The indent and the bold title are the whole of what marks a group: no rule
/// and no connector is painted for it. The indent lies outside the card's rect, so the
/// selection's inversion of that rect, and the hit-test that reads it, start where the
/// card does. A band one row tall
/// runs its titles and cards along one line, where an indent would mark nothing, so it
/// indents nothing.
pub(super) const CARD_INDENT: u16 = 1;

pub use crate::ui::chrome::NavBorderColors;

pub use crate::model::{NavSize, ViewLayout};

/// The collapsed width of a side nav: exactly the resting prefix, which the collapsed
/// column keeps on its bottom line with no padding. The column exists only to keep the
/// prefix in view and to be clicked open, so every further cell would be taken from the
/// terminal view. Its nav border shares the column's terminal-side edge.
pub(crate) fn collapsed_nav_width(ui_prefix: &str) -> u16 {
    UnicodeWidthStr::width(ui_prefix).min(u16::MAX as usize) as u16
}

/// The prefix hint with one cell either side: the chip a nav border row carries and
/// the chip a vertical nav's prefix hint row paints.
pub(crate) fn prefix_chip_width(ui_prefix: &str) -> u16 {
    collapsed_nav_width(ui_prefix).saturating_add(2)
}

/// Whether the prefix key list is open: a live prefix that no input popup outranks.
pub(crate) fn key_list_open(state: &crate::state::State) -> bool {
    state.chrome.armed && !state.is_inputting()
}

/// The auto horizontal nav's card height for a body of `body_rows` rows (the caller
/// passes `full_height - 1`). This is the seed a RELATIVE height resize
/// (prefix Ctrl-↑/↓ in a horizontal nav) starts from while `nav_height` is still 0 (auto),
/// so the first key adjusts the height the user actually sees.
pub fn default_nav_height(body_rows: u16) -> u16 {
    top_nav_height(body_rows)
}

/// The navigation view's height in a horizontal nav: ~40% of the body, at least a few
/// rows, but never so tall the terminal view loses its last rows. Composed with min/max (not `clamp`) so a
/// tiny body - where the floor would exceed the ceiling and `clamp` would panic - just yields
/// the small floor instead.
fn top_nav_height(body_h: u16) -> u16 {
    let want = (body_h as u32 * 2 / 5) as u16;
    let ceil = body_h.saturating_sub(3).max(1);
    want.max(3).min(ceil).max(1)
}

/// The screen regions the switcher draws into, derived ONCE per frame so the renderer,
/// the PTY sizing, and mouse hit-testing all agree (one geometry, no divergence). The
/// navigation view and terminal view split the whole area side by side (a vertical nav,
/// sized by `nav_width`) or stacked (a horizontal nav, sized by `nav_height`), parted by
/// the one-cell nav border: a rule a drag resizes the nav from. The prefix hint rests at
/// the navigation view's start: the FIRST row of a vertical nav's column, and the nav
/// border row itself in a horizontal nav, so every row a horizontal nav takes holds cards
/// and the terminal view keeps every row it owns.
/// A collapsed nav gives the cards no region: a vertical nav keeps a column as wide as
/// its collapsed width with the prefix hint on its first row, a horizontal nav keeps the
/// nav border row alone. A collapsed vertical nav's nav border takes no column of its
/// own: it runs down the column's terminal-side edge on every row below the prefix hint,
/// so the prefix keeps every one of its characters and the terminal view gains the
/// column. `nav_width == 0` is the nav-hidden sentinel: the terminal owns the whole area.
/// `nav_height == 0` means the horizontal nav's height is auto (~40% of the area).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Regions {
    pub layout: ViewLayout,
    pub nav: Rect,
    pub nav_border: Rect,
    pub terminal: Rect,
    /// The row the prefix hint chip paints: the navigation view's first row in a
    /// vertical nav, the nav border row in a horizontal nav. Empty when the nav is hidden.
    pub prefix_hint: Rect,
}

impl Default for Regions {
    fn default() -> Self {
        Self {
            layout: ViewLayout::Vertical,
            nav: Rect::default(),
            nav_border: Rect::default(),
            terminal: Rect::default(),
            prefix_hint: Rect::default(),
        }
    }
}

/// A horizontal nav's card height: a user-set `nav_height` (dragged border) clamped so
/// both views keep room, or the auto ~40% when `nav_height == 0`. min/max (not `clamp`)
/// so a tiny body cannot panic on inverted bounds.
fn top_nav_height_for(body_h: u16, nav_height: u16) -> u16 {
    if nav_height == 0 {
        top_nav_height(body_h)
    } else {
        nav_height.min(body_h.saturating_sub(2)).max(1)
    }
}

/// Splits a vertical nav's column into `(prefix hint row, card list)`: the prefix hint
/// takes the FIRST row, and the cards keep the rest. A column too short to hold both
/// gives the whole column to the cards and no prefix hint, so a tiny terminal still
/// navigates.
fn split_prefix_row(nav: Rect) -> (Rect, Rect) {
    if nav.height <= 1 {
        return (nav, Rect::default());
    }
    let r = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(nav);
    (r[0], r[1])
}

fn collapsed_prefix_row(nav: Rect) -> Rect {
    if nav.height == 0 {
        Rect::default()
    } else {
        Rect::new(nav.x, nav.y, nav.width, 1)
    }
}

/// The regions of a collapsed vertical nav: a column exactly `nav_width` wide at the
/// nav's side, its prefix hint on the first row, and the nav border on the column's
/// terminal-side edge below that row. The prefix character on that edge stays readable
/// because the border stops short of it; the terminal view keeps everything beside the
/// column.
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
        nav: Rect::default(),
        nav_border: if w == 0 {
            Rect::default()
        } else {
            Rect::new(
                edge_x,
                area.y.saturating_add(1),
                1,
                area.height.saturating_sub(1),
            )
        },
        terminal: Rect::new(terminal_x, area.y, area.width - w, area.height),
        prefix_hint: collapsed_prefix_row(nav),
    }
}

pub fn compute_regions(area: Rect, nav: NavSize) -> Regions {
    // The layout follows the attachment position: a left or right placement is a vertical
    // nav, a top or bottom one a horizontal nav. The position travels with the hidden nav
    // unchanged, so hiding it cannot flip the layout; the hidden sentinel below still
    // gives the whole area to the terminal.
    let layout = nav.position.layout();
    let (nav_width, nav_height) = (nav.width, nav.height);
    if nav_width == 0 {
        return Regions {
            layout,
            nav: Rect::default(),
            nav_border: Rect::default(),
            terminal: area,
            prefix_hint: Rect::default(),
        };
    }
    match nav.position {
        NavPosition::Left | NavPosition::Right if nav.collapsed => {
            collapsed_column(area, layout, nav_width, nav.position)
        }
        NavPosition::Floating => {
            // The terminal keeps the whole area and the nav floats over it as a box,
            // placed by the runtime over the terminal's empty space. Its prefix hint
            // rests on the box border's top-left.
            let nav = nav
                .floating
                .unwrap_or_else(|| default_floating_box(area, nav_width));
            Regions {
                layout,
                nav,
                nav_border: Rect::default(),
                terminal: area,
                prefix_hint: floating_prefix_hint(nav),
            }
        }
        NavPosition::Left => {
            let c = Layout::horizontal([
                Constraint::Length(nav_width),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            let (prefix_hint, nav) = split_prefix_row(c[0]);
            Regions {
                layout,
                nav,
                nav_border: c[1],
                terminal: c[2],
                prefix_hint,
            }
        }
        NavPosition::Right => {
            // The left column mirrored: the terminal view keeps the remainder, the border
            // and the navigation view follow on the right. The navigation view's in-region
            // layout (card flow, prefix hint) is identical at both placements.
            let c = Layout::horizontal([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(nav_width),
            ])
            .split(area);
            let (prefix_hint, nav) = split_prefix_row(c[2]);
            Regions {
                layout,
                nav,
                nav_border: c[1],
                terminal: c[0],
                prefix_hint,
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
                nav: r[0],
                nav_border: r[1],
                terminal: r[2],
                prefix_hint: r[1],
            }
        }
        NavPosition::Bottom => {
            // The top placement mirrored: the nav border is the row ABOVE the cards, and
            // the prefix hint rests on it at the row's left.
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
                nav: r[2],
                nav_border: r[1],
                terminal: r[0],
                prefix_hint: r[1],
            }
        }
    }
}

/// The floating nav's box when the runtime has not placed it yet: the top-right corner,
/// as wide as the nav and half the area tall.
pub(crate) fn default_floating_box(area: Rect, nav_width: u16) -> Rect {
    let w = nav_width.min(area.width);
    let h = (area.height / 2).max(3).min(area.height);
    Rect::new(area.right().saturating_sub(w), area.y, w, h)
}

/// The farthest the floating nav's right edge may stand off the terminal's right wall.
/// The box sits flush against the wall or up to this many cells left of it.
pub const FLOATING_MARGIN: u16 = 10;

/// Where the floating nav's box sits: its right edge within [`FLOATING_MARGIN`] of the
/// terminal's right wall, over the strip whose tallest all-blank vertical run fits the
/// box and is largest, with the box centered in that run. Falls back to the top-right
/// corner when no all-blank run fits the box. `blank` reports whether a cell carries no
/// glyph.
pub fn floating_nav_box(area: Rect, w: u16, h: u16, blank: impl Fn(u16, u16) -> bool) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let right = area.right();
    let x_lo = right
        .saturating_sub(FLOATING_MARGIN)
        .saturating_sub(w)
        .max(area.x);
    let mut best: Option<(u16, u16, u16)> = None; // (run_height, x, y)
    let mut x = right.saturating_sub(w);
    loop {
        if let Some((y, run)) = tallest_blank_run(area, x, w, &blank) {
            if run >= h && best.is_none_or(|(bh, _, _)| run > bh) {
                best = Some((run, x, y + (run - h) / 2));
            }
        }
        if x == x_lo {
            break;
        }
        x = x.saturating_sub(1);
        if x < x_lo {
            break;
        }
    }
    match best {
        Some((_, x, y)) => Rect::new(x, y, w, h),
        None => default_floating_box(area, w),
    }
}

/// The tallest vertical run of all-blank cells in the `w`-wide strip starting at column
/// `x`, as `(row, height)`. `None` when no cell in the strip is blank.
fn tallest_blank_run(
    area: Rect,
    x: u16,
    w: u16,
    blank: &impl Fn(u16, u16) -> bool,
) -> Option<(u16, u16)> {
    let mut best: Option<(u16, u16)> = None;
    let mut run_y: Option<u16> = None;
    let mut run_h = 0u16;
    let close = |run_y: &mut Option<u16>, run_h: &mut u16, best: &mut Option<(u16, u16)>| {
        if let Some(sy) = run_y.take() {
            if best.is_none_or(|(_, bh)| *run_h > bh) {
                *best = Some((sy, *run_h));
            }
            *run_h = 0;
        }
    };
    for y in area.y..area.bottom() {
        let all_blank = (x..x + w).all(|cx| blank(cx, y));
        if all_blank {
            if run_y.is_none() {
                run_y = Some(y);
                run_h = 1;
            } else {
                run_h += 1;
            }
        } else {
            close(&mut run_y, &mut run_h, &mut best);
        }
    }
    close(&mut run_y, &mut run_h, &mut best);
    best
}

/// The prefix hint's row on a floating nav's box: the box's top border, on its left.
fn floating_prefix_hint(box_rect: Rect) -> Rect {
    if box_rect.height == 0 {
        return Rect::default();
    }
    Rect::new(
        box_rect.x + 1,
        box_rect.y,
        box_rect.width.saturating_sub(2),
        1,
    )
}

/// The smallest window xmux draws its split view in; a smaller one shows the required
/// size instead.
pub(super) const MIN_SCREEN_WIDTH: u16 = 24;
pub(super) const MIN_SCREEN_HEIGHT: u16 = 4;

/// Whether showing `nav` in `area` leaves the terminal view smaller than the smallest
/// window xmux draws in. Such a nav hides while the terminal view holds the focus, as
/// auto-hide hides it, so a small window gives the view the user is working in all of
/// its room instead of a strip too narrow for a screen's rows.
pub(crate) fn nav_crowds_terminal(area: Rect, nav: NavSize) -> bool {
    let t = compute_regions(area, nav).terminal;
    t.width < MIN_SCREEN_WIDTH || t.height < MIN_SCREEN_HEIGHT
}

pub use crate::state::Scan;

/// What the user is interested in: the one value both selection rules read
/// (Selection by Interest in docs/principles.md). A card that DISAPPEARS moves
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
    /// moves the selection or the session's host answers without it.
    Awaiting(Address),
}

/// Which part of a nav row a selection or the pointer is on. A card is one target. A
/// section title and a host card's `{machine}/{mux}` each read as two: the machine half names
/// the machine and the rest names the host, so the title of `db-01/tmux` opens the screen
/// of `db-01` from one half and of `db-01/tmux` from the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// The whole card: a session, a host's card, or a machine's card.
    Card,
    /// The machine half of a section title or of a host card.
    Machine,
    /// The host half of a section title.
    Host,
}

/// Where the selection stands: a row, the part of it, and a node deeper than any
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
    pub host: String,
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
    /// Set on an `R` re-scan - explicit, on-demand recovery for the viewed session.
    reattach_kick: bool,

    rows: Vec<Row>,
    selected: usize,
    /// The part of the selected row the selection is on.
    part: Part,
    /// A node the selection names that has no nav target of its own; the selected
    /// row and part then stand for its nearest ancestor on the list.
    deep: Option<Node>,
    /// For each node the selection stepped up from, the child it left, so a step down
    /// returns to it.
    trail: std::collections::HashMap<Node, Node>,
    /// The hover in the nav: the target under the pointer while the nav holds
    /// the focus, as a row identity and the part of it. The terminal view shows its
    /// screen; nothing else follows it.
    hover: Option<(RowRef, Part)>,
    /// The selected link on the shown machine or host screen, by index and by what
    /// it names, so a rebuild that adds or drops links keeps the same link selected. With
    /// no name the screen stands on its start link.
    link_selection: usize,
    link_selection_node: Option<crate::ui::chrome::LinkTarget>,
    /// The link under the pointer on that screen while the terminal view holds the focus.
    link_hover: Option<usize>,
    /// Host whose login pane was opened explicitly from the check table or palette.
    login_target: Option<String>,

    terminal_view_target: TerminalViewTarget,
    /// The session xmux is ITSELF running in, when it is inside one. The
    /// one session the terminal view refuses: see [`Switcher::is_own_session`].
    own_session: Option<Address>,
    /// The session card whose display client moved somewhere this nav lists no card for,
    /// and the mux's label for that place.
    away: Option<(Address, String)>,
    /// The sessions that rang their bell or sent a notification while not on screen,
    /// each marked on its card until it is shown.
    alerted: std::collections::HashSet<Address>,
    /// The stopped session the user executed, which the terminal view attaches to and so
    /// resumes. It holds while that session stays selected and stopped; a stopped session
    /// that is only selected shows its screen and attaches nothing.
    resumed: Option<Address>,
    /// The stopped session an execution just resumed, taken by the update step, which
    /// attaches it afresh: a display that ended on that session would otherwise keep its
    /// last frame.
    resume_kick: Option<Address>,
    /// Whether the current sorted list receives contiguous numbers on each rebuild.
    renumbering: bool,
    /// Card numbers keyed by identity. The configured policy either deals them in the
    /// current sorted list order or keeps each card's number until the next full scan.
    numbers: std::collections::HashMap<CardId, usize>,
    next_number: usize,
    numbers_fixed: bool,
    /// Whether the full scan still waits on its roster answer, which can add machines after
    /// every host on the list has answered. The numbers are not fixed while it does.
    numbers_held: bool,
    terminal_view: bool,
    /// Whether host cards are omitted after leaving nav from a session card.
    host_band_hidden: bool,
    /// Whether the prefix is armed. An armed prefix paints the hidden host band, so the
    /// cards a chord can reach are on screen while it is typed.
    prefix_armed: bool,

    /// The session whose card a full re-scan turned into its host card, held until the
    /// selection moves. While it holds, the scanning host card keeps that session's
    /// confirmed grid instead of its scanning screen, and only when the session is on
    /// the card's own host, so a scanning host card never shows another host's grid.
    rescan_collapse: Option<Address>,
    /// The host a create was asked on while the selection has not moved since. The
    /// created session takes the selection only while it holds, so a move the user made
    /// while the create ran is not undone when the create finishes.
    create_host: Option<String>,
    /// The transient offset and in-flight border drag of the active modal popup. Its
    /// frame geometry belongs to the render plan shared with mouse input.
    popup_geo: PopupGeometry,
    /// Whether the landing screen fills the terminal view: from launch until the user
    /// first executes a target, and never again in the run. While it is open the
    /// selection highlights and attaches nothing.
    landing: bool,
    /// Whether the selection names nothing: the node it named was lost with nothing
    /// of its machine left on the list. The selected row then only marks the place the
    /// lost card stood, which the next arrow key starts from, and the terminal view shows
    /// the landing list.
    vacant: bool,
    /// The node the selection named before it became vacant, which it returns to when
    /// the user changes the filter so that node is listed again.
    lost: Option<Node>,
    /// The card that stood where the lost card stood when the selection became vacant,
    /// by identity, so the place the next arrow key starts from survives a list change.
    vacant_place: Option<RowRef>,
    /// The filter the last rebuild applied, so a rebuild can tell a filter the user
    /// changed from an answer that arrived.
    last_filter: String,
    /// Whether the running scan came from a rescan key: only a scan the user asked
    /// for floats its advice box, so the launch probe stays silent.
    explicit_rescan: bool,
}

mod columns;
mod input;
mod mouse;
mod render;
#[cfg(test)]
pub(crate) use render::MIDDLE_ELLIPSIS;
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
            link_selection: 0,
            link_selection_node: None,
            link_hover: None,
            login_target: None,
            terminal_view_target: TerminalViewTarget::default(),
            own_session: None,
            away: None,
            alerted: std::collections::HashSet::new(),
            resumed: None,
            resume_kick: None,
            renumbering: true,
            numbers: std::collections::HashMap::new(),
            next_number: 1,
            numbers_fixed: false,
            numbers_held: false,
            terminal_view: false,
            host_band_hidden: false,
            prefix_armed: false,
            rescan_collapse: None,
            create_host: None,
            popup_geo: PopupGeometry::default(),
            landing: false,
            vacant: false,
            lost: None,
            vacant_place: None,
            last_filter: String::new(),
            explicit_rescan: false,
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

    /// Seeds the switcher from the resolved host list alone - no probing - so
    /// the first frame paints host-skeleton rows, each in a scanning state, in
    /// tens of milliseconds. Streamed [`apply_host_result`]
    /// calls fill the tree in afterward. The caller seeds `state` via
    /// [`crate::state::State::from_hosts`].
    pub fn from_hosts(state: &mut crate::state::State) -> Self {
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

    /// Opens the landing screen. The app calls this once at launch, before anything is
    /// chosen.
    pub fn open_landing(&mut self) {
        self.landing = true;
        self.on_focus_changed();
    }

    pub(crate) fn landing_open(&self) -> bool {
        self.landing
    }

    /// Whether the terminal view shows the landing list: while the landing screen is
    /// open, and while the selection names nothing and the pointer is on no card.
    fn landing_shown(&self) -> bool {
        self.landing || (self.vacant && self.hover.is_none())
    }

    /// Closes the landing screen for the rest of the run, so the selection drives the
    /// terminal view from here on. Returns whether it was open. Closing it is the first
    /// execution, so the card it executed is a choice from here on: a session card that
    /// appears later no longer takes the selection as the first session.
    pub(crate) fn close_landing(&mut self) -> bool {
        if !std::mem::take(&mut self.landing) {
            return false;
        }
        if self.interest == Interest::FirstSession {
            self.interest = Interest::Selected;
        }
        self.link_hover = None;
        self.on_focus_changed();
        true
    }

    /// Names the session xmux is running in, so the terminal view can refuse it. The app
    /// calls this once at startup; outside a mux, and where the session could not be
    /// named, it is never called and nothing is refused.
    pub fn set_own_session(&mut self, address: Option<Address>) {
        self.own_session = address;
    }

    /// Records where the display client of the session at an address went, when the nav
    /// lists no card for that place, or clears it with `None`.
    pub fn set_away(&mut self, away: Option<(Address, String)>) {
        self.away = away;
    }

    /// Marks the session at `address` as having asked for attention. Returns true when it
    /// was not marked yet.
    pub fn mark_alert(&mut self, address: Address) -> bool {
        self.alerted.insert(address)
    }

    /// Takes the mark off `host`'s session `name`, once it is on screen.
    pub fn clear_alert(&mut self, host: &str, name: &str) {
        self.alerted
            .retain(|address| address.host != host || address.session != name);
    }

    /// Whether `host`'s session `name` asked for attention since it was last shown.
    pub(crate) fn alerted(&self, host: &str, name: &str) -> bool {
        self.alerted
            .iter()
            .any(|address| address.host == host && address.session == name)
    }

    /// The label of the place the display client of `host`'s session `name` went, when
    /// that place has no card.
    fn away_of(&self, host: &str, name: &str) -> Option<&str> {
        self.away
            .as_ref()
            .filter(|(address, _)| address.host == host && address.session == name)
            .map(|(_, label)| label.as_str())
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
        // Each surface's hover lives only while that surface holds the focus.
        let hovered = self.hover.is_some() || self.link_hover.is_some();
        if terminal {
            self.hover = None;
        } else if !self.landing {
            // The landing screen is pickable from the nav's focus, so its pointer stays.
            self.link_hover = None;
        }
        let entered = terminal && !self.terminal_view;
        if entered {
            self.host_band_hidden = matches!(self.current_ref(), Some(RowRef::Session { .. }));
        } else if !terminal {
            self.host_band_hidden = false;
        }
        self.terminal_view = terminal;
        // Moving the focus into the terminal view executes the selection, which is what
        // resumes a stopped session.
        if entered {
            self.execute_stopped();
        } else if hovered {
            self.on_focus_changed();
        }
    }

    /// Records whether the prefix is armed, which paints a hidden host band.
    pub fn sync_prefix_armed(&mut self, armed: bool) {
        self.prefix_armed = armed;
    }

    /// Whether the card of `address` is a stopped session.
    fn is_stopped(&self, address: &Address) -> bool {
        self.row_of_session(address).is_some_and(
            |i| matches!(&self.rows[i].reference, RowRef::Session { sess } if sess.stopped),
        )
    }

    /// Whether `address` is a stopped session the user has not executed, which the
    /// terminal view shows as its screen and does not attach to: attaching resumes it.
    fn holds_stopped(&self, address: &Address) -> bool {
        self.is_stopped(address) && self.resumed.as_ref() != Some(address)
    }

    /// Executes the selection for a stopped session: when the selection is one, the
    /// terminal view attaches to it, and the mux's attach resumes it.
    pub(crate) fn execute_stopped(&mut self) {
        if let Some(Node::Session(address)) = self.selected_node() {
            if self.is_stopped(&address) {
                self.resumed = Some(address.clone());
                self.resume_kick = Some(address);
            }
        }
        self.on_focus_changed();
    }

    /// Takes the stopped session an execution resumed since the last call.
    pub fn take_resume_kick(&mut self) -> Option<Address> {
        self.resume_kick.take()
    }

    /// Whether the paint leaves the host band out: hidden by the move into the terminal
    /// view from a session card. An armed prefix paints the band without changing that
    /// decision, so it hides again when the chord ends, and a selection on a host card
    /// paints the band, since a selected card is always painted.
    fn band_unpainted(&self) -> bool {
        self.host_band_hidden
            && !self.prefix_armed
            && !matches!(
                self.current_ref(),
                Some(RowRef::Host { .. } | RowRef::Machine { .. })
            )
    }

    /// Whether `(host, target)` addresses the session xmux is ITSELF running in.
    ///
    /// That session has a live grid like any other, and showing it is still refused:
    /// attaching to it puts a second client on the session that holds xmux, which moves
    /// the user's own client and paints xmux inside itself. The match is on this one
    /// address, so a session running a DIFFERENT xmux mirrors like any other, showing
    /// that xmux's screen, and the refused card itself stays selectable.
    fn is_own_session(&self, host: &str, target: &str) -> bool {
        match &self.own_session {
            Some(own) => !target.is_empty() && own.host == host && own.session == target,
            None => false,
        }
    }

    /// Takes the pending rescan-kick flag that [`Switcher::request_rescan`] sets. The
    /// update step takes it when it turns the rescan command into a runtime effect.
    pub fn take_rescan_kick(&mut self) -> bool {
        std::mem::take(&mut self.rescan_kick)
    }

    /// Consumes the re-attach kick (set by an `R` re-scan): the loop tears down the
    /// current display attachment so the next attach re-creates a fresh client.
    pub fn take_reattach_kick(&mut self) -> bool {
        std::mem::take(&mut self.reattach_kick)
    }

    // --- tree model ---------------------------------------------------------

    fn rebuild(&mut self, state: &mut crate::state::State) {
        if self
            .login_target
            .as_ref()
            .is_some_and(|host| !login_answers(state, host))
        {
            self.login_target = None;
        }
        let prior = Prior {
            node: self.selected_node(),
            row: self.current_ref().cloned(),
            deep: self.deep.is_some(),
            index: self.selected,
        };

        // The deterministic display order (groups local→WSL→remote then by host name,
        // sessions by name) is applied here, once, so every mutation path lands on it and
        // a routine poll reproduces the same order exactly - there is nothing to freeze.
        // Pure row generation lives in `cards::flatten`; rebuild orchestrates order →
        // flatten → the selection resolved from the interest around it.
        for g in state.groups.iter_mut() {
            cards::sort_by_name(&mut g.sessions);
        }
        state.groups = cards::order_groups(&state.groups);
        // The mux each card NAMES comes from one resolver, so a session card, its host's
        // card and the screen behind either cannot spell one mux three ways.
        let named_mux = |host: &str| state.chrome.host_mux(host).to_string();
        let hostless = state.hostless_machines();
        let flat = |filter: &str| {
            cards::flatten(
                &state.groups,
                &state.scanning,
                &hostless,
                &state.machine_scanning,
                filter,
                &named_mux,
            )
        };
        let rows = flat(&state.filter);
        // While the numbers are dealt in list order, they are dealt over the list the
        // filter does not narrow, so a filter typed during a scan cannot renumber the cards
        // it hides.
        let unfiltered = (!self.renumbering && !self.numbers_fixed && !state.filter.is_empty())
            .then(|| flat(""));
        let settled = state.scanning.is_empty() && state.machine_scanning.is_empty();

        let old_rows = std::mem::replace(&mut self.rows, rows);
        // A pointer target belongs to the layout it was read from. Inventory can
        // replace a whole host card with a shared title or reorder its neighbours.
        if old_rows.len() != self.rows.len()
            || old_rows.iter().zip(&self.rows).any(|(before, after)| {
                std::mem::discriminant(&before.reference)
                    != std::mem::discriminant(&after.reference)
                    || !same_node(&before.reference, &after.reference)
            })
        {
            self.clear_hover(true, true);
            if let Some(
                Modal::Help { hover, .. }
                | Modal::Check { hover, .. }
                | Modal::Palette { hover, .. },
            ) = &mut state.modal
            {
                *hover = None;
            }
        }
        self.number_cards(unfiltered.as_deref(), settled);
        let refiltered = state.filter != self.last_filter;
        self.last_filter = state.filter.clone();
        let before = prior.node.clone();
        let lost = if self.vacant {
            self.lost.clone()
        } else {
            prior.node.clone()
        };
        let at = self.place_of(&prior, &old_rows);
        // A vacant selection returns to the node it lost only when the user changed the
        // filter so that node is listed again; an answer that lists it moves nothing.
        let returned = if self.vacant && refiltered {
            self.lost
                .as_ref()
                .and_then(|node| self.target_of(node, None))
                .map(|(row, part)| Target {
                    row,
                    part,
                    deep: None,
                })
        } else {
            None
        };
        let target = match returned {
            Some(target) => Some(target),
            None => self.resolve_selection(prior, state),
        };
        if self
            .hover
            .as_ref()
            .is_some_and(|(r, _)| self.row_matching(r).is_none())
        {
            self.hover = None;
        }
        match target {
            Some(target) => self.place(before, target),
            None => self.vacate(at, lost),
        }
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
            .link_selection_node
            .as_ref()
            .and_then(|target| links.iter().position(|l| l.target == *target))
        {
            Some(i) => self.link_selection = i,
            None if self.link_selection_node.is_none() => {
                // A screen nobody has stepped on yet stands on its start link, and
                // keeps it only once the link names what the start stands for, a node of
                // the level below: a screen whose children have not arrived waits for
                // its first child.
                self.link_selection = start_link(&links);
                if links
                    .get(self.link_selection)
                    .and_then(|l| l.node())
                    .is_some_and(|node| !matches!(node, Node::Machine(_)))
                {
                    self.link_selection_node =
                        links.get(self.link_selection).map(|l| l.target.clone());
                }
            }
            None => {
                self.link_selection = self.link_selection.min(links.len().saturating_sub(1));
                self.link_selection_node = links.get(self.link_selection).map(|l| l.target.clone());
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
    fn resolve_selection(&mut self, prior: Prior, state: &crate::state::State) -> Option<Target> {
        let first_selectable = || Target {
            row: self.rows.iter().position(Row::selectable).unwrap_or(0),
            part: Part::Card,
            deep: None,
        };
        match self.interest.clone() {
            Interest::FirstSession => {
                // A session answering later than the first one does not take the selection:
                // the interest is settled by the first, so the launch attaches one session
                // rather than one per answer, and the terminal view shows the session
                // the selection names throughout the scan.
                match self
                    .rows
                    .iter()
                    .position(|r| matches!(r.reference, RowRef::Session { .. }))
                {
                    Some(i) => {
                        self.interest = Interest::Selected;
                        Some(Target::card(i))
                    }
                    None => Some(first_selectable()),
                }
            }
            Interest::Awaiting(address) => {
                if let Some(i) = self.row_of_session(&address) {
                    self.interest = Interest::Selected;
                    self.rescan_collapse = None;
                    return Some(Target::card(i));
                }
                // The interest ends when the host answered without the session, or
                // failed: a session its host can no longer reach is a lost context, and
                // a later recovery must not pull the selection back down to it. A session
                // the filter hides is still in the answer, so its card appears the moment
                // the filter lets it through.
                let listed = state.groups.iter().any(|g| {
                    g.host == address.host
                        && g.err.is_none()
                        && g.sessions.iter().any(|s| s.address() == address)
                });
                if !listed && !state.scanning.contains(&address.host) {
                    self.interest = Interest::Selected;
                    self.rescan_collapse = None;
                }
                self.lineage_target(&prior, state)
            }
            Interest::Selected => self.lineage_target(&prior, state),
        }
    }

    /// Where the selection on `prior` goes on the rebuilt rows: `prior` itself while it
    /// has a target, otherwise the nearest node up its lineage that has one.
    ///
    /// - a session goes to its host (its section title, or the host's card once it
    ///   has no session to show);
    /// - a host goes to its machine (the machine's card when the machine is down, else the machine
    ///   half of the row the host stood on, else of the machine's first row);
    /// - a machine whose card gave way to the cards of its hosts (its card while it was
    ///   down, or while no host of it was known) stays selected on the machine half of
    ///   its first row, so its screen stays and lists the hosts as links;
    /// - when nothing of the machine survives, `None`: the selection names nothing, since
    ///   it never moves down or sideways.
    ///
    /// A node the selection reached with no nav target of its own (a screen link opened
    /// it) stays selected while the inventory still holds it. A node that HAD a target
    /// and lost it walks up instead, which is how a machine going down or logged out gathers
    /// the selection from its hosts and sessions onto its one card.
    fn lineage_target(&self, prior: &Prior, state: &crate::state::State) -> Option<Target> {
        let mut node = prior.node.clone()?;
        let near = prior.row.as_ref().and_then(row_host).map(str::to_owned);
        loop {
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
            node = node.parent()?;
        }
    }

    /// The row that holds the place of the card the selection stood on: the first card
    /// after it in the prior card order that survived, else the last surviving card before
    /// it. A vacant selection keeps it as the place the next arrow key starts from.
    fn place_of(&self, prior: &Prior, old_rows: &[Row]) -> usize {
        if self.vacant {
            return self
                .vacant_place
                .as_ref()
                .and_then(|r| self.row_matching(r))
                .unwrap_or(self.selected);
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
            .unwrap_or(0)
    }

    /// Leaves the selection naming nothing, at row `at`, remembering `lost`.
    fn vacate(&mut self, at: usize, lost: Option<Node>) {
        let was = self.selected_node();
        self.selected = at.min(self.rows.len().saturating_sub(1));
        self.part = Part::Card;
        self.deep = None;
        if !self.vacant {
            self.vacant_place = self.rows.get(self.selected).map(|r| r.reference.clone());
        }
        self.vacant = true;
        self.lost = lost;
        self.login_target = None;
        if was.is_some() {
            self.link_selection = 0;
            self.link_selection_node = None;
        }
        self.on_focus_changed();
    }

    /// The nav target that stands for `node`: its own card or title half, `None` when the
    /// list has none. A machine stands on its card while it is down, otherwise on the machine
    /// half of one of its rows: the row of `near` (the host the selection comes from)
    /// when that is one of its, else its first row.
    fn target_of(&self, node: &Node, near: Option<&str>) -> Option<(usize, Part)> {
        match node {
            Node::Session(address) => self.row_of_session(address).map(|i| (i, Part::Card)),
            Node::Host(host) => {
                self.rows
                    .iter()
                    .enumerate()
                    .find_map(|(i, r)| match &r.reference {
                        RowRef::Section { host: s } if s == host => Some((i, Part::Host)),
                        RowRef::Host { host: s, .. } if s == host => Some((i, Part::Card)),
                        _ => None,
                    })
            }
            Node::Machine(machine) => {
                if let Some(i) = self.rows.iter().position(
                    |r| matches!(&r.reference, RowRef::Machine { machine: m, .. } if m == machine),
                ) {
                    return Some((i, Part::Card));
                }
                let halved = |r: &Row, want: Option<&str>| match &r.reference {
                    RowRef::Section { host } | RowRef::Host { host, .. } => {
                        crate::session::machine_of(host) == machine
                            && want.is_none_or(|w| w == host)
                    }
                    _ => false,
                };
                near.and_then(|n| self.rows.iter().position(|r| halved(r, Some(n))))
                    .or_else(|| self.rows.iter().position(|r| halved(r, None)))
                    .map(|i| (i, Part::Machine))
            }
        }
    }

    /// The target of a node the nav has no target for: the node itself, standing on its
    /// nearest ancestor's target, or on the first card when no ancestor is on the list.
    fn deep_target(&self, node: Node) -> Target {
        let mut up = node.parent();
        while let Some(ancestor) = up {
            if let Some((row, part)) = self.target_of(&ancestor, node.host()) {
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
    /// (its session cards hang under it), a host's card or a machine's card. A session
    /// card hangs under its section and starts nothing.
    fn starts_run(&self, i: usize) -> bool {
        matches!(
            self.rows.get(i).map(|r| &r.reference),
            Some(RowRef::Section { .. } | RowRef::Host { .. } | RowRef::Machine { .. })
        )
    }

    /// Gives every card on the current list its number. Sorted numbering follows the
    /// visible list on every rebuild, dealing contiguous numbers. Stable numbering holds identities until a full
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
    /// releases it (`false`). A release fixes the numbers at once when every host has
    /// already answered, since the last rebuild dealt them in list order.
    pub fn hold_numbers(&mut self, held: bool, state: &crate::state::State) {
        self.numbers_held = held;
        if held {
            self.reopen_numbers();
        } else if state.scanning.is_empty() && state.machine_scanning.is_empty() {
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

    /// The number card `i` carries under the configured policy: the number the paint
    /// writes on the card and the one the jump resolves, under either policy. A section
    /// title has no number and is never a jump target.
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
    /// its `{machine}/{mux}` from, and it scrolls off the top edge with the cards it heads;
    /// the side placement pulls the list back to show a title when the card under it and
    /// the title fit on screen together.
    fn selected_section_title(&self) -> Option<usize> {
        let sel = self.selection_row()?;
        let r = self.rows.get(sel)?;
        if !matches!(r.reference, RowRef::Session { .. }) {
            return None;
        }
        self.rows[..sel]
            .iter()
            .rposition(|r| matches!(r.reference, RowRef::Section { .. }))
    }

    /// Puts the selection on card `idx` as a whole (the host half of a section
    /// title, which is no card).
    fn set_selected(&mut self, idx: usize) {
        self.set_target(Target::card(idx));
    }

    /// Puts the selection on `target`. A section title is never selected whole: its
    /// host half stands for it. The login a machine's screen was opened for ends when the
    /// selection leaves that machine, and the screen's link selection starts over when the
    /// selection names another node.
    fn set_target(&mut self, target: Target) {
        let before = self.selected_node();
        self.place(before, target);
    }

    /// Puts the selection on `target`, coming from the node `before` named. A
    /// rebuild passes the node the selection named on the rows it replaced, so a list
    /// that changed around an unchanged node keeps that node's selected link.
    fn place(&mut self, before: Option<Node>, target: Target) {
        self.vacant = false;
        self.lost = None;
        self.vacant_place = None;
        if self.rows.is_empty() {
            self.deep = target.deep;
            return;
        }
        let row = target.row.min(self.rows.len() - 1);
        let part = match (&self.rows[row].reference, target.part) {
            (RowRef::Section { .. }, Part::Card) => Part::Host,
            (RowRef::Section { .. }, part) => part,
            (RowRef::Host { .. }, Part::Machine) => Part::Machine,
            _ => Part::Card,
        };
        self.selected = row;
        self.part = part;
        self.deep = target.deep;
        let after = self.selected_node();
        let selected_machine = match &after {
            Some(Node::Machine(machine)) => Some(machine.as_str()),
            _ => None,
        };
        if self
            .login_target
            .as_deref()
            .is_some_and(|host| Some(crate::session::machine_of(host)) != selected_machine)
        {
            self.login_target = None;
        }
        if before != after {
            self.link_selection = 0;
            self.link_selection_node = None;
        }
        self.on_focus_changed();
    }

    /// The node the selection names.
    pub(crate) fn selected_node(&self) -> Option<Node> {
        if self.vacant {
            return None;
        }
        self.deep.clone().or_else(|| {
            self.rows
                .get(self.selected)
                .map(|r| node_of(&r.reference, self.part))
        })
    }

    /// The node whose screen the terminal view shows: the hover while the pointer
    /// is on a nav target, else the selection.
    pub(crate) fn shown_node(&self) -> Option<Node> {
        match &self.hover {
            Some((reference, part)) => Some(node_of(reference, *part)),
            None => self.selected_node(),
        }
    }

    /// Moves the selection to `node`: onto its nav target, or onto the nearest
    /// ancestor's target as a node the nav has no target for. A machine keeps the row the
    /// selection leaves when that row is one of its. A move from a node to its parent
    /// records the child, so a step down returns to it.
    fn select_node(&mut self, node: Node) {
        let before = self.selected_node();
        let near = self.current_ref().and_then(row_host).map(str::to_owned);
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

    /// `Ctrl+↑`: the selection walks up a level, session to host to machine.
    fn ascend(&mut self) {
        let Some(parent) = self.selected_node().and_then(|node| node.parent()) else {
            return;
        };
        self.note_user_move();
        self.select_node(parent);
    }

    /// `Ctrl+↓`: the selection walks down a level, to the child it last came up from
    /// while that is still a child, else to the first child: a machine's hosts by name, a
    /// host's sessions in card order.
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
        self.create_host = None;
    }

    fn move_selection(&mut self, delta: isize) {
        let sel = self.selectable_indices();
        if sel.is_empty() {
            return;
        }
        self.note_user_move();
        // A selection that names nothing starts on the card standing where the lost card
        // stood, whichever way the step goes.
        if self.vacant {
            let at = sel
                .iter()
                .copied()
                .find(|&i| i >= self.selected)
                .unwrap_or(sel[sel.len() - 1]);
            self.set_selected(at);
            return;
        }
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
    /// A category is a host that has sessions, the no-session group, or the disconnected group
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
    /// flatten emits a section and its sessions together, and sinks every host with
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
        if self.vacant {
            return None;
        }
        self.rows.get(self.selected).map(|r| &r.reference)
    }

    /// The row the selection is drawn on, `None` while it names nothing.
    pub(crate) fn selection_row(&self) -> Option<usize> {
        (!self.vacant).then_some(self.selected)
    }

    /// The row of the session card `host/name`.
    #[cfg(test)]
    pub(crate) fn session_row(&self, host: &str, name: &str) -> Option<usize> {
        self.row_of_session(&Address::new(host, name))
    }

    /// The card the selection is on, as an identity a later look can compare with: the
    /// same card across a rebuild that moved its row.
    #[cfg(test)]
    pub(crate) fn selected_card(&self) -> Option<RowRef> {
        self.current_ref().cloned()
    }

    /// The host the selection acts on. A session's and a host's own; for a machine, the
    /// host the row standing for it names (its card's login host, or the host of
    /// the title or card whose machine half is selected), which is what a login, a machine
    /// re-scan and a logout address. `None` for a machine with no row on the list.
    pub(crate) fn current_host(&self) -> Option<String> {
        match self.selected_node()? {
            Node::Session(address) => Some(address.host),
            Node::Host(host) => Some(host),
            Node::Machine(machine) => self
                .current_ref()
                .and_then(row_host)
                .filter(|host| crate::session::machine_of(host) == machine)
                .map(str::to_owned),
        }
    }

    pub(crate) fn current_unreachable_screen(&self, state: &crate::state::State) -> bool {
        self.current_view_screen(state) == Some(ViewScreen::Unreachable)
    }

    /// True when the selected machine's screen carries the login pane: the machine is down and
    /// a login answers it, or its pane was opened from the machine problems. A keystroke
    /// typed while the terminal view is focused then drives that pane rather than
    /// reaching a session.
    pub(crate) fn current_machine_blocked(&self) -> bool {
        let Some(Node::Machine(machine)) = self.selected_node() else {
            return false;
        };
        matches!(self.current_ref(), Some(RowRef::Machine { blocked: true, machine: m, .. }) if *m == machine)
            || self
                .login_target
                .as_deref()
                .is_some_and(|host| crate::session::machine_of(host) == machine)
    }

    /// Whether the selected machine's screen carries the login pane, which then takes the keys
    /// typed while the terminal view holds the focus. Only a machine's screen carries it: a
    /// host refused until a login states its failure and leaves the login to its machine.
    pub(crate) fn login_pane_shown(&self, state: &crate::state::State) -> bool {
        matches!(self.selected_node(), Some(Node::Machine(_)))
            && (self.current_machine_blocked()
                || self.current_view_screen(state) == Some(ViewScreen::Login))
    }

    /// Which screen the terminal view shows in place of the grid, or `None` for a session.
    /// It is the screen of the shown node: the hover's while the pointer is on a
    /// nav target, else the selection's.
    pub(crate) fn current_view_screen(&self, state: &crate::state::State) -> Option<ViewScreen> {
        if self.landing_shown() {
            return Some(ViewScreen::Landing);
        }
        self.view_screen_of(self.shown_node().as_ref(), state)
    }

    /// Which screen the terminal view shows for `node`, or `None` for a session's grid;
    /// see [`Self::current_view_screen`].
    fn view_screen_of(
        &self,
        node: Option<&Node>,
        state: &crate::state::State,
    ) -> Option<ViewScreen> {
        let displayed = (!state.displayed.host.is_empty() && !state.displayed.session.is_empty())
            .then(|| Address::new(&state.displayed.host, &state.displayed.session));
        if let Some(Node::Session(address)) = node {
            if self.holds_stopped(address) && !self.is_own_session(&address.host, &address.session)
            {
                return Some(ViewScreen::Stopped);
            }
        }
        if let Some(Node::Machine(machine)) = node {
            let login_open = self
                .login_target
                .as_deref()
                .is_some_and(|host| crate::session::machine_of(host) == machine)
                && (state.groups.iter().any(|g| {
                    crate::session::machine_of(&g.host) == machine && g.failure().is_some()
                }) || machine_failure_alone(state, machine).is_some());
            let login_reported = state
                .login
                .as_ref()
                .is_some_and(|draft| crate::session::machine_of(&draft.host) == machine)
                && state
                    .login_reports
                    .get(machine.as_str())
                    .and_then(crate::model::LoginFailure::of_login)
                    .is_some();
            if login_open || login_reported {
                return Some(ViewScreen::Login);
            }
            return Some(crate::model::choose_machine_screen(machine_failure(
                state, machine,
            )));
        }
        let selected_host = match node {
            Some(Node::Host(host)) => Some(host.as_str()),
            _ => None,
        };
        let selected_address = match node {
            Some(Node::Session(address)) => Some(address.clone()),
            _ => None,
        };
        let group =
            selected_host.and_then(|host| state.groups.iter().find(|group| group.host == host));
        let scanning = match node {
            Some(Node::Host(host)) => state.scanning.contains(host),
            None => state.scanning_any(),
            _ => false,
        };
        crate::model::choose_view_screen(
            selected_host,
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
                        && selected_host == Some(address.host.as_str())
                        && matches!(self.current_ref(), Some(RowRef::Host { .. })),
                }),
        )
    }

    /// The node a view screen of `kind` is about, and the address it is reached by: the
    /// session for the self-session state, a host with an empty session half for a
    /// host's states, and for a machine the host its login and probes address. `None`
    /// before anything is selected.
    pub(crate) fn view_subject(&self, kind: ViewScreen) -> Option<(Node, Address)> {
        let node = self.shown_node()?;
        let address = match (&node, kind) {
            (Node::Session(address), _) => address.clone(),
            (Node::Host(host), _) => Address::new(host, ""),
            (Node::Machine(machine), _) => {
                let host = match &self.hover {
                    Some((reference, _)) => row_host(reference).map(str::to_owned),
                    None => self.current_host(),
                };
                Address::new(host.unwrap_or_else(|| machine.clone()), "")
            }
        };
        Some((node, address))
    }

    /// The links the screen of `node` offers, in the order the arrow keys walk them: the
    /// level below, then the screen's actions, then the level above. A machine's screen
    /// lists its hosts by name, each with its session count or state; a host's screen its
    /// sessions in card order, and last its machine, which its headline carries. A
    /// session's grid offers none.
    pub(crate) fn screen_links(
        &self,
        node: &Node,
        state: &crate::state::State,
    ) -> Vec<crate::ui::chrome::ScreenLink> {
        use crate::ui::chrome::{LinkTarget, ScreenLink};
        let machine = match node {
            Node::Machine(machine) => machine.as_str(),
            Node::Host(host) => crate::session::machine_of(host),
            Node::Session(_) => return Vec::new(),
        };
        let ssh = state
            .chrome
            .host_reach
            .iter()
            .any(|(host, reach)| crate::session::machine_of(host) == machine && reach.ssh);
        let host_details = match node {
            Node::Host(host) => state.host_details.contains(host),
            _ => self
                .current_host()
                .is_some_and(|host| state.host_details.contains(&host)),
        };
        let actions = self
            .view_screen_of(Some(node), state)
            .map(|kind| crate::model::screen_actions(kind, ssh))
            .unwrap_or_default()
            .into_iter()
            .map(|action| ScreenLink {
                target: LinkTarget::Action(action),
                label: action.words(host_details).to_string(),
                value: String::new(),
                number: None,
            });
        let mut links: Vec<ScreenLink> = match node {
            Node::Machine(machine) => state
                .groups
                .iter()
                .filter(|g| crate::session::machine_of(&g.host) == machine)
                .filter_map(|g| {
                    // A host is named by its mux, and only by a mux an answer
                    // confirmed, so a host whose mux no answer has confirmed yet is not
                    // linked.
                    let answered = g.err.is_none() && !state.scanning.contains(&g.host);
                    let mux = state.chrome.host_mux(&g.host);
                    if mux.is_empty() || !crate::session::mux_may_be_named(&g.host, answered) {
                        return None;
                    }
                    let label = mux.to_string();
                    let value = if state.scanning.contains(&g.host) {
                        format!(
                            "{} scanning",
                            crate::ui::spinner_glyph(state.chrome.spinner_frame)
                        )
                    } else if let Some(kind) = g.failure() {
                        cards::failure_word(kind, g.logged_out()).to_string()
                    } else {
                        match g.sessions.len() {
                            0 => cards::host_state_word(false, false, false, false).to_string(),
                            1 => "1 session".to_string(),
                            n => format!("{n} sessions"),
                        }
                    };
                    Some(ScreenLink {
                        target: LinkTarget::Node(Node::Host(g.host.clone())),
                        label,
                        value,
                        number: None,
                    })
                })
                .collect(),
            Node::Host(host) => state
                .groups
                .iter()
                .filter(|g| g.host == *host && g.err.is_none())
                .flat_map(|g| &g.sessions)
                .map(|sess| ScreenLink {
                    target: LinkTarget::Node(Node::Session(sess.address())),
                    label: sess.name.clone(),
                    value: session_facts(sess, state),
                    number: None,
                })
                .collect(),
            Node::Session(_) => Vec::new(),
        };
        links.extend(actions);
        if matches!(node, Node::Host(_)) {
            links.push(ScreenLink {
                target: LinkTarget::Node(Node::Machine(machine.to_string())),
                label: machine.to_string(),
                value: String::new(),
                number: None,
            });
        }
        links
    }

    /// The landing screen's links: every card of the nav, in its order and under its
    /// number, each written as its path in the hierarchy.
    pub(crate) fn landing_links(
        &self,
        state: &crate::state::State,
    ) -> Vec<crate::ui::chrome::ScreenLink> {
        use crate::ui::chrome::ScreenLink;
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selectable())
            .map(|(i, row)| {
                let label = card_path(row);
                let value = match &row.reference {
                    RowRef::Session { sess } => session_facts(sess, state),
                    reference => cards::card_state_word(reference)
                        .unwrap_or_default()
                        .to_string(),
                };
                ScreenLink {
                    target: crate::ui::chrome::LinkTarget::Node(node_of(
                        &row.reference,
                        Part::Card,
                    )),
                    label,
                    value,
                    number: Some(self.card_number(i)),
                }
            })
            .collect()
    }

    /// What the screen of `kind` paints: the address it is reached by, whether it is a
    /// machine's, its links, and which link is selected and which is under the pointer.
    /// `None` when the screen is about no node.
    pub(crate) fn screen_parts(
        &self,
        kind: ViewScreen,
        state: &crate::state::State,
    ) -> Option<ScreenParts> {
        if kind == ViewScreen::Landing {
            // The landing list and the nav share the one selection, so the link it
            // marks is the card the nav marks, whichever view holds the focus.
            let links = self.landing_links(state);
            let selected = self.selected_node();
            let link = links.iter().position(|l| l.node() == selected.as_ref());
            return Some(ScreenParts {
                address: Address::new("", ""),
                machine_screen: false,
                links,
                selection_and_hover: (link, self.link_hover),
            });
        }
        let (node, address) = self
            .view_subject(kind)
            .filter(|(_, address)| !address.host.is_empty())?;
        Some(ScreenParts {
            address,
            machine_screen: matches!(node, Node::Machine(_)),
            links: self.screen_links(&node, state),
            selection_and_hover: self.link_selection_and_hover(state),
        })
    }

    /// The links of the shown screen, empty while it is a session's grid.
    pub(crate) fn shown_links(
        &self,
        state: &crate::state::State,
    ) -> Vec<crate::ui::chrome::ScreenLink> {
        if self.landing_shown() {
            return self.landing_links(state);
        }
        match self.shown_node() {
            Some(node) if self.current_view_screen(state).is_some() => {
                self.screen_links(&node, state)
            }
            _ => Vec::new(),
        }
    }

    /// Which link of the shown screen is selected and which is under the pointer.
    /// Both belong to the terminal view, so neither is drawn while the nav holds the focus
    /// or while the nav's hover is showing another screen there. The login pane
    /// owns the keyboard, so on its screen no link holds the selection and only the
    /// pointer reaches the links.
    pub(crate) fn link_selection_and_hover(
        &self,
        state: &crate::state::State,
    ) -> (Option<usize>, Option<usize>) {
        if !self.terminal_view || self.hover.is_some() {
            return (None, None);
        }
        if self.login_pane_shown(state) {
            return (None, self.link_hover);
        }
        (
            Some(self.selection_link_index(&self.shown_links(state))),
            self.link_hover,
        )
    }

    /// Where the selection stands among `links`: the start link while nobody has
    /// stepped on the screen, else the selected link, within the links there are.
    fn selection_link_index(&self, links: &[crate::ui::chrome::ScreenLink]) -> usize {
        match self.link_selection_node {
            None => start_link(links),
            Some(_) => self.link_selection.min(links.len().saturating_sub(1)),
        }
    }

    /// Where the hover targets stand, as the paint draws them: the nav row and part
    /// under the pointer, and the screen link under it.
    pub(crate) fn hover_targets(&self) -> (Option<(usize, Part)>, Option<usize>) {
        let nav = self
            .hover
            .as_ref()
            .and_then(|(reference, part)| self.row_matching(reference).map(|i| (i, *part)));
        (nav, self.link_hover)
    }

    /// Ends pointer targets whose geometry or inventory is no longer current.
    pub(crate) fn clear_hover(&mut self, nav: bool, links: bool) {
        if nav && self.hover.take().is_some() {
            self.on_focus_changed();
        }
        if links {
            self.link_hover = None;
        }
    }

    /// The arrow keys on a machine's or a host's screen while the terminal view holds the
    /// focus: they walk its links and cycle, a step past the last link returning to the
    /// first and a step before the first to the last.
    pub(crate) fn step_link(&mut self, delta: isize, state: &crate::state::State) {
        let links = self.shown_links(state);
        let n = links.len();
        if n == 0 || self.login_pane_shown(state) {
            return;
        }
        self.link_selection =
            (self.selection_link_index(&links) as isize + delta).rem_euclid(n as isize) as usize;
        self.link_selection_node = links.get(self.link_selection).map(|l| l.target.clone());
    }

    /// The action of link `index` of the shown screen, or of its selected link when
    /// `index` is `None`, which the update transition runs by the action's key. `None`
    /// when that link opens a node, or when Enter belongs to the login pane's form or to
    /// a stopped session's screen.
    pub(crate) fn link_action(
        &self,
        index: Option<usize>,
        state: &crate::state::State,
    ) -> Option<crate::model::ScreenAction> {
        let links = self.shown_links(state);
        let index = match index {
            Some(i) => i,
            None if self.login_pane_shown(state)
                || self.current_view_screen(state) == Some(ViewScreen::Stopped) =>
            {
                return None;
            }
            None => self.selection_link_index(&links),
        };
        match links.get(index)?.target {
            crate::ui::chrome::LinkTarget::Action(action) => Some(action),
            crate::ui::chrome::LinkTarget::Node(_) => None,
        }
    }

    /// Executes link `index` of the shown screen: the node it names becomes the
    /// selection and its screen opens. After a step up the path, the link standing for the
    /// node just left is selected on the new screen, so a step back down is one Enter
    /// away; a step down starts the new screen on its start link.
    pub(crate) fn open_link(&mut self, index: usize, state: &crate::state::State) -> bool {
        let Some(node) = self
            .shown_links(state)
            .into_iter()
            .nth(index)
            .and_then(|l| l.node().cloned())
        else {
            return false;
        };
        let before = self.selected_node();
        self.note_user_move();
        self.link_hover = None;
        self.select_node(node);
        // A landing link is the first execution: the screen it opens replaces the landing.
        self.close_landing();
        if let Some(before) = before {
            if let Some(node) = self
                .selected_node()
                .filter(|node| is_step_up(&before, node))
            {
                if let Some(i) = self
                    .screen_links(&node, state)
                    .iter()
                    .position(|l| l.node() == Some(&before))
                {
                    self.link_selection = i;
                    self.link_selection_node = Some(crate::ui::chrome::LinkTarget::Node(before));
                }
            }
        }
        true
    }

    /// Opens the selected link of the shown screen (Enter in the terminal view).
    /// The login pane's screen has none: Enter there belongs to the form. A stopped
    /// session's screen has none either, and Enter there executes the session, as it does
    /// from the nav.
    pub(crate) fn open_selected_link(&mut self, state: &crate::state::State) -> bool {
        if self.login_pane_shown(state) {
            return false;
        }
        if self.current_view_screen(state) == Some(ViewScreen::Stopped) {
            self.execute_stopped();
            return true;
        }
        let index = self.selection_link_index(&self.shown_links(state));
        self.open_link(index, state)
    }

    // --- preview ------------------------------------------------------------

    fn on_focus_changed(&mut self) {
        // An execution resumes a stopped session once: the hold ends when the selection
        // moves or the session runs, so a session that stops again is not resumed again
        // without the user asking.
        let resumed = self.resumed.take().filter(|address| {
            self.selected_node() == Some(Node::Session(address.clone())) && self.is_stopped(address)
        });
        self.resumed = resumed;
        // The shown node's session, never xmux's OWN session. Emptying the target here is
        // what makes the refusal total: the target is the one value the display reconcile,
        // the attach, and the mux-side switch all read, so none of them can reach this
        // session by another path. The landing screen empties it the same way, which is
        // why a selection made on it attaches nothing, and so does a stopped session the
        // user has not executed, since attaching resumes it.
        self.terminal_view_target = match self.shown_node() {
            Some(Node::Session(address))
                if !self.landing
                    && !self.is_own_session(&address.host, &address.session)
                    && !self.holds_stopped(&address) =>
            {
                TerminalViewTarget {
                    host: address.host,
                    target: address.session,
                }
            }
            _ => TerminalViewTarget::default(),
        };
    }

    /// The session the shown node attaches to, `None` for a machine or a host.
    pub fn current_attach_target(
        &self,
        _state: &crate::state::State,
    ) -> Option<TerminalViewTarget> {
        let target = self.terminal_view_target.clone();
        (!target.target.is_empty()).then_some(target)
    }

    /// Moves the tree selection to the session row whose address (`host/session`)
    /// is `address`. The semantic target of `Action::Switch` - addresses a row by
    /// identity, not a screen position or a relative step, so an agent driving ctl
    /// lands on the right session regardless of how the tree is currently ordered.
    /// A no-op (returns false) when no such row exists or the selection is already there.
    ///
    /// The one mover for a selection xmux is TOLD to make, whoever asked: a ctl `switch`,
    /// or the nav following the session the mux moved its own display client onto. Both
    /// name a card and move to it, and nothing downstream tells them apart, so they share
    /// one entry point. Neither waits for a card that is not on the list yet. A create
    /// lands on its new card through the awaited interest (`Interest::Awaiting`) instead.
    pub fn select_address(&mut self, address: &Address) -> bool {
        // A selection xmux is told to make is an execution: it ends the landing screen,
        // even when the selection already stands on that card.
        if self.row_of_session(address).is_some() {
            self.close_landing();
        }
        let moved = match self.row_of_session(address) {
            Some(i) if self.selected_node() != Some(Node::Session(address.clone())) => {
                self.note_user_move();
                self.set_selected(i);
                true
            }
            _ => false,
        };
        if self.row_of_session(address).is_some() {
            self.execute_stopped();
        }
        moved
    }

    // --- refresh ------------------------------------------------------------

    /// Resets every host to its scanning skeleton and signals the event loop to
    /// re-kick the streaming probes (the `R` re-scan) - sessions and panes stream
    /// back in exactly as on first launch. The selection does not drift: the session
    /// under it becomes the awaited [`Interest`], so the selection rests on that
    /// session's host card through the skeleton phase (the lineage of a vanished
    /// session) and returns to the session the instant its host re-streams it.
    pub fn request_rescan(&mut self, state: &mut crate::state::State) {
        self.explicit_rescan = true;
        let selected = match self.selected_node() {
            Some(Node::Session(address)) => Some(address),
            _ => None,
        };
        self.rescan_collapse = selected.clone();
        if let Some(address) = selected {
            self.interest = Interest::Awaiting(address);
        }
        self.reopen_numbers();
        state.scanning = state.groups.iter().map(|g| g.host.clone()).collect();
        for g in state.groups.iter_mut() {
            g.err = None;
            g.sessions.clear();
        }
        let hostless: Vec<String> = state
            .hostless_machines()
            .into_iter()
            .map(|m| m.name.clone())
            .collect();
        for machine in hostless {
            state.machine_scanning.insert(machine.clone());
            state.machine_scan_deadlines.remove(&machine);
            if let Some(m) = state.machine_mut(&machine) {
                m.err = None;
            }
        }
        self.rescan_kick = true;
        self.reattach_kick = true;
        self.rebuild(state);
    }

    /// Streams in one host's `list-sessions` outcome: clears its scanning
    /// state and replaces that host's sessions (reachable) or records its failure
    /// (unreachable). The host authoritatively owns its session list. Ordering is
    /// not this function's concern: `rebuild` applies the deterministic display
    /// order, which a scan result and a routine poll reproduce exactly.
    ///
    /// A result that RENAMED sessions ([`cards::renamed_sessions`]) carries the selection
    /// and the displayed record across to each new name, so the card the user is on stays
    /// the card they are on and nothing reads the rename as a move to another session.
    /// The renames are returned so the loop can carry its own display record across too.
    pub fn apply_host_result(
        &mut self,
        host: String,
        sessions: Vec<Session>,
        err: Option<String>,
        state: &mut crate::state::State,
    ) -> Vec<(String, String)> {
        let renamed = state
            .groups
            .iter()
            .find(|g| g.host == host)
            .filter(|_| err.is_none())
            .map(|g| cards::renamed_sessions(&g.sessions, &sessions))
            .unwrap_or_default();
        for (from, to) in &renamed {
            // The card is the same card under its new name, so it keeps its number.
            let old = CardId::Session(host.clone(), from.clone());
            if let Some(n) = self.numbers.remove(&old) {
                self.numbers
                    .insert(CardId::Session(host.clone(), to.clone()), n);
            }
            for row in self.rows.iter_mut() {
                if let RowRef::Session { sess } = &mut row.reference {
                    if sess.host == host && sess.name == *from {
                        sess.name = to.clone();
                    }
                }
            }
            for sel in [&mut state.selection, &mut state.displayed] {
                if sel.host == host && sel.session == *from {
                    sel.session = to.clone();
                }
            }
        }
        state.scanning.remove(&host);
        state.scan_deadlines.remove(&host);
        if !state.scanning_any() {
            self.explicit_rescan = false;
        }
        // The failure run, counted where every result lands so no path can skip it: a
        // result that failed lengthens it, one that answered clears it, and a logout, which
        // is no failure, ends it. It is shown, not acted on - see `State::failure_runs`.
        match &err {
            Some(e) if e == crate::model::LOGGED_OUT => {
                state.failure_runs.remove(&host);
            }
            Some(_) => *state.failure_runs.entry(host.clone()).or_insert(0) += 1,
            None => {
                state.failure_runs.remove(&host);
                state
                    .last_reached
                    .insert(host.clone(), std::time::SystemTime::now());
            }
        }
        // A mux that answers describes the machine in place of the login steps that
        // settled before it.
        if err.is_none() {
            state.drop_settled_login(crate::session::machine_of(&host));
        }
        let existing = state.groups.iter().position(|g| g.host == host);
        match existing {
            Some(i) => {
                state.groups[i].err = err;
                state.groups[i].sessions = sessions;
            }
            None => state.groups.push(Group {
                host,
                err,
                sessions,
            }),
        }
        self.rebuild(state);
        renamed
    }

    /// Adds a host that was not there at launch (a mux discovery answered) as a
    /// SCANNING host card, so it appears the moment it is found instead of at the next
    /// run. Idempotent: a host already in the nav is left exactly as it is.
    ///
    /// It APPENDS the new host to `state.groups`; `rebuild` then places it in the
    /// deterministic order.
    pub fn add_host(&mut self, host: String, state: &mut crate::state::State) {
        self.add_hosts(vec![host], state);
    }

    /// Adds every host of `hosts` the nav does not show yet, then rebuilds once, so
    /// the card a machine stood on before any host of it was known gives way to all of
    /// them at once while the selection stays on the machine. The host named by the
    /// machine alone keeps that card's number.
    pub fn add_hosts(&mut self, hosts: Vec<String>, state: &mut crate::state::State) {
        let mut added = false;
        for host in hosts {
            if state.groups.iter().any(|g| g.host == host) {
                continue;
            }
            let machine = crate::session::machine_of(&host).to_string();
            if !state.has_hosts(&machine) {
                if let Some(n) = self.numbers.remove(&CardId::Machine(machine.clone())) {
                    if host == machine {
                        self.numbers.insert(CardId::Host(host.clone()), n);
                    }
                }
                state.machine_scanning.remove(&machine);
                state.machine_scan_deadlines.remove(&machine);
            }
            if state.machine(&machine).is_none() {
                state.machines.push(crate::model::Machine::new(machine));
            }
            state.scanning.insert(host.clone());
            state.scan_deadlines.remove(&host);
            state.groups.push(Group {
                host,
                err: None,
                sessions: Vec::new(),
            });
            added = true;
        }
        if added {
            self.rebuild(state);
        }
    }

    /// Puts the card of `host` back in flight: it spins and carries no failure, for an
    /// answer that is on its way. Idempotent, and a host the nav does not show is left
    /// alone.
    pub fn mark_scanning(&mut self, host: &str, state: &mut crate::state::State) {
        let Some(g) = state.groups.iter_mut().find(|g| g.host == host) else {
            return;
        };
        if g.err.is_none() && state.scanning.contains(host) {
            return;
        }
        g.err = None;
        state.scanning.insert(host.to_string());
        state.scan_deadlines.remove(host);
        self.rebuild(state);
    }

    /// Puts every host `machine` serves in flight for a re-scan of that machine alone.
    /// Each card spins or keeps the sessions it lists until its answer lands, so the
    /// list and its numbers hold still while the machine is asked again.
    pub fn mark_machine_scanning(&mut self, machine: &str, state: &mut crate::state::State) {
        for g in state.groups.iter_mut() {
            if crate::session::machine_of(&g.host) == machine {
                g.err = None;
                state.scanning.insert(g.host.clone());
                state.scan_deadlines.remove(&g.host);
            }
        }
        if !state.has_hosts(machine) {
            if let Some(m) = state.machine_mut(machine).filter(|m| !m.muxless) {
                m.err = None;
                state.machine_scanning.insert(machine.to_string());
                state.machine_scan_deadlines.remove(machine);
            }
        }
        self.rebuild(state);
    }

    /// Puts `machine` on the roster, its card spinning while no host of it is known.
    /// Idempotent.
    pub fn add_machine(&mut self, machine: String, state: &mut crate::state::State) {
        state.add_machine(machine);
        self.rebuild(state);
    }

    /// Drops a machine the roster no longer names. Its hosts leave through
    /// [`Switcher::remove_host`]. Idempotent.
    pub fn remove_machine(&mut self, machine: &str, state: &mut crate::state::State) {
        if state.machine(machine).is_none() {
            return;
        }
        state.machines.retain(|m| m.name != machine);
        state.machine_scanning.remove(machine);
        self.rebuild(state);
    }

    /// Records the answer `machine` gave as a whole: `Some` why it could not be asked,
    /// `None` that it answered. The card of a machine with no host known states it, and
    /// its answer is no longer on its way.
    pub fn apply_machine_result(
        &mut self,
        machine: &str,
        err: Option<String>,
        state: &mut crate::state::State,
    ) {
        let Some(m) = state.machine_mut(machine) else {
            return;
        };
        if err.is_some() {
            m.muxless = false;
        }
        m.err = err.clone();
        state.machine_scanning.remove(machine);
        state.machine_scan_deadlines.remove(machine);
        if !state.scanning_any() {
            self.explicit_rescan = false;
        }
        // The failure run, counted under the machine's name the way a host counts its
        // own and ended by a logout.
        match &err {
            Some(reason) => {
                if reason == crate::model::LOGGED_OUT {
                    state.failure_runs.remove(machine);
                } else {
                    *state.failure_runs.entry(machine.to_string()).or_insert(0) += 1;
                }
            }
            None => {
                state.failure_runs.remove(machine);
            }
        }
        self.rebuild(state);
    }

    /// Settles `machine` as serving no mux xmux supports: with no host known, it has
    /// nothing to show, so its card goes.
    pub fn settle_muxless(&mut self, machine: &str, state: &mut crate::state::State) {
        let Some(m) = state.machine_mut(machine) else {
            return;
        };
        m.muxless = true;
        m.err = None;
        state.machine_scanning.remove(machine);
        state.machine_scan_deadlines.remove(machine);
        self.rebuild(state);
    }

    /// Drops a host whose MACHINE the roster no longer names, and everything the nav
    /// held for it, the machine with its last host. Idempotent: a host the nav does
    /// not show is left alone.
    ///
    /// A selection on the dropped card moves along its lineage, as on every rebuild.
    pub fn remove_host(&mut self, host: &str, state: &mut crate::state::State) {
        if !state.groups.iter().any(|g| g.host == host) {
            return;
        }
        state.groups.retain(|g| g.host != host);
        let machine = crate::session::machine_of(host);
        if !state.has_hosts(machine) {
            state.machines.retain(|m| m.name != machine);
            state.machine_scanning.remove(machine);
            state.machine_scan_deadlines.remove(machine);
        }
        state.scanning.remove(host);
        state.scan_deadlines.remove(host);
        state.failure_runs.remove(host);
        state.last_reached.remove(host);
        state.live_hosts.remove(host);
        state.host_details.remove(host);
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

/// One row of the table of machine problems: a host in a problem state, the cause, the
/// reason its last answer gave, and whether the hiding leaves it without a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckEntry {
    pub(crate) host: String,
    /// The host as its card names it.
    pub(crate) label: String,
    pub(crate) kind: crate::model::FailureKind,
    pub(crate) reason: String,
}

/// The card a number is kept for: a session by its address, a host's card by its
/// host, a machine's card by its machine. A session that ends and later returns under the
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
        RowRef::Session { sess } => Some(CardId::Session(sess.host.clone(), sess.name.clone())),
        RowRef::Host { host, .. } => Some(CardId::Host(host.clone())),
        RowRef::Machine { machine, .. } => Some(CardId::Machine(machine.clone())),
        RowRef::Section { .. } => None,
    }
}

/// The context parts of a row: `(machine, mux, session)`. A host-state card and a
/// section title carry only their machine; a session card names its session's machine, mux
/// kind (empty when not yet known), and session name.
fn context_of(row: &Row) -> (&str, &str, &str) {
    // The MACHINE half, never the whole host id: a host id already carries the mux
    // when its machine serves several, and the card renders the mux as its own span, so
    // returning the id whole would read `local:zellij/zellij`. The mux comes off the row
    // itself, resolved once when the row was built, so every row on one host names its
    // mux the same way whatever each of them had to read it from.
    match &row.reference {
        RowRef::Host { host, .. } | RowRef::Section { host, .. } => {
            (crate::session::machine_of(host), &row.mux, "")
        }
        RowRef::Machine { machine, .. } => (machine, "", ""),
        RowRef::Session { sess } => (crate::session::machine_of(&sess.host), &row.mux, &sess.name),
    }
}

/// A card written as its path in the hierarchy: a session card as
/// `{machine}/{mux}/{session}`, a host card as `{machine}/{mux}`, and a machine card, or a
/// host card whose mux no answer confirmed, as its machine alone.
/// The link a screen starts on: a host screen's first session, or its machine link
/// while it has none; any other screen's first link.
fn start_link(links: &[crate::ui::chrome::ScreenLink]) -> usize {
    let up = links
        .iter()
        .position(|l| matches!(l.node(), Some(Node::Machine(_))));
    match (links.first().and_then(|l| l.node()), up) {
        (Some(Node::Session(_)), _) | (_, None) => 0,
        (_, Some(up)) => up,
    }
}

/// Whether going from `from` to `to` climbs one level of the path: a session to its
/// host, or a host to its machine.
fn is_step_up(from: &Node, to: &Node) -> bool {
    matches!(
        (from, to),
        (Node::Session(_), Node::Host(_)) | (Node::Host(_), Node::Machine(_))
    )
}

fn card_path(row: &Row) -> String {
    let (machine, mux, session) = context_of(row);
    [machine, mux, session]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// The category reached by a horizontal step. Vertical steps visit every card.
#[derive(PartialEq, Eq)]
enum NavCategory {
    Host(String),
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
        RowRef::Section { host, .. } => NavCategory::Host(host.clone()),
        RowRef::Session { sess } => NavCategory::Host(sess.host.clone()),
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

/// The host a row stands on: a session's, a title's or a host card's own, and the
/// host a machine's card logs in through.
fn row_host(reference: &RowRef) -> Option<&str> {
    match reference {
        RowRef::Host { host, .. } | RowRef::Section { host } | RowRef::Machine { host, .. } => {
            Some(host)
        }
        RowRef::Session { sess } => Some(&sess.host),
    }
}

/// Whether two row references name the same row across a rebuild: a session by its
/// address, a host by its id whether it shows as its section title or its host-state
/// card, a machine's card by its machine. The selection holds on that identity, so a host
/// gaining or losing its sessions keeps it.
fn same_node(a: &RowRef, b: &RowRef) -> bool {
    match (a, b) {
        (RowRef::Session { .. }, RowRef::Session { .. }) => {
            session_addr_of(a) == session_addr_of(b)
        }
        (RowRef::Machine { machine: x, .. }, RowRef::Machine { machine: y, .. }) => x == y,
        (
            RowRef::Section { host: x } | RowRef::Host { host: x, .. },
            RowRef::Section { host: y } | RowRef::Host { host: y, .. },
        ) => x == y,
        _ => false,
    }
}

/// The node `part` of a row names: the machine half of a title or a host card names the
/// machine, a title's other half and a host card the host, a machine's card the machine, and a
/// session card the session.
fn node_of(reference: &RowRef, part: Part) -> Node {
    match reference {
        RowRef::Session { sess } => Node::Session(sess.address()),
        RowRef::Section { host } | RowRef::Host { host, .. } if part == Part::Machine => {
            Node::Machine(crate::session::machine_of(host).to_string())
        }
        RowRef::Section { host } | RowRef::Host { host, .. } => Node::Host(host.clone()),
        RowRef::Machine { machine, .. } => Node::Machine(machine.clone()),
    }
}

/// Whether the inventory still holds `node`: a machine while any host of it is listed, a
/// host while it is listed, a session while its host lists it.
fn node_exists(node: &Node, state: &crate::state::State) -> bool {
    match node {
        Node::Machine(machine) => {
            state.has_hosts(machine) || state.hostless_machines().iter().any(|m| m.name == *machine)
        }
        Node::Host(host) => state.groups.iter().any(|g| g.host == *host),
        Node::Session(address) => state.groups.iter().any(|g| {
            g.host == address.host
                && g.err.is_none()
                && g.sessions.iter().any(|s| s.name == address.session)
        }),
    }
}

/// The nodes one level below `node`, in the order a step down picks from: a machine's
/// hosts by name, a host's sessions in card order.
fn node_children(node: &Node, state: &crate::state::State) -> Vec<Node> {
    match node {
        Node::Machine(machine) => {
            let mut hosts: Vec<&str> = state
                .groups
                .iter()
                .map(|g| g.host.as_str())
                .filter(|s| crate::session::machine_of(s) == machine)
                .collect();
            hosts.sort_unstable();
            hosts
                .into_iter()
                .map(|s| Node::Host(s.to_string()))
                .collect()
        }
        Node::Host(host) => state
            .groups
            .iter()
            .filter(|g| g.host == *host && g.err.is_none())
            .flat_map(|g| g.sessions.iter().map(|s| Node::Session(s.address())))
            .collect(),
        Node::Session(_) => Vec::new(),
    }
}

/// The failure a machine as a whole is in: the failure every one of its hosts shares, a
/// login one when any of them needs a login. `None` while any host answered or is still
/// waiting on its answer, since a machine is down only when none of its hosts connected.
pub(crate) fn machine_failure(
    state: &crate::state::State,
    machine: &str,
) -> Option<crate::model::FailureKind> {
    use crate::model::FailureKind;
    if !state.has_hosts(machine) {
        return machine_failure_alone(state, machine);
    }
    let mut blocked = false;
    let mut any = false;
    for g in state
        .groups
        .iter()
        .filter(|g| crate::session::machine_of(&g.host) == machine)
    {
        any = true;
        match g.failure() {
            _ if state.scanning.contains(&g.host) => return None,
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

/// The failure a machine with no host known is in, read off its own answer, and `None`
/// while that answer is on its way or for a machine with a host.
pub(crate) fn machine_failure_alone(
    state: &crate::state::State,
    machine: &str,
) -> Option<crate::model::FailureKind> {
    if state.has_hosts(machine) || state.machine_scanning.contains(machine) {
        return None;
    }
    state.machine(machine)?.failure()
}

/// Whether the login pane opened for `host` still has a failure to answer: a failed
/// host of that address, or a machine of that name that failed with no host known.
fn login_answers(state: &crate::state::State, host: &str) -> bool {
    state
        .groups
        .iter()
        .any(|group| group.host == host && group.failure().is_some())
        || machine_failure_alone(state, host).is_some()
}

/// Whether every host of a machine is still waiting on its answer, or, for a machine with
/// no host known, whether its own answer is.
pub(crate) fn is_machine_scanning(state: &crate::state::State, machine: &str) -> bool {
    if !state.has_hosts(machine) {
        return state.machine_scanning.contains(machine);
    }
    let mut hosts = state
        .groups
        .iter()
        .filter(|g| crate::session::machine_of(&g.host) == machine)
        .peekable();
    hosts.peek().is_some() && hosts.all(|g| state.scanning.contains(&g.host))
}

/// What a session's link and hint say about it: its windows and whether a client other
/// than xmux's own is on it.
fn session_facts(sess: &Session, state: &crate::state::State) -> String {
    let mut facts = Vec::new();
    if sess.windows > 0 {
        let s = if sess.windows == 1 { "" } else { "s" };
        facts.push(format!("{} window{s}", sess.windows));
    }
    if state.attached_by_others(sess) {
        facts.push("attached".to_string());
    }
    if sess.stopped {
        facts.push(crate::session::STOPPED.to_string());
    }
    facts.join(", ")
}

/// What a view screen paints besides its kind; see [`Switcher::screen_parts`].
pub(crate) struct ScreenParts {
    pub(crate) address: Address,
    pub(crate) machine_screen: bool,
    pub(crate) links: Vec<crate::ui::chrome::ScreenLink>,
    pub(crate) selection_and_hover: (Option<usize>, Option<usize>),
}

/// The selection as a rebuild found it, before the rows are re-derived.
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
mod tests_selection;

#[cfg(test)]
pub(crate) mod tests_support;
