//! Rendering and layout for the switcher's chrome: the tree|terminal view border,
//! hint bar, and host screens that fill the terminal-view region in place of a mux.
//! [`State`](crate::state::State) owns the [`Chrome`] data this module paints.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

#[cfg(test)]
use crate::state::chrome::FLASH_TTL;
use crate::state::Chrome;
pub use crate::state::{HostReach, ViewBorderColors};
use crate::ui::modal::wrap_text;
use crate::ui::switcher::fit;

/// Parses a tmux-style colour token into a ratatui [`Color`], matching tmux/psmux's
/// colour slots so the view border colours can be configured exactly like
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
    if let Some(idx) = lower
        .strip_prefix("colour")
        .or_else(|| lower.strip_prefix("color"))
    {
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

/// The tree|terminal view border's three colours: `active` marks nav focus,
/// `inactive` marks terminal focus, and `hover` is the drag-resize grab cue.
///
/// The defaults are xmux's own and the same on every host: the palette's `primary`
/// for nav focus, `disabled` for terminal focus, and `accent` for the grab cue. The
/// border says which VIEW holds focus, which is a fact about xmux and not about the mux
/// on the other side of it, so a border that changed hue as the selection moved between
/// hosts was reading as a state change where there was none.
///
/// [`Self::resolve`] layers one tier over that: a `[ui] view-*-border-style` value the
/// user named. Their terminal, their choice.
impl Default for ViewBorderColors {
    fn default() -> Self {
        Self::from_palette(&crate::ui::palette::Palette::default())
    }
}

impl ViewBorderColors {
    fn from_palette(pal: &crate::ui::palette::Palette) -> Self {
        ViewBorderColors {
            active: pal.primary,
            inactive: pal.disabled,
            hover: pal.accent,
        }
    }

    /// Applies the `[ui] view-*-border-style` overrides over the defaults. An empty
    /// config string means "unset" - that is why the config keys default to empty (see
    /// [`crate::provision::config::UiConfig`]) - and leaves that role at its default colour.
    pub fn resolve(cfg_active: &str, cfg_inactive: &str, cfg_hover: &str) -> Self {
        Self::resolve_with_palette(
            cfg_active,
            cfg_inactive,
            cfg_hover,
            &crate::ui::palette::Palette::default(),
        )
    }

    pub(crate) fn resolve_with_palette(
        cfg_active: &str,
        cfg_inactive: &str,
        cfg_hover: &str,
        palette: &crate::ui::palette::Palette,
    ) -> Self {
        let d = ViewBorderColors::from_palette(palette);
        let pick = |cfg: &str, fb: Color| {
            if cfg.trim().is_empty() {
                fb
            } else {
                map_color(cfg)
            }
        };
        ViewBorderColors {
            active: pick(cfg_active, d.active),
            inactive: pick(cfg_inactive, d.inactive),
            hover: pick(cfg_hover, d.hover),
        }
    }
}

/// The hint bar's built-in default style: the active palette's `bar_bg` background with
/// `bar_fg` text - two ANSI slots, so the theme resolves both and the pair stays legible
/// on any theme that keeps its own slots legible. It reads as chrome rather than
/// shouting over the content.
/// Key tokens get the accent on top of this (see [`Chrome::hint_bar_spans`] - only
/// while this default is in effect, so a `[ui] hint-bar-style` override keeps its
/// exact colours). Used when `[ui] hint-bar-style` is unset.
pub(crate) fn hint_bar_default_style(palette: &crate::ui::palette::Palette) -> Style {
    Style::default().bg(palette.bar_bg).fg(palette.bar_fg)
}

/// Parses a `[ui] hint-bar-style` spec into the hint bar [`Style`]. Empty ⇒ the
/// built-in tmux default ([`hint_bar_default_style`]). Otherwise a tmux-style comma
/// list: `bg=<colour>` sets the background, `fg=<colour>` (or a bare colour token) the
/// foreground, using the same colour slots as the view border ([`map_color`], so
/// named colours, `colourN`, `#RRGGBB`, `default`). Unrecognised tokens are ignored.
pub(crate) fn parse_hint_bar_style(spec: &str, palette: &crate::ui::palette::Palette) -> Style {
    if spec.trim().is_empty() {
        return hint_bar_default_style(palette);
    }
    let mut style = Style::default();
    for tok in spec.split(',') {
        let tok = tok.trim();
        if let Some(c) = tok.strip_prefix("bg=") {
            style = style.bg(map_color(c));
        } else if let Some(c) = tok.strip_prefix("fg=") {
            style = style.fg(map_color(c));
        } else if !tok.is_empty() {
            style = style.fg(map_color(tok));
        }
    }
    style
}

/// Parses a `[ui] selection-style` spec into the selected card's background. Empty ⇒
/// `None`, leaving the selection to reverse video - the terminal theme's own selected
/// look, and xmux's default. Accepts the same colour slots as the view
/// border ([`map_color`]): `bg=<colour>`, or a bare colour token, since a selection
/// surface IS a background and naming it twice would be noise. A `fg=` token is
/// ignored - the card's text keeps its per-level roles.
pub(crate) fn parse_selection_bg(spec: &str) -> Option<Color> {
    for tok in spec.split(',') {
        let tok = tok.trim();
        if let Some(c) = tok.strip_prefix("bg=") {
            return Some(map_color(c));
        }
        if !tok.is_empty() && !tok.starts_with("fg=") {
            return Some(map_color(tok));
        }
    }
    None
}

/// Builds the palette overrides from `[ui]` keys: each non-empty role string becomes
/// `Some(map_color(..))`, each empty one `None` (the theme's own slot). `selection-style`
/// folds into the same struct. The caller resolves the result with the selected theme.
pub(crate) fn palette_overrides(
    ui: &crate::provision::config::UiConfig,
) -> crate::ui::palette::Overrides {
    let pick = |s: &str| -> Option<Color> {
        if s.trim().is_empty() {
            None
        } else {
            Some(map_color(s))
        }
    };
    crate::ui::palette::Overrides {
        primary: pick(&ui.primary),
        secondary: pick(&ui.secondary),
        accent: pick(&ui.accent),
        decoration: pick(&ui.decoration),
        warning: pick(&ui.warning),
        error: pick(&ui.error),
        disabled: pick(&ui.disabled),
        bar_bg: pick(&ui.bar_bg),
        bar_fg: pick(&ui.bar_fg),
        bar_accent: pick(&ui.bar_accent),
        selection_bg: parse_selection_bg(&ui.selection_style),
    }
}

/// The hint bar's refusal style: a solid error bar (the active palette's
/// `error` as the background, the bar's own text slot on top) that breaks hard
/// from the calm default so a refused action reads as an
/// error at a glance, not as more of the hint bar's keys. Every flash paints
/// this. Fixed, not configurable: an error must stay legible regardless of any
/// `[ui] hint-bar-style` override.
pub(crate) fn error_flash_style(palette: &crate::ui::palette::Palette) -> Style {
    Style::default().bg(palette.error).fg(palette.bar_fg)
}

/// How much of its row the hint bar paints.
///
/// At rest the bar is the prefix indicator, a label sized to what it says, so a column's
/// bottom row and a band's seam keep the rest of their cells. A floating bar takes the
/// whole row, because it has to be readable over whatever it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarFill {
    /// The whole rect: a solid bar. What a floating bar always uses.
    Row,
    /// The text plus a cell of padding, on its own background: the resting label.
    Content,
}

/// The login pane's keys, stated under its form.
const LOGIN_HINTS: &[crate::ui::modal::Hint] = &[
    ("Tab", "next"),
    ("Enter", "next / log in"),
    ("Space", "choose"),
    ("Esc", "nav"),
];

/// The mark a BLOCKED host wears on its nav card, flush after the host name. A blocked
/// host is a failure the user can act on (the login pane), so it keeps the warning
/// colour. One column wide: a card's columns are laid out in
/// cells, and a wide glyph here would shift every column after it.
pub(crate) const BLOCK_MARK: &str = "?";
pub(crate) const UNREACHABLE_MARK: &str = "▲";
pub(crate) const LIST_FAILED_MARK: &str = "✗";

use crate::model::ViewScreen;

pub(crate) struct ViewScreenRender<'a> {
    pub(crate) address: &'a crate::session::Address,
    pub(crate) kind: ViewScreen,
    pub(crate) focused: bool,
    /// The screen is a machine's rather than a host's or a session's.
    pub(crate) machine_screen: bool,
    /// The links the screen offers, in the order the arrow keys walk them. A host's
    /// first link is its machine, written as the machine half of the headline.
    pub(crate) links: &'a [ScreenLink],
    /// The hard-selected link, drawn while the terminal view holds the focus.
    pub(crate) link: Option<usize>,
    /// The link under the pointer.
    pub(crate) link_hover: Option<usize>,
}

/// One link a screen offers: the node it opens, the name it is written as, what the
/// screen states beside it, and the card number it carries on the landing screen. In
/// terminal focus a link is a selection target: the arrow keys move its hard selection,
/// the pointer its soft one, and Enter or a click opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenLink {
    pub(crate) node: crate::model::Node,
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) number: Option<usize>,
}

/// Where a screen painted one of its links: the link, the line, the first column and the
/// width, relative to the screen's own area.
type LinkCell = (usize, usize, u16, u16);

/// A view screen's rows as its paint and its hit-test both read them.
struct ScreenLines {
    lines: Vec<Line<'static>>,
    /// The row and column of the login pane's caret while a text field takes keys.
    caret: Option<(usize, u16)>,
    links: Vec<LinkCell>,
    /// The row of the login pane's focused stop while the pane takes keys.
    focus_line: Option<usize>,
}

/// The first line of a view screen shown in `height` rows: the top, unless the
/// hard-selected link or the login pane's focused stop would fall below the area, in
/// which case the screen scrolls just far enough to show it on its last row. A link the
/// user can select is a link the user can see before opening it, and a stop that takes
/// keys is a stop the user can see while typing into it.
fn screen_top(screen: &ScreenLines, selected: Option<usize>, height: u16) -> usize {
    selected
        .and_then(|s| screen.links.iter().find(|l| l.0 == s))
        .map(|&(_, line, _, _)| line)
        .or(screen.focus_line)
        .map_or(0, |line| (line + 1).saturating_sub(height as usize))
}

impl ViewScreen {
    /// The state word under the headline. The two SETTLED HOST states read theirs from
    /// the one source the nav cards read, so a card and the screen reached from it can
    /// never name the same state two ways; the self-session state is not a host state and
    /// names itself. A blocked host's word never says what it was blocked on; the login
    /// pane states that.
    fn word(self) -> &'static str {
        match self {
            ViewScreen::Scanning => "scanning",
            ViewScreen::SelfSession => "running xmux",
            ViewScreen::Login => crate::ui::tree::host_state_word(false, true, false, true),
            ViewScreen::ListFailed => crate::ui::tree::host_state_word(false, false, true, true),
            ViewScreen::Unreachable => crate::ui::tree::host_state_word(false, false, false, true),
            ViewScreen::Empty => crate::ui::tree::host_state_word(false, false, false, false),
            ViewScreen::Host => "sessions",
            ViewScreen::Machine => crate::ui::tree::MACHINE_REACHABLE,
            ViewScreen::Landing => "",
        }
    }
}

/// How many times in a row this host has failed, in words.
///
/// It separates one failed request from repeated failed requests. The last successful
/// reach is recorded separately, since a failed host is asked again only on user action.
fn failure_run_words(runs: u32) -> String {
    match runs {
        0 | 1 => "first failure".to_string(),
        n => format!("{n} in a row"),
    }
}

fn reached_at(at: std::time::SystemTime) -> String {
    time::OffsetDateTime::from(at)
        .format(&time::macros::format_description!(
            "[year]-[month]-[day] [hour]:[minute] UTC"
        ))
        .unwrap_or_else(|_| "unknown time".into())
}

fn unreachable_verdict(reason: &str) -> String {
    let reason = crate::model::host_def::without_exit_line(reason);
    if let Some(first) = reason
        .lines()
        .next()
        .filter(|first| first.starts_with("the "))
    {
        return first.trim().to_string();
    }
    let line = reason
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("connection closed")
        .trim();
    if let Some(target) = line.strip_prefix("ssh: connect to host ") {
        if let Some((address, rest)) = target.split_once(" port ") {
            if let Some((port, cause)) = rest.split_once(": ") {
                return match cause {
                    "Connection timed out" => {
                        format!("No answer from {address}:{port} before the connection timeout.")
                    }
                    "Connection refused" => format!("Connection refused by {address}:{port}."),
                    _ => format!("{address}:{port}: {cause}"),
                };
            }
        }
    }
    line.to_string()
}

#[cfg(test)]
mod host_verdict_tests {
    use super::unreachable_verdict;

    #[test]
    fn verdict_uses_the_meaningful_ssh_line() {
        assert_eq!(
            unreachable_verdict("ssh: connect to host 10.0.9.3 port 22: Connection timed out\ncommand exited with status 255"),
            "No answer from 10.0.9.3:22 before the connection timeout."
        );
        assert_eq!(
            unreachable_verdict("ssh: connect to host prod port 22: Connection refused"),
            "Connection refused by prod:22."
        );
    }
}

/// The OTHER hosts on `host`'s machine, each with what it last answered, in the
/// inventory's own order.
///
/// A machine serving several muxes gets one host per mux, and they fail
/// independently: this is what says whether the machine or the mux is the thing that is
/// down. Empty when the machine serves this host alone, and then the screen carries no
/// such row rather than an empty one.
fn siblings(
    state: &crate::state::State,
    host: &str,
    label: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let machine = crate::session::machine_of(host);
    state
        .groups
        .iter()
        .filter(|g| g.host != host && crate::session::machine_of(&g.host) == machine)
        .map(|g| {
            let failure = g.failure();
            let word = if state.scanning.contains(&g.host) {
                "still scanning".to_string()
            } else if let Some(kind) = failure {
                crate::ui::tree::failure_word(kind, g.logged_out()).to_string()
            } else {
                match g.sessions.len() {
                    0 => crate::ui::tree::host_state_word(false, false, false, false).to_string(),
                    1 => "1 session".to_string(),
                    n => format!("{n} sessions"),
                }
            };
            format!("{} · {word}", label(&g.host))
        })
        .collect()
}

/// How xmux reaches one host, in the words the unreachable screen prints.
///
/// Resolved once at startup from that host's own config, because how a host is
/// REACHED cannot change under a run - only whether it answers can. Every field is
/// already words: the screen prints them and nothing branches on any of them, which is
/// what keeps this layer blind to which machine kind or which mux a host is.
/// The left cell of a host-screen row: what the row is about, and how it reads.
enum ScreenCell {
    /// A key the user can press on this screen. Bold and nothing else, the help modal's
    /// own key column, so a key reads as a key wherever it is offered.
    Key(String),
    /// The name of the datum beside it, muted so the datum itself reads first.
    Label(&'static str),
    /// No cell: the value continues the row above in the same value column, so a
    /// multi-line value stays one row rather than becoming a run of nameless ones.
    Continued,
    /// No row at all - the blank line parting two blocks of them.
    Gap,
    /// A row whose value opens with link `link`: the name of the list in the cell on the
    /// list's first row, no cell on the rows after it, or the card number on every row of
    /// the landing screen's list.
    Link(Option<String>, usize),
}

impl ScreenCell {
    fn text(&self) -> &str {
        match self {
            ScreenCell::Key(k) => k,
            ScreenCell::Label(l) => l,
            ScreenCell::Link(Some(l), _) => l,
            ScreenCell::Continued | ScreenCell::Gap | ScreenCell::Link(None, _) => "",
        }
    }

    fn style(&self, palette: &crate::ui::palette::Palette) -> Style {
        match self {
            ScreenCell::Key(_) => crate::ui::palette::interaction_key_style(),
            ScreenCell::Label(_) | ScreenCell::Link(..) => Style::default().fg(palette.decoration),
            ScreenCell::Continued | ScreenCell::Gap => Style::default(),
        }
    }
}

/// How a link reads: the accent, reversed while it is the hard selection and underlined
/// while the pointer is on it, the same two looks a nav target takes.
fn link_style(
    palette: &crate::ui::palette::Palette,
    index: usize,
    view: (Option<usize>, Option<usize>),
) -> Style {
    let mut style = Style::default().fg(palette.accent);
    if view.0 == Some(index) {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if view.1 == Some(index) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

impl Default for Chrome {
    fn default() -> Self {
        let palette = crate::ui::palette::Palette::default();
        Chrome {
            flash: String::new(),
            flash_until: None,
            selection_hint: None,
            first_key_seen: false,
            first_key_notice: false,
            auto_hide: false,
            view_border_hovered: false,
            spinner: HashSet::new(),
            spinner_frame: 0,
            animation_ms: 0,
            braille_animation: true,
            login_defaults: HashMap::new(),
            ssh_stanzas: HashMap::new(),
            roster_providers: HashMap::new(),
            host_reach: HashMap::new(),
            log_path: String::new(),
            ui_prefix: "C-g".into(),
            armed: false,
            nav_position: crate::ui::switcher::NavPosition::Left,
            colors: ViewBorderColors::from_palette(&palette),
            hint_bar_style: hint_bar_default_style(&palette),
        }
    }
}

impl Chrome {
    /// Derives the chrome's own styles from the applied palette and the `[ui]` overrides:
    /// the view border colours, which mark the focused view and take no colour from any
    /// machine or mux, and the hint bar style (`[ui] hint-bar-style`, else the tmux default).
    pub(crate) fn apply_palette(
        &mut self,
        ui: &crate::provision::config::UiConfig,
        palette: &crate::ui::palette::Palette,
    ) {
        if crate::ui::palette::no_color() {
            self.colors = ViewBorderColors::from_palette(palette);
            self.hint_bar_style = hint_bar_default_style(palette);
        } else {
            self.colors = ViewBorderColors::resolve_with_palette(
                &ui.view_active_border_style,
                &ui.view_border_style,
                &ui.view_border_hover_style,
                palette,
            );
            self.hint_bar_style = parse_hint_bar_style(&ui.hint_bar_style, palette);
        }
    }

    /// The rule between the nav and the terminal view. The whole rule uses the active
    /// colour while the nav is focused and the inactive colour while the terminal is
    /// focused. The glyph also encodes auto-hide-nav mode: a double line when on and a
    /// single line when off, so a visible nav that will vanish on blur is distinguishable
    /// from a pinned one. Hover keeps its heavy glyph and hover colour.
    pub(crate) fn render_view_border(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        let color = if terminal_focused {
            self.colors.inactive
        } else {
            self.colors.active
        };
        // Band layout: the view border runs horizontally between the two views. It uses
        // one colour across its full length, like the vertical rule.
        if area.width > area.height {
            let g = if self.view_border_hovered {
                "━"
            } else if self.auto_hide {
                "═"
            } else {
                "─"
            };
            let style = Style::default().fg(if self.view_border_hovered {
                self.colors.hover
            } else {
                color
            });
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    g.repeat(area.width as usize),
                    style,
                ))),
                area,
            );
            return;
        }
        // Box-drawing rules have no bold form (the BOLD modifier does not thicken them),
        // so hover swaps the glyph itself to the HEAVY vertical for a thicker line in the
        // hover colour: the same rule, thicker and lit, as the grab cue.
        let (glyph, style) = if self.view_border_hovered {
            ("┃", Style::default().fg(self.colors.hover))
        } else if self.auto_hide {
            ("║", Style::default().fg(color))
        } else {
            ("│", Style::default().fg(color))
        };
        let bars = Text::from(
            (0..area.height)
                .map(|_| Line::from(Span::styled(glyph, style)))
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Paragraph::new(bars), area);
    }

    /// Thickens the stretch of a side nav's view border beside the cards on screen when
    /// the list overflows: the border's own heavy glyph in the border's own colour, so the
    /// overflow is read off the one line the nav draws. The hover cue already thickens the
    /// whole border, so the stretch is not drawn over it.
    pub(crate) fn render_seam_thumb(&self, frame: &mut Frame, rect: Rect, terminal_focused: bool) {
        if rect.is_empty() || self.view_border_hovered {
            return;
        }
        let color = if terminal_focused {
            self.colors.inactive
        } else {
            self.colors.active
        };
        let style = Style::default().fg(color);
        let buf = frame.buffer_mut();
        for y in rect.y..rect.bottom() {
            let cell = &mut buf[(rect.x, y)];
            cell.set_symbol("┃");
            cell.set_style(style);
        }
    }

    /// The terminal-view HOST SCREEN: what fills the terminal-view region in place of a
    /// mux, for a selected host with no session to show.
    ///
    /// One screen grammar for settled states and a scanning host: the host's name as the headline,
    /// under it the same status word its nav card carries, then the rows
    /// that apply to it. A row uses a left-aligned name, whitespace, then the value,
    /// so a key offered on a screen looks like a key
    /// offered anywhere else, and a datum's name stays quieter than the datum.
    ///
    /// The settled host screens and the own-session screen are ONE factual screen in
    /// several states, not a panel each: one builder lays them all out, so the headline,
    /// the state word, and the key rows cannot drift apart, and a later settled state
    /// joins this grammar. The domain model chooses the state; this renders the result.
    /// When the rows left below the content fit a complete frame, the Braille animation
    /// is centered there, on a settled screen as on a scanning one.
    pub(crate) fn render_view_screen(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        view: ViewScreenRender<'_>,
        palette: &crate::ui::palette::Palette,
    ) -> Option<ratatui::layout::Position> {
        let screen = self.view_screen_lines(state, &view, area.width, palette);
        let top = screen_top(&screen, view.link, area.height);
        let caret = screen.caret;
        let lines: Vec<Line<'static>> = screen.lines.into_iter().skip(top).collect();
        let content_rows = lines.len().min(area.height as usize) as u16;
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        let blank = Rect {
            y: area.y + content_rows,
            height: area.height - content_rows,
            ..area
        };
        if self.braille_animation && crate::ui::braille_x::fits(blank) {
            crate::ui::braille_x::render(frame, blank, self.animation_ms);
        }
        caret
            .and_then(|(row, col)| Some((row.checked_sub(top)?, col)))
            .filter(|&(row, col)| row < area.height as usize && col < area.width)
            .map(|(row, col)| ratatui::layout::Position {
                x: area.x + col,
                y: area.y + row as u16,
            })
    }

    /// How far the scan has come, as the landing screen states it under its headline: how
    /// many machines answered out of all of them, turning the spinner the cards turn while
    /// any is still scanning. A machine serving several muxes counts once, and only once
    /// none of its hosts is still scanning.
    pub(crate) fn scan_progress(&self, state: &crate::state::State) -> String {
        let hostless = state.hostless_machines();
        let machines: std::collections::BTreeSet<&str> = state
            .groups
            .iter()
            .map(|g| crate::session::machine_of(&g.host))
            .chain(hostless.iter().map(|m| m.name.as_str()))
            .collect();
        let scanning: std::collections::BTreeSet<&str> = state
            .groups
            .iter()
            .filter(|g| state.scanning.contains(&g.host))
            .map(|g| crate::session::machine_of(&g.host))
            .chain(
                hostless
                    .iter()
                    .filter(|m| state.machine_scanning.contains(&m.name))
                    .map(|m| m.name.as_str()),
            )
            .collect();
        let total = machines.len();
        let done = total - scanning.len();
        if scanning.is_empty() {
            format!("{done} of {total} machines scanned")
        } else {
            let sp = crate::ui::spinner_glyph(self.spinner_frame);
            format!("{sp} {done} of {total} machines scanned")
        }
    }

    /// Where `view` paints its links inside `area`, read from the same lines the paint
    /// draws and scrolled the same way, so a click is hit-tested against what is on
    /// screen.
    pub(crate) fn view_link_rects(
        &self,
        state: &crate::state::State,
        view: &ViewScreenRender<'_>,
        area: Rect,
        palette: &crate::ui::palette::Palette,
    ) -> Vec<(usize, Rect)> {
        let screen = self.view_screen_lines(state, view, area.width, palette);
        let top = screen_top(&screen, view.link, area.height);
        screen
            .links
            .into_iter()
            .filter_map(|(link, line, col, width)| Some((link, line.checked_sub(top)?, col, width)))
            .filter(|&(_, line, col, _)| line < area.height as usize && col < area.width)
            .map(|(link, line, col, width)| {
                (
                    link,
                    Rect {
                        x: area.x + col,
                        y: area.y + line as u16,
                        width: width.min(area.width - col),
                        height: 1,
                    },
                )
            })
            .collect()
    }

    /// The name a view screen carries at its top, in the grammar the nav cards use:
    /// `{machine}/{mux}` for a host's screen, and that with the session under it for the
    /// session xmux is itself running in. What arrives is the [`crate::session::Address`]
    /// the screen was reached by, which is the host id and, for the session screen, its
    /// session name - the two halves are already separate, so nothing is re-split.
    fn headline(
        &self,
        state: &crate::state::State,
        address: &crate::session::Address,
        kind: ViewScreen,
        machine_screen: bool,
    ) -> String {
        if address.host.is_empty() && kind != ViewScreen::Landing {
            return String::new();
        }
        if machine_screen {
            return crate::session::machine_of(&address.host).to_string();
        }
        match kind {
            // The root of the hierarchy, above every machine.
            ViewScreen::Landing => "xmux".into(),
            ViewScreen::Scanning => self.host_label(&address.host),
            ViewScreen::SelfSession => {
                if address.session.is_empty() {
                    self.host_label(&address.host)
                } else {
                    // The mux the session's card names, so the screen behind the card
                    // spells it the same way before the host's reach resolves.
                    let host_mux = |host: &str| self.host_mux(host).to_string();
                    let mux = state
                        .groups
                        .iter()
                        .flat_map(|g| &g.sessions)
                        .find(|s| s.host == address.host && s.name == address.session)
                        .map_or_else(
                            || host_mux(&address.host),
                            |s| crate::ui::tree::session_mux(s, &host_mux),
                        );
                    crate::session::session_label(
                        crate::session::machine_of(&address.host),
                        &mux,
                        &address.session,
                    )
                }
            }
            // An EMPTY host answered - it has no session, which is itself an answer
            // through its mux - so its screen names the pair. An unreachable or blocked
            // host answered nothing, so its screen reads the machine alone unless the id
            // names the mux. A listing failure keeps the confirmed mux that answered.
            ViewScreen::Unreachable
            | ViewScreen::Login
            | ViewScreen::ListFailed
            | ViewScreen::Empty
            | ViewScreen::Host
            | ViewScreen::Machine => self.host_label_when(
                &address.host,
                matches!(kind, ViewScreen::Empty | ViewScreen::Host),
            ),
        }
    }

    /// The ssh config entry the machine of `host` matches, one row per line, or the row
    /// saying none matches.
    fn ssh_config_rows(&self, host: &str) -> Vec<(ScreenCell, String)> {
        let stanza = self
            .ssh_stanzas
            .get(crate::session::machine_of(host))
            .map(String::as_str)
            .unwrap_or_default();
        if stanza.is_empty() {
            return vec![(
                ScreenCell::Label("ssh config"),
                "(no matching entry)".into(),
            )];
        }
        stanza
            .lines()
            .enumerate()
            .map(|(i, l)| {
                let cell = if i == 0 {
                    ScreenCell::Label("ssh config")
                } else {
                    ScreenCell::Continued
                };
                (cell, l.trim_end().to_string())
            })
            .collect()
    }

    /// The lines of [`render_view_screen`](Self::render_view_screen), with the login
    /// pane's caret and focused stop and where the links stand. Split out
    /// because the layout IS the list of rows: both states build one, so neither can drift
    /// into a paragraph of its own shape.
    fn view_screen_lines(
        &self,
        state: &crate::state::State,
        view: &ViewScreenRender<'_>,
        width: u16,
        palette: &crate::ui::palette::Palette,
    ) -> ScreenLines {
        let (address, kind, focused, machine_screen) =
            (view.address, view.kind, view.focused, view.machine_screen);
        let marks = (view.link, view.link_hover);
        // The login pane is a machine's: a host refused until a login reads its failure,
        // and its machine's screen is where the login is.
        let pane = kind == ViewScreen::Login && machine_screen;
        // A host or machine the user logged out of: the login screen states that state in
        // place of a refusal.
        let logged_out = kind == ViewScreen::Login && state.logged_out(&address.host);
        let pal = palette;
        let mut caret = None;
        let mut focus_line = None;
        let p = &self.ui_prefix;
        let host = address.host.as_str();
        // The rows in reading order: a reachable empty host offers actions before
        // observation facts; failures explain the reason before their actions.
        let mut rows: Vec<(ScreenCell, String)> = Vec::new();
        // The rows before this index are the host facts; on the login pane they fold
        // under its details choice while it states a failure.
        let mut facts_end = 0;
        if kind == ViewScreen::SelfSession {
            // The whole screen is the why. No key is offered: nothing the user could
            // press here would make this session showable, and the session is reachable
            // from its own mux without xmux in the middle.
            rows.push((
                ScreenCell::Label("mirror"),
                "refused: xmux is running in this session".into(),
            ));
            rows.push((ScreenCell::Gap, String::new()));
            rows.push((
                ScreenCell::Label("why"),
                "showing it would attach a second client to the session holding xmux, \
                 which moves your own client and paints xmux inside itself"
                    .into(),
            ));
        } else if matches!(
            kind,
            ViewScreen::Unreachable | ViewScreen::Login | ViewScreen::ListFailed
        ) {
            // WHAT failed, then WHEN, then what was asked of the host and how, then who
            // put it on the list, then how it is configured, then what else on that same
            // machine answered, then where the whole history is written. Read top to
            // bottom it is one account of a failure: the message, its age, the command
            // behind it, and the two things that decide whether the box or the mux is at
            // fault. Nothing here is abbreviated to fit - a value too wide hangs under
            // its value column (see below), because a datum the user came here to read is
            // worth more than a tidy column. The user reached this screen because the
            // one-line state word was not enough, so it states everything known about the
            // state, not the minimum that identifies it; a datum nothing recorded is an
            // ABSENT row, never a blank one.
            // The login pane states its failure above these rows, as a verdict over ssh's
            // own text, so only the other screens carry the reason as a row. A logout is
            // the state word itself and has no reason to state.
            if !pane && !logged_out {
                let login_report = state.login_reports.get(crate::session::machine_of(host));
                let reason = login_report
                    .and_then(|report| report.connect.reason().map(str::to_string))
                    .or_else(|| {
                        state
                            .groups
                            .iter()
                            .find(|g| g.host == host)
                            .and_then(|g| g.err.clone())
                    })
                    .or_else(|| {
                        state
                            .machine(host)
                            .filter(|m| !state.has_hosts(&m.name))
                            .and_then(|m| m.err.clone())
                    })
                    .unwrap_or_else(|| "connection closed".into());
                rows.push((ScreenCell::Label("reason"), reason));
            }
            if let Some(registration) = state
                .registration_reports
                .get(crate::session::machine_of(host))
                .filter(|_| machine_screen)
            {
                use crate::ui::ops::RegistrationOutcome;
                let registration = match registration {
                    RegistrationOutcome::NotRequested => None,
                    RegistrationOutcome::Registered => Some("registered".to_string()),
                    RegistrationOutcome::Skipped(reason) => Some(format!("skipped: {reason}")),
                    RegistrationOutcome::Failed(reason) => Some(format!("failed: {reason}")),
                };
                if let Some(registration) = registration {
                    rows.push((ScreenCell::Label("public key"), registration));
                }
            }
            if let Some(runs) = state.failure_runs.get(host) {
                rows.push((ScreenCell::Label("failures"), failure_run_words(*runs)));
            }
            rows.push((ScreenCell::Gap, String::new()));
            // What was asked, and of what. The mux and the machine are the two
            // independent things that can be wrong: the box may be up with no such mux
            // on it, or the mux fine behind a box that cannot be reached. The machine
            // screen states how the machine was asked, which is all an unresolved machine
            // has; the mux row belongs to the host screen.
            if let Some(reach) = self.host_reach.get(host) {
                let level_rows = if machine_screen {
                    [
                        ("machine", &reach.machine),
                        ("socket", &reach.socket),
                        ("probe", &reach.probe),
                    ]
                    .to_vec()
                } else {
                    [
                        ("mux", &reach.mux),
                        ("socket", &reach.socket),
                        ("probe", &reach.probe),
                    ]
                    .to_vec()
                };
                for (label, value) in level_rows {
                    if !value.is_empty() {
                        rows.push((ScreenCell::Label(label), value.clone()));
                    }
                }
            }
            // WHERE this machine came from, between what failed and how it is configured. A
            // machine that fails is worth nothing if the user cannot tell why it is on the
            // list at all: a tailnet peer they never wrote down reads as a mystery until
            // the row names the provider that offered it, which is also the provider
            // they would turn off.
            if let Some(provider) = self
                .roster_providers
                .get(crate::session::machine_of(host))
                .filter(|_| machine_screen)
            {
                rows.push((ScreenCell::Label("provider"), provider.clone()));
            }
            if machine_screen {
                rows.extend(self.ssh_config_rows(host));
            }
            // The other muxes on the SAME machine, each with what it answered. This is
            // the one row that tells the user which half is broken without leaving the
            // screen: a sibling serving sessions says the box is up and this mux is not.
            for (i, sib) in siblings(state, host, &|s| self.host_label(s))
                .into_iter()
                .filter(|_| !machine_screen)
                .enumerate()
            {
                let cell = if i == 0 {
                    ScreenCell::Label("same machine")
                } else {
                    ScreenCell::Continued
                };
                rows.push((cell, sib));
            }
            if !self.log_path.is_empty() {
                rows.push((ScreenCell::Label("log"), self.log_path.clone()));
            }
            rows.push((ScreenCell::Gap, String::new()));
            facts_end = rows.len();
        } else if kind == ViewScreen::Machine {
            // How the machine is reached, then how it logs in, then when it last answered:
            // the facts that belong to the machine whichever of its muxes is asked.
            let reach = self.host_reach.get(host);
            if reach.is_some_and(|reach| reach.ssh) {
                let defaults = self.login_defaults(host);
                rows.push((ScreenCell::Label("address"), defaults.address.value));
                rows.push((ScreenCell::Label("port"), defaults.port.value));
                rows.push((ScreenCell::Label("user"), defaults.username.value));
                let machine = crate::session::machine_of(host);
                let method = state
                    .ssh_login(machine, None)
                    .map(|method| method.label())
                    .unwrap_or("not observed");
                rows.push((ScreenCell::Label("SSH login"), method.into()));
                rows.extend(self.ssh_config_rows(host));
            } else if let Some(reach) = reach.filter(|reach| !reach.machine.is_empty()) {
                rows.push((ScreenCell::Label("machine"), reach.machine.clone()));
            }
            if let Some(registration) = state
                .registration_reports
                .get(crate::session::machine_of(host))
            {
                use crate::ui::ops::RegistrationOutcome;
                let value = match registration {
                    RegistrationOutcome::NotRequested => None,
                    RegistrationOutcome::Registered => Some("registered".to_string()),
                    RegistrationOutcome::Skipped(reason) => Some(format!("skipped: {reason}")),
                    RegistrationOutcome::Failed(reason) => Some(format!("failed: {reason}")),
                };
                if let Some(value) = value {
                    rows.push((ScreenCell::Label("public key"), value));
                }
            }
            let machine = crate::session::machine_of(host);
            if let Some(reached) = state
                .last_reached
                .iter()
                .filter(|(s, _)| crate::session::machine_of(s) == machine)
                .map(|(_, at)| *at)
                .max()
            {
                rows.push((ScreenCell::Label("last reached"), reached_at(reached)));
            }
            rows.push((ScreenCell::Gap, String::new()));
        } else if kind == ViewScreen::Host {
            let count = state
                .groups
                .iter()
                .find(|g| g.host == host)
                .map_or(0, |g| g.sessions.len());
            rows.push((ScreenCell::Label("sessions"), count.to_string()));
            if self.host_reach.contains_key(host) {
                rows.push((
                    ScreenCell::Label("updates"),
                    state.refresh_words(host).into(),
                ));
            }
            if let Some(reached) = state.last_reached.get(host) {
                rows.push((ScreenCell::Label("last listed"), reached_at(*reached)));
            }
            rows.push((ScreenCell::Gap, String::new()));
            rows.push((
                ScreenCell::Key(format!("{p} n")),
                "start a new session".into(),
            ));
        } else if kind == ViewScreen::Landing {
            // The landing screen is its list of cards and nothing else.
        } else if kind == ViewScreen::Scanning {
            // A scan has no answer yet, so the screen states only what earlier answers
            // observed. A key is not offered: the re-scan it would start is under way.
            if let Some(runs) = state.failure_runs.get(host) {
                rows.push((ScreenCell::Label("failures"), failure_run_words(*runs)));
            }
            if let Some(reached) = state.last_reached.get(host) {
                rows.push((ScreenCell::Label("last reached"), reached_at(*reached)));
            }
        } else {
            if kind == ViewScreen::Empty {
                rows.push((ScreenCell::Label("sessions"), "0".into()));
                if self.host_reach.contains_key(host) {
                    rows.push((
                        ScreenCell::Label("updates"),
                        state.refresh_words(host).into(),
                    ));
                }
                if let Some(reached) = state.last_reached.get(host) {
                    rows.push((ScreenCell::Label("last listed"), reached_at(*reached)));
                }
                rows.push((ScreenCell::Gap, String::new()));
            }
            // Creating under an unreachable host is refused, so `n` is offered only where
            // it can actually run.
            rows.push((
                ScreenCell::Key(format!("{p} n")),
                "start a new session".into(),
            ));
        }
        if !matches!(
            kind,
            ViewScreen::SelfSession | ViewScreen::Scanning | ViewScreen::Landing
        ) {
            rows.push((
                ScreenCell::Key(format!("{p} r")),
                "rescan this machine".into(),
            ));
            rows.push((
                ScreenCell::Key(format!("{p} R")),
                "rescan all machines".into(),
            ));
        }
        if kind == ViewScreen::Machine && self.host_reach.get(host).is_some_and(|reach| reach.ssh) {
            rows.push((
                ScreenCell::Key(format!("{p} L")),
                "log out of this machine".into(),
            ));
        }

        if kind == ViewScreen::Empty {
            // A reachable empty host offers its next actions before its observation facts.
            if let Some(gap) = rows
                .iter()
                .position(|(cell, _)| matches!(cell, ScreenCell::Gap))
            {
                let facts: Vec<_> = rows.drain(..gap).collect();
                rows.remove(0);
                rows.push((ScreenCell::Gap, String::new()));
                rows.extend(facts);
            }
        }

        if kind == ViewScreen::Unreachable {
            let diagnostics = std::mem::take(&mut rows);
            let reason = diagnostics
                .iter()
                .find_map(|(cell, value)| {
                    matches!(cell, ScreenCell::Label("reason")).then_some(value.as_str())
                })
                .unwrap_or("connection closed");
            let reached = state
                .last_reached
                .get(host)
                .map(|time| format!(" · last reached {}", reached_at(*time)))
                .unwrap_or_default();
            rows.push((
                ScreenCell::Label("verdict"),
                format!("{}{}", unreachable_verdict(reason), reached),
            ));
            let failures = state.failure_runs.get(host).copied().unwrap_or(1);
            rows.push((ScreenCell::Label("status"), failure_run_words(failures)));
            rows.push((ScreenCell::Gap, String::new()));
            rows.push((ScreenCell::Label("What to do"), String::new()));
            rows.push((
                ScreenCell::Key(format!("{p} r")),
                "rescan this machine".into(),
            ));
            rows.push((
                ScreenCell::Key(format!("{p} R")),
                "rescan all machines".into(),
            ));
            rows.push((ScreenCell::Gap, String::new()));
            rows.push((
                ScreenCell::Label("d details"),
                if state.host_details.contains(host) {
                    "hide diagnostics".into()
                } else {
                    "show diagnostics".into()
                },
            ));
            if state.host_details.contains(host) {
                rows.push((ScreenCell::Gap, String::new()));
                rows.extend(diagnostics);
            }
        }

        // The login pane's failure folds the machine facts under its details choice: ssh's
        // whole text and the facts come back together when the user unfolds them.
        let failure = pane.then(|| state.login_failure(host)).flatten();
        let unfolded = state
            .login
            .as_ref()
            .filter(|d| d.host == host)
            .is_some_and(|d| d.details);
        // While a login's steps run, the facts describe the probe failure that login is
        // answering, so they stay folded with nothing to unfold them.
        let steps_running = pane
            && state
                .login_progress
                .get(host)
                .is_some_and(crate::model::LoginProgress::running);
        match &failure {
            Some(failure) if unfolded => {
                if !failure.raw.is_empty() {
                    rows.insert(0, (ScreenCell::Label("ssh output"), failure.raw.clone()));
                }
            }
            Some(_) => {
                rows.drain(..facts_end);
            }
            None if steps_running => {
                rows.drain(..facts_end);
            }
            None => {}
        }

        // The level below, as links: a machine's hosts, a host's sessions, and on the
        // landing screen every card under its number. A host's first link is its machine,
        // which the headline carries.
        let landing = kind == ViewScreen::Landing;
        let listed = if machine_screen || landing { 0 } else { 1 };
        let name = if machine_screen { "hosts" } else { "sessions" };
        // Card numbers line up by units place, as they do in the nav's address column.
        let number_w = view
            .links
            .iter()
            .filter_map(|l| l.number)
            .map(|n| n.to_string().len())
            .max()
            .unwrap_or(0);
        if view.links.len() > listed && kind != ViewScreen::SelfSession {
            rows.push((ScreenCell::Gap, String::new()));
            for (i, link) in view.links.iter().enumerate().skip(listed) {
                let value = if link.value.is_empty() {
                    link.label.clone()
                } else {
                    format!("{}  {}", link.label, link.value)
                };
                let cell = match link.number {
                    Some(n) => Some(format!("{n:>number_w$}")),
                    None => (i == listed && !landing).then(|| name.to_string()),
                };
                rows.push((ScreenCell::Link(cell, i), value));
            }
        }

        // One column width for keys and labels alike keeps values aligned.
        let cw = rows
            .iter()
            .map(|(c, _)| c.text().chars().count())
            .max()
            .unwrap_or(0);
        // A value too wide for its column continues under the SAME value column rather than
        // clipping at the pane edge: ssh names a failure in the LAST clause of a long
        // line, and the card carries only that clause, so a screen that clipped would
        // leave the whole message nowhere readable. A value that already fits is passed
        // through untouched, which is what keeps the ssh stanza's own indentation.
        let value_w = width.saturating_sub(cw as u16 + 4);
        let rows: Vec<(ScreenCell, String)> = rows
            .into_iter()
            .flat_map(|(cell, value)| {
                let mut cell = Some(cell);
                let mut out: Vec<(ScreenCell, String)> = Vec::new();
                for src in value.trim_end().lines() {
                    let fits =
                        unicode_width::UnicodeWidthStr::width(src) <= value_w.max(1) as usize;
                    let parts = if fits {
                        vec![src.to_string()]
                    } else {
                        wrap_text(src.trim(), value_w)
                    };
                    for part in parts {
                        out.push((cell.take().unwrap_or(ScreenCell::Continued), part));
                    }
                }
                match cell {
                    // Nothing to write: the row is a gap, and it keeps its blank line.
                    Some(c) => vec![(c, String::new())],
                    None => out,
                }
            })
            .collect();

        let rule = Span::raw("  ");
        let state_style = Style::default().fg(match kind {
            ViewScreen::Scanning => pal.decoration,
            ViewScreen::Unreachable => pal.error,
            ViewScreen::Login => pal.warning,
            ViewScreen::ListFailed => pal.primary,
            ViewScreen::Empty
            | ViewScreen::SelfSession
            | ViewScreen::Host
            | ViewScreen::Machine
            | ViewScreen::Landing => pal.decoration,
        });
        let headline = self.headline(state, address, kind, machine_screen);
        let bold = Style::default()
            .fg(pal.secondary)
            .add_modifier(Modifier::BOLD);
        let mut links: Vec<LinkCell> = Vec::new();
        // A machine's and a host's screen look alike, so the headline names the level
        // before the path: the user reads `machine db-01` or `host db-01/tmux` and knows
        // which one every row and key below it is about.
        let level = match kind {
            ViewScreen::SelfSession | ViewScreen::Landing => "",
            _ if headline.is_empty() => "",
            _ if machine_screen => "machine ",
            _ => "host ",
        };
        let lead = vec![
            Span::raw(" "),
            Span::styled(level, Style::default().fg(pal.decoration)),
        ];
        let path_col = 1 + level.len() as u16;
        // A host's headline is its path, and the machine half of the path is the link up
        // to the machine's screen.
        let headline_line = match view.links.first() {
            Some(up)
                if !machine_screen
                    && !matches!(kind, ViewScreen::SelfSession | ViewScreen::Landing)
                    && headline.starts_with(&up.label) =>
            {
                let rest = headline[up.label.len()..].to_string();
                links.push((
                    0,
                    1,
                    path_col,
                    unicode_width::UnicodeWidthStr::width(up.label.as_str()) as u16,
                ));
                Line::from(
                    [
                        lead,
                        vec![
                            Span::styled(
                                up.label.clone(),
                                link_style(pal, 0, marks).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(rest, bold),
                        ],
                    ]
                    .concat(),
                )
            }
            _ => Line::from([lead, vec![Span::styled(headline, bold)]].concat()),
        };
        let mut out = vec![
            Line::from(""),
            headline_line,
            Line::from(Span::styled(
                format!(
                    " {}",
                    if landing {
                        self.scan_progress(state)
                    } else if logged_out {
                        crate::ui::tree::LOGGED_OUT.to_string()
                    } else {
                        kind.word().to_string()
                    }
                ),
                state_style,
            )),
        ];
        // The login pane OWNS the connection values: they sit at the panel's top,
        // edited in place from the terminal view (no modal, no nav). The inputs come in
        // two groups, what ssh dials with and what happens after it worked, and whitespace
        // parts them from what the pane reports back: the steps of a login and the
        // failure it ended in. The focused text
        // field shows a cursor, both only while the terminal view is focused and no login
        // runs, so the pane says whether it is taking keys.
        //
        // The pane holds what ssh will not ask for and nothing else. Every value starts at
        // what provisioning reports OpenSSH would use, with the matching ssh config entry
        // as fallback: the chrome receives resolved starting values and the matching
        // stanza text, and does not parse ssh configuration. A blocked host's switch and
        // create are refused. The password is never rendered, and after submit it lives
        // only in the process credential broker, so the rendered frame carries no
        // plaintext.
        if pane {
            use crate::model::{LoginField, LoginStep, StepState};
            use crate::state::{AfterLogin, LoginFocus};
            let running = state.login_run.as_ref().is_some_and(|l| l.host == host);
            let taking_keys = focused && !running;
            let defaults = self.login_defaults(host);
            let draft = state.login.as_ref().filter(|d| d.host == host);
            let fallback = crate::state::LoginDraft {
                address: defaults.address.value.clone(),
                port: defaults.port.value.clone(),
                username: defaults.username.value.clone(),
                default_address: defaults.address.value.clone(),
                default_port: defaults.port.value.clone(),
                default_username: defaults.username.value.clone(),
                ssh_effective: defaults.ssh_effective.clone(),
                ..Default::default()
            };
            let d = draft.unwrap_or(&fallback);
            let marked = failure.as_ref().map(|f| f.fields()).unwrap_or_default();
            // The inputs keep one column of their own, so unfolding the machine facts below
            // the rule never moves a field.
            let fcw = ["address*", "port*", "username*", "password"]
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            let cursor = |active: bool| if active && taking_keys { "▊" } else { "" };
            let reversed = |style: Style, active: bool| {
                if active && taking_keys {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style
                }
            };
            // Labels keep a fixed left edge; the value cell carries input focus.
            let label = |text: String, cause: bool| -> Vec<Span<'static>> {
                let style = Style::default().fg(if cause { pal.error } else { pal.decoration });
                let pad = fcw.saturating_sub(text.chars().count());
                if text.is_empty() {
                    return vec![Span::raw(" ")];
                }
                vec![
                    Span::raw("   "),
                    Span::styled(text, style),
                    Span::raw(" ".repeat(pad)),
                ]
            };
            // A field carries its own emptiness: a required one is marked in its label,
            // and an optional one says so in the space its value would occupy, so the
            // pane never needs a legend to be read. The field a failure concerns carries
            // the failure's mark after its value.
            let field = |name: &str,
                         required: bool,
                         value: &str,
                         mask: bool,
                         active: bool,
                         provenance: &str,
                         which: LoginField| {
                let shown = if mask {
                    "•".repeat(value.chars().count())
                } else {
                    value.to_string()
                };
                let (text, style) = if shown.is_empty() && !active {
                    (
                        if required {
                            String::new()
                        } else {
                            "optional".into()
                        },
                        Style::default().fg(pal.decoration),
                    )
                } else {
                    (shown, Style::default().fg(pal.secondary))
                };
                let cause = marked.contains(&which);
                let mut spans = label(format!("{name}{}", if required { "*" } else { "" }), cause);
                spans.push(rule.clone());
                // The focused value is reversed over its own cells and one caret cell,
                // and the column keeps its width in plain padding.
                let pad = 22usize.saturating_sub(text.chars().count());
                if active && taking_keys {
                    spans.push(Span::styled(text, reversed(style, true)));
                    spans.push(Span::styled(" ", reversed(Style::default(), true)));
                    spans.push(Span::raw(" ".repeat(pad.saturating_sub(1))));
                } else {
                    spans.push(Span::styled(text, style));
                    spans.push(Span::raw(" ".repeat(pad)));
                }
                // Where the value is and how the field reads it; the failure's mark
                // follows. A pane too narrow for these beside the value continues them on
                // the next row under the value column, so nothing it states is cut.
                let mut tail = Vec::new();
                if !provenance.is_empty() {
                    tail.push(Span::styled(
                        format!("  {provenance}"),
                        Style::default().fg(pal.decoration),
                    ));
                }
                if cause {
                    tail.push(Span::styled("  ✗", Style::default().fg(pal.error)));
                }
                let head = Line::from(spans);
                let tail_w: usize = tail.iter().map(Span::width).sum();
                if head.width() + tail_w <= width as usize {
                    let mut line = head;
                    line.spans.extend(tail);
                    vec![line]
                } else if tail.is_empty() {
                    vec![head]
                } else {
                    // The tail's own two leading cells part it from the value; under the
                    // value column they are the indent.
                    let mut under = vec![Span::raw(" ".repeat(fcw + 3))];
                    under.extend(tail);
                    vec![head, Line::from(under)]
                }
            };
            // Choice labels stay plain; the value and its padding carry focus. The focused
            // stop's value is reversed (a stop with no value reverses its own text), only
            // while the pane takes keys. One radio choice under "After login" selects doing
            // nothing, saving the connection values, or registering this machine's public
            // key. Saving is offered only while it would change what ssh uses.
            let choice = |name: &str, mark: &str, text: &str, active: bool| {
                let style = if active {
                    Style::default().fg(pal.secondary)
                } else {
                    Style::default().fg(pal.decoration)
                };
                let mut spans = label(name.to_string(), false);
                spans.push(rule.clone());
                spans.push(Span::styled(
                    if mark.is_empty() {
                        text.to_string()
                    } else {
                        format!(" {mark} {text} ")
                    },
                    reversed(style, active),
                ));
                spans.push(Span::styled(cursor(active), style));
                Line::from(spans)
            };
            let group = |title: &str| {
                Line::from(Span::styled(
                    format!(" {title}"),
                    Style::default()
                        .fg(pal.secondary)
                        .add_modifier(Modifier::BOLD),
                ))
            };
            let provenance = |value: &str, original: &str, resolved: &'static str| {
                if value == original {
                    resolved
                } else {
                    "edited"
                }
            };
            out.push(Line::from(""));
            out.push(group("Connection"));
            // The line each stop of the form stands on, so the screen can scroll the
            // focused one into view in a window too short for the whole form.
            let mut stops: Vec<(LoginFocus, usize)> = Vec::new();
            stops.push((LoginFocus::Address, out.len()));
            out.extend(field(
                "address",
                true,
                &d.address,
                false,
                d.focus == LoginFocus::Address,
                provenance(&d.address, &d.default_address, defaults.address.provenance),
                LoginField::Address,
            ));
            stops.push((LoginFocus::Port, out.len()));
            out.extend(field(
                "port",
                true,
                &d.port,
                false,
                d.focus == LoginFocus::Port,
                provenance(&d.port, &d.default_port, defaults.port.provenance),
                LoginField::Port,
            ));
            stops.push((LoginFocus::Username, out.len()));
            out.extend(field(
                "username",
                true,
                &d.username,
                false,
                d.focus == LoginFocus::Username,
                provenance(
                    &d.username,
                    &d.default_username,
                    defaults.username.provenance,
                ),
                LoginField::Username,
            ));
            stops.push((LoginFocus::Password, out.len()));
            out.extend(field(
                "password",
                false,
                &d.password,
                true,
                d.focus == LoginFocus::Password,
                "",
                LoginField::Password,
            ));
            // The terminal's own cursor sits on the focused field's caret, where an input
            // method draws what it is composing.
            let focused_field = stops
                .iter()
                .find(|&&(stop, _)| stop == d.focus)
                .map(|&(_, row)| row);
            let field_focused = matches!(
                d.focus,
                LoginFocus::Address
                    | LoginFocus::Port
                    | LoginFocus::Username
                    | LoginFocus::Password
            );
            if let Some(row) = focused_field.filter(|_| taking_keys && field_focused) {
                caret = crate::ui::modal::caret_offset(&out[row]).map(|col| (row, col));
            }
            out.push(Line::from(""));
            out.push(group("After login"));
            out.push(choice(
                "",
                if d.after_login == AfterLogin::Nothing {
                    "(*)"
                } else {
                    "( )"
                },
                "do nothing",
                d.focus == LoginFocus::AfterNothing,
            ));
            stops.push((LoginFocus::AfterNothing, out.len() - 1));
            if d.offers_ssh_config() {
                out.push(choice(
                    "",
                    if d.after_login == AfterLogin::SshConfig {
                        "(*)"
                    } else {
                        "( )"
                    },
                    "save connection to ssh config",
                    d.focus == LoginFocus::AfterSshConfig,
                ));
                stops.push((LoginFocus::AfterSshConfig, out.len() - 1));
            }
            out.push(choice(
                "",
                if d.after_login == AfterLogin::RegisterKey {
                    "(*)"
                } else {
                    "( )"
                },
                "register my public key",
                d.focus == LoginFocus::AfterPublicKey,
            ));
            stops.push((LoginFocus::AfterPublicKey, out.len() - 1));
            out.push(Line::from(""));
            // A login under way replaces the button it was started from. The pane keeps
            // every value, so what the user sees is the thing they submitted, still
            // theirs, with the one thing they can now say about it. A lone Esc ends the
            // login. Its verdict brings the button back with the connection values still
            // there; the password is typed again, since it leaves the draft on submit.
            if running {
                out.push(choice("", "", "logging in…  esc to stop", false));
            } else {
                out.push(choice("", "", "[ Log in ]", d.focus == LoginFocus::Submit));
                stops.push((LoginFocus::Submit, out.len() - 1));
            }
            out.push(Line::from(""));
            out.push(Line::from(""));
            // The steps of this machine's last login, while they run and after one of them
            // failed. A login whose every step worked has handed the pane to its sessions,
            // so its steps say nothing more. Each step (connect, authenticate, the selected
            // follow-ups, find mux) carries one state mark: blank for pending, the spinner
            // for running, `✓`, `✗`, or `·` for skipped. A step moves only on an event the
            // login itself reported, never on a timer.
            let wrap_w = width.saturating_sub(4).max(1);
            let progress = state.login_progress.get(host).filter(|p| !p.succeeded());
            if let Some(progress) = progress {
                out.push(Line::from(""));
                for row in &progress.steps {
                    let name = match row.step {
                        LoginStep::Connect => match &progress.target {
                            Some(target) => format!("connect {target}"),
                            None => "connect".to_string(),
                        },
                        LoginStep::Authenticate => format!(
                            "authenticate{}{}",
                            progress
                                .user
                                .as_ref()
                                .map(|u| format!(" as {u}"))
                                .unwrap_or_default(),
                            if progress.password {
                                " with the password"
                            } else {
                                ""
                            }
                        ),
                        LoginStep::Save => "write address, port, username to ssh config".into(),
                        LoginStep::RegisterKey => "register my public key".into(),
                        LoginStep::FindMux => "find mux".into(),
                    };
                    let (glyph, glyph_style, text_style) = match row.state {
                        StepState::Pending => {
                            (' ', Style::default(), Style::default().fg(pal.decoration))
                        }
                        StepState::Running => (
                            crate::ui::spinner_glyph(self.spinner_frame),
                            Style::default().fg(pal.warning),
                            Style::default().fg(pal.secondary),
                        ),
                        StepState::Done => (
                            '✓',
                            Style::default().fg(pal.accent),
                            Style::default().fg(pal.secondary),
                        ),
                        StepState::Failed => (
                            '✗',
                            Style::default().fg(pal.error),
                            Style::default().fg(pal.error),
                        ),
                        StepState::Skipped => (
                            '·',
                            Style::default().fg(pal.decoration),
                            Style::default().fg(pal.decoration),
                        ),
                    };
                    let text = match &row.note {
                        Some(note) => format!("{name}: {note}"),
                        None => name,
                    };
                    // A note may run over several lines (a summary over its detail), so each
                    // line wraps on its own, as the verdict's do.
                    let parts: Vec<String> = text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .flat_map(|l| wrap_text(l.trim(), wrap_w))
                        .collect();
                    for (i, part) in parts.into_iter().enumerate() {
                        let mark = if i == 0 { glyph } else { ' ' };
                        out.push(Line::from(vec![
                            Span::styled(format!(" {mark} "), glyph_style),
                            Span::styled(part, text_style),
                        ]));
                    }
                }
            }
            // The failure reads in one order: the verdict, the field it concerns (marked
            // above), ssh's own last line dimmed, and the choice that unfolds the rest:
            // ssh's whole text with the host facts the other screens state. The details
            // choice is a stop only while the pane states a failure, and Space picks it
            // like any other choice.
            if let Some(failure) = &failure {
                out.push(Line::from(""));
                for (i, line) in failure.verdict.lines().enumerate() {
                    for (j, part) in wrap_text(line, wrap_w).into_iter().enumerate() {
                        let mark = if i == 0 && j == 0 { "✗" } else { " " };
                        out.push(Line::from(vec![
                            Span::styled(format!(" {mark} "), Style::default().fg(pal.error)),
                            Span::styled(
                                part,
                                Style::default().fg(if i == 0 { pal.error } else { pal.secondary }),
                            ),
                        ]));
                    }
                }
                if let Some(last) = failure.raw.lines().rev().find(|l| !l.trim().is_empty()) {
                    for part in wrap_text(last.trim(), wrap_w) {
                        out.push(Line::from(Span::styled(
                            format!("   {part}"),
                            Style::default().fg(pal.decoration),
                        )));
                    }
                }
                out.push(choice(
                    "",
                    if d.details { "[x]" } else { "[ ]" },
                    "details",
                    d.focus == LoginFocus::Details,
                ));
                stops.push((LoginFocus::Details, out.len() - 1));
            }
            if taking_keys {
                focus_line = stops
                    .iter()
                    .find(|&&(stop, _)| stop == d.focus)
                    .map(|&(_, row)| row);
            }
            // The pane's keys, under the form while it takes them.
            if focused {
                out.push(Line::from(""));
                let mut spans = vec![Span::raw(" ")];
                spans.extend(crate::ui::modal::hint_spans(
                    &crate::ui::modal::fit_hints(LOGIN_HINTS, width.saturating_sub(2) as usize),
                    pal,
                ));
                out.push(Line::from(spans));
            }
        }
        out.push(Line::from(""));
        for (cell, value) in rows {
            if matches!(cell, ScreenCell::Gap) {
                out.push(Line::from(""));
                continue;
            }
            let mut spans = vec![
                Span::styled(format!("   {:<cw$}", cell.text()), cell.style(palette)),
                rule.clone(),
            ];
            match cell {
                ScreenCell::Link(_, i) => {
                    let label = &view.links[i].label;
                    let (name, rest) = if value.starts_with(label.as_str()) {
                        (label.clone(), value[label.len()..].to_string())
                    } else {
                        (value.clone(), String::new())
                    };
                    let col = 3 + cw as u16 + 2;
                    links.push((
                        i,
                        out.len(),
                        col,
                        unicode_width::UnicodeWidthStr::width(name.as_str()) as u16,
                    ));
                    spans.push(Span::styled(name, link_style(pal, i, marks)));
                    spans.push(Span::styled(rest, Style::default().fg(pal.decoration)));
                }
                _ => spans.push(Span::raw(value)),
            }
            out.push(Line::from(spans));
        }
        ScreenLines {
            lines: out,
            caret,
            links,
            focus_line,
        }
    }

    /// The hint bar's logical text, fit to `width`. At rest this text is only the prefix,
    /// the nav's prefix indicator, and it stays the prefix while the prefix is armed (the
    /// key list beside it names the keys). An open input keeps the prefix too: the input
    /// says its keys where it is typed. The transient states outrank the rest, in order:
    /// a flash (a refusal), the input, the armed prefix, the hint after a selection move,
    /// then the scan progress. A flash is returned raw - it may
    /// exceed `width`; [`Self::hint_bar_lines`] wraps it so it never clips.
    pub(crate) fn hint_bar_text(&self, width: u16, state: &crate::state::State) -> String {
        // Use the active prefix so the hint_bar matches the user's configured binding.
        let p = &self.ui_prefix;
        if !self.flash.is_empty() {
            format!(" ✗ {}", self.flash)
        } else if state.is_inputting() {
            // An open input says its keys on its own box's border, so the indicator rests.
            fit(&[format!(" {p}"), p.to_string()], width)
        } else if self.armed {
            // A live prefix names its keys in the key list beside the indicator, so the
            // indicator keeps the prefix alone.
            fit(&[format!(" {p}"), p.to_string()], width)
        } else if let Some(hint) = &self.selection_hint {
            selection_hint_text(hint, p, width)
        } else if state.scanning_any() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the resting prefix) once every host has settled. It
            // turns the SAME spinner the scanning cards do, on the same frame, so the
            // bar and the cards read as one thing still loading. A machine with no host
            // known counts as one entry of its own.
            let total = state.groups.len() + state.hostless_machines().len();
            let done = total.saturating_sub(state.scanning.len() + state.machine_scanning.len());
            let sp = crate::ui::spinner_glyph(self.spinner_frame);
            fit(
                &[
                    format!(" {sp} scanning hosts {done}/{total}…"),
                    format!(" {sp} scanning {done}/{total}…"),
                    format!(" {sp} {done}/{total}"),
                    format!(" {sp}{done}/{total}"),
                ],
                width,
            )
        } else if !state.filter.is_empty() {
            // The applied filter has no row of its own, so it shows here with how to
            // change and clear it.
            fit(
                &[
                    format!(" filter: {} · {p} / edit · Esc clear", state.filter),
                    format!(" filter: {}", state.filter),
                ],
                width,
            )
        } else {
            // At rest the text is the prefix alone.
            fit(&[format!(" {p}"), p.to_string()], width)
        }
    }

    /// The hint_bar text split into the lines to render. The fit-based text is always one
    /// line; only a flash (an arbitrary error message) may exceed `width`, so it wraps
    /// across as many nav rows as it needs rather than clipping.
    pub(crate) fn hint_bar_lines(&self, width: u16, state: &crate::state::State) -> Vec<String> {
        let text = self.hint_bar_text(width, state);
        // Only a flash can exceed `width` (the fit-based text is already constrained);
        // wrap it on word boundaries with a consistent left margin.
        if !!self.flash.is_empty() {
            return vec![text];
        }
        wrap_text(text.trim_start(), width.saturating_sub(1))
            .into_iter()
            .map(|l| format!(" {l}"))
            .collect()
    }

    /// The style the hint bar paints with this frame. While a flash is showing it is
    /// the [`error_flash_style`]; otherwise the configured status style. Split from
    /// [`Self::render_hint_bar`] so the choice is unit-testable without a backend.
    pub(crate) fn hint_bar_render_style(&self, palette: &crate::ui::palette::Palette) -> Style {
        if self.flash.is_empty() {
            self.hint_bar_style
        } else {
            error_flash_style(palette)
        }
    }

    /// One hint-bar line as styled spans: each ` · `-separated segment's leading key
    /// token (the prefix `C-g` is its own segment, so every other segment is one key)
    /// gets the accent, the separators go muted, and the rest inherits the bar's base
    /// style. Purely presentational - the text is exactly the [`Self::hint_bar_lines`]
    /// line, so the fit / wrap behaviour is untouched.
    fn hint_bar_line_spans(
        &self,
        line: String,
        palette: &crate::ui::palette::Palette,
        fact: Option<&str>,
    ) -> Line<'static> {
        // The bar's OWN accent, not the card accent: the keys sit on `bar_bg`, a
        // surface the card accent may not read on (see `Palette::bar_accent`). The keys
        // are also BOLD, so a key reads as a key wherever it is offered (the help modal's
        // key column and the host-screen rows are bold the same way).
        let accent = crate::ui::palette::interaction_key_style().fg(palette.bar_accent);
        let sep_style = Style::default().fg(palette.decoration);
        let mut spans: Vec<Span> = Vec::new();
        let segments: Vec<&str> = line.split(" · ").collect();
        let last = segments.len().saturating_sub(1);
        for (i, seg) in segments.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", sep_style));
            }
            // A selection hint ends on its fact, which is words about the card and holds
            // no key.
            if i == last && i > 0 && fact.is_some_and(|f| !f.is_empty() && seg.starts_with(f)) {
                spans.push(Span::raw(seg.to_string()));
                continue;
            }
            // The key = the first token, or the first two when the segment starts with
            // the prefix ("C-g n"). Leading spaces (the bar's left margin) stay raw.
            let lead_len = seg.len() - seg.trim_start().len();
            let (lead, body) = seg.split_at(lead_len);
            if !lead.is_empty() {
                spans.push(Span::raw(lead.to_string()));
            }
            let mut parts = body.splitn(2, ' ');
            let first = parts.next().unwrap_or_default();
            let rest = parts.next();
            let (key, desc) = match rest {
                Some(rest) if first == self.ui_prefix => {
                    let mut sub = rest.splitn(2, ' ');
                    let second = sub.next().unwrap_or_default();
                    (format!("{first} {second}"), sub.next().map(str::to_string))
                }
                _ => (first.to_string(), rest.map(str::to_string)),
            };
            spans.push(Span::styled(key, accent));
            if let Some(desc) = desc {
                spans.push(Span::raw(format!(" {desc}")));
            }
        }
        Line::from(spans)
    }

    /// The version the prefix key list writes on its bottom border: `xmux v<version>`,
    /// built from the crate's own name and version so it always matches what
    /// `xmux --version` reports.
    pub(crate) fn version_label(&self) -> String {
        format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
    }

    pub(crate) fn render_hint_bar(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        fill: BarFill,
        palette: &crate::ui::palette::Palette,
    ) {
        let lines = self.hint_bar_lines(area.width, state);
        // Key tokens get the accent only on the built-in default style with no flash
        // showing: a `[ui] hint-bar-style` override keeps its exact colours (uniform,
        // as configured), and a flash keeps the one solid style of its kind.
        let width = lines
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let flash = !self.flash.is_empty();
        let styled = !flash && self.hint_bar_style == hint_bar_default_style(palette);
        let fact = (!self.armed)
            .then_some(self.selection_hint.as_ref())
            .flatten()
            .map(|h| h.fact.split(':').next().unwrap_or_default().to_string());
        let text = if styled {
            Text::from(
                lines
                    .into_iter()
                    .map(|l| self.hint_bar_line_spans(l, palette, fact.as_deref()))
                    .collect::<Vec<_>>(),
            )
        } else {
            Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>())
        };
        // The hint bar is a solid status bar: the configured status style
        // (`hint_bar_default_style` / the `[ui] hint-bar-style` override) normally, or the
        // flash style of its kind while a flash shows. The style fills the whole area,
        // so the bar spans full width even where the text does not; unstyled spans
        // inherit the bar's fg/bg.
        //
        // `Clear` first, because a style only recolours cells - it does not blank them.
        // A floating bar covers the live grid, so without this the grid's own
        // characters survive in the columns the bar's text does not reach and the bar
        // reads as text spilled across the screen instead of a bar covering it.
        let painted = match fill {
            BarFill::Row => area,
            BarFill::Content => Self::bar_content_rect(area, width),
        };
        frame.render_widget(Clear, painted);
        let style = if flash {
            error_flash_style(palette)
        } else {
            self.hint_bar_style
        };
        frame.render_widget(Paragraph::new(text).style(style), painted);
    }

    /// Paints the resting prefix across a collapsed nav's indicator: after a one-cell
    /// margin on a band's padded chip, from the first cell of a side column, which is
    /// exactly as wide as the prefix. Transient bars are handled by the ordinary
    /// floating-bar path instead.
    pub(crate) fn render_collapsed_hint_bar(
        &self,
        frame: &mut Frame,
        area: Rect,
        padded: bool,
        palette: &crate::ui::palette::Palette,
    ) {
        frame.render_widget(Clear, area);
        let margin = if padded { " " } else { "" };
        let line = self.hint_bar_line_spans(format!("{margin}{}", self.ui_prefix), palette, None);
        frame.render_widget(
            Paragraph::new(line).style(self.hint_bar_render_style(palette)),
            area,
        );
    }

    /// How many cells a [`BarFill::Content`] bar paints, so whatever else is on the row
    /// (a band's overflow counts) can stop where the bar starts instead of being painted
    /// over.
    pub(crate) fn hint_bar_chip_width(&self, width: u16, state: &crate::state::State) -> u16 {
        let content = self
            .hint_bar_lines(width, state)
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0);
        Self::bar_content_rect(Rect::new(0, 0, width, 1), content).width
    }

    /// The bar's rect trimmed to what it has to say, plus one cell of padding, so a
    /// resting bar reads as a label on its row instead of a slab of colour across a
    /// window it has one word for. Never wider than the row it was given.
    fn bar_content_rect(area: Rect, content_w: u16) -> Rect {
        Rect {
            width: content_w.saturating_add(1).min(area.width),
            ..area
        }
    }
}

/// The hint after a selection move, fit to `width`: the card's keys and its fact. A
/// narrow bar first shortens every key's description, then gives up the fact's tail, then
/// the later keys; the first key always keeps its name.
fn selection_hint_text(
    hint: &crate::state::chrome::SelectionHint,
    prefix: &str,
    width: u16,
) -> String {
    let keys = |n: usize, long: bool| -> Vec<String> {
        hint.keys
            .iter()
            .take(n)
            .map(|(k, l, s)| format!("{} {}", k, if long { l } else { s }))
            .collect()
    };
    let join = |parts: Vec<String>| format!(" {}", parts.join(" · "));
    let with_fact = |mut parts: Vec<String>, fact: &str| {
        if !fact.is_empty() {
            parts.push(fact.to_string());
        }
        parts
    };
    // The fact is a state word, and after a colon the reason behind it: the reason is the
    // part a narrow bar gives up first.
    let word = hint.fact.split(':').next().unwrap_or_default().to_string();
    let n = hint.keys.len();
    let mut candidates = vec![
        join(with_fact(keys(n, true), &hint.fact)),
        join(with_fact(keys(n, false), &hint.fact)),
        join(with_fact(keys(n, false), &word)),
    ];
    for k in (1..n).rev() {
        candidates.push(join(with_fact(keys(k, false), &word)));
    }
    candidates.push(join(keys(1, false)));
    if n == 0 {
        candidates.push(format!(" {prefix}"));
    }
    crate::ui::switcher::fit(&candidates, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flash comes down on its own, so a refusal nobody answered stops holding the
    /// hint bar. Dropping it is a one-time change: the bar is already back afterwards.
    #[test]
    fn a_flash_comes_down_after_its_own_life() {
        let mut c = Chrome::default();
        c.flash("boom");
        let now = Instant::now();
        assert!(!c.expire_flash(now), "it has only just been shown");
        assert_eq!(c.flash, "boom");
        assert!(c.expire_flash(now + FLASH_TTL), "its life is over");
        assert!(c.flash.is_empty());
        assert!(
            !c.expire_flash(now + FLASH_TTL),
            "an empty bar changes nothing"
        );
    }

    /// A key that takes the flash down takes its deadline with it, so nothing is left to
    /// fire later at a bar the user already cleared.
    #[test]
    fn clearing_a_flash_leaves_nothing_to_expire() {
        let mut c = Chrome::default();
        c.flash("boom");
        c.clear_flash();
        assert!(!c.expire_flash(Instant::now() + FLASH_TTL));
    }

    #[test]
    fn a_selection_style_names_one_background() {
        // A selection surface IS a background, so a bare colour token is it; `bg=` is
        // accepted for symmetry with the other [ui] colour keys, and an `fg=` token is
        // not a surface and must not become one.
        assert_eq!(parse_selection_bg(""), None);
        assert_eq!(parse_selection_bg("   "), None);
        assert_eq!(parse_selection_bg("blue"), Some(Color::Blue));
        assert_eq!(parse_selection_bg("bg=blue"), Some(Color::Blue));
        assert_eq!(
            parse_selection_bg("#204060"),
            Some(Color::Rgb(0x20, 0x40, 0x60))
        );
        assert_eq!(
            parse_selection_bg("fg=red"),
            None,
            "a foreground is not a surface"
        );
        assert_eq!(
            parse_selection_bg("fg=red,bg=blue"),
            Some(Color::Blue),
            "the background wins wherever it sits in the list"
        );
    }

    #[test]
    fn parse_hint_bar_style_default_and_override() {
        let palette = crate::ui::palette::Palette::default();
        // Empty (and whitespace-only) ⇒ the built-in tmux default (yellowgreen / gray5).
        assert_eq!(
            parse_hint_bar_style("", &palette),
            hint_bar_default_style(&palette)
        );
        assert_eq!(
            parse_hint_bar_style("   ", &palette),
            hint_bar_default_style(&palette)
        );
        // bg=/fg= tokens set the two colours (tmux status-style syntax).
        let s = parse_hint_bar_style("bg=blue,fg=white", &palette);
        assert_eq!(s.bg, Some(Color::Blue));
        assert_eq!(s.fg, Some(Color::White));
        // A bare colour token is the foreground (tmux convention).
        assert_eq!(parse_hint_bar_style("red", &palette).fg, Some(Color::Red));
    }

    #[test]
    fn version_label_names_the_crate_and_its_version() {
        let c = Chrome::default();
        let label = c.version_label();
        assert_eq!(
            label,
            format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        );
        assert!(label.starts_with("xmux v"), "label: {label:?}");
    }

    #[test]
    fn hint_bar_shows_a_flash_over_an_open_input() {
        // A flash outranks an open input: a logout confirm Entered without the word
        // flashes while leaving the input open, so the flash must show; once it clears,
        // the bar rests again.
        use crate::ui::modal::{Input, InputMode, Modal};
        let mut c = Chrome::default();
        let state = crate::state::State {
            modal: Some(Modal::Input(Box::new(Input::new(
                InputMode::Filter,
                "xm".into(),
                None,
            )))),
            ..Default::default()
        };
        let t = c.hint_bar_text(60, &state);
        assert_eq!(
            t.trim(),
            "C-g",
            "the input says its keys where it is typed, so the bar rests: {t:?}"
        );
        // A flash displaces the input while it lasts.
        c.flash("type logout to confirm");
        let t2 = c.hint_bar_text(60, &state);
        assert!(
            t2.contains("type logout to confirm"),
            "the flash shows over the input: {t2:?}"
        );
        // The next key clears the flash and the bar rests again.
        c.flash.clear();
        let t3 = c.hint_bar_text(60, &state);
        assert_eq!(
            t3.trim(),
            "C-g",
            "the bar rests once the flash clears: {t3:?}"
        );
    }

    #[test]
    fn hint_bar_keeps_the_prefix_alone_at_rest_and_while_armed() {
        let mut c = Chrome::default();
        let mut state = crate::state::State::default();
        // At rest the logical text is the prefix alone.
        assert_eq!(c.hint_bar_text(80, &state).trim(), "C-g");
        // Armed, the key list beside the indicator names the keys, and the indicator
        // keeps the prefix alone, over the scan progress it would otherwise show.
        state.scanning.insert("local".into());
        c.set_armed(true);
        assert_eq!(c.hint_bar_text(400, &state).trim(), "C-g");
        // A flash outranks the armed prefix: a refusal must not be hidden by it.
        c.flash("host unreachable");
        assert!(c.hint_bar_text(120, &state).contains("host unreachable"));
    }

    #[test]
    fn the_selection_hint_names_its_keys_and_fact_and_shortens_to_fit() {
        let mut c = Chrome::default();
        let state = crate::state::State::default();
        let now = std::time::Instant::now();
        c.show_selection_hint(
            vec![
                (
                    "Enter".into(),
                    "focus terminal view".into(),
                    "terminal".into(),
                ),
                (
                    "C-g r".into(),
                    "rescan this machine".into(),
                    "rescan".into(),
                ),
            ],
            "unreachable: Connection refused".into(),
            now,
        );
        let wide = c.hint_bar_text(200, &state);
        assert_eq!(
            wide,
            " Enter focus terminal view · C-g r rescan this machine · unreachable: Connection refused"
        );
        // Shorter descriptions first, then the reason behind the state word, then keys.
        let short = c.hint_bar_text(70, &state);
        assert_eq!(
            short,
            " Enter terminal · C-g r rescan · unreachable: Connection refused"
        );
        let word = c.hint_bar_text(45, &state);
        assert_eq!(word, " Enter terminal · C-g r rescan · unreachable");
        let one = c.hint_bar_text(30, &state);
        assert_eq!(one, " Enter terminal · unreachable");
        let bare = c.hint_bar_text(16, &state);
        assert_eq!(bare, " Enter terminal", "a key keeps its name to the last");
        // It lasts three seconds, then the resting prefix returns.
        assert!(!c.expire_selection_hint(now + std::time::Duration::from_millis(2999)));
        assert!(c.selection_hint.is_some());
        assert!(c.expire_selection_hint(now + std::time::Duration::from_secs(3)));
        assert_eq!(c.hint_bar_text(200, &state).trim(), "C-g");
    }

    #[test]
    fn an_armed_prefix_outranks_the_selection_hint() {
        let mut c = Chrome::default();
        let state = crate::state::State::default();
        c.show_selection_hint(
            vec![(
                "Enter".into(),
                "focus the terminal".into(),
                "terminal".into(),
            )],
            "2 windows".into(),
            std::time::Instant::now(),
        );
        assert!(c.hint_bar_text(200, &state).contains("2 windows"));
        c.set_armed(true);
        assert_eq!(c.hint_bar_text(200, &state).trim(), "C-g");
    }

    #[test]
    fn flash_paints_the_error_style_not_the_status_style() {
        let mut c = Chrome::default();
        let palette = crate::ui::palette::Palette::default();
        assert_eq!(
            c.hint_bar_render_style(&palette),
            c.hint_bar_style,
            "with no flash the bar keeps the configured status style"
        );
        c.flash("cannot kill a host");
        assert_eq!(
            c.hint_bar_render_style(&palette),
            error_flash_style(&palette),
            "a refusal flash paints the distinct error style"
        );
        assert_ne!(
            error_flash_style(&palette),
            c.hint_bar_style,
            "the error style is visually distinct from the status style"
        );
    }

    #[test]
    fn map_color_named_and_default() {
        assert_eq!(map_color("green"), Color::Green);
        assert_eq!(map_color("blue"), Color::Blue);
        assert_eq!(map_color("yellow"), Color::Yellow);
        assert_eq!(map_color("white"), Color::White);
        assert_eq!(map_color("default"), Color::Reset);
        assert_eq!(
            map_color(""),
            Color::Reset,
            "empty = inherit/terminal default"
        );
        assert_eq!(map_color("brightblack"), Color::DarkGray);
    }

    #[test]
    fn map_color_indexed_and_hex() {
        assert_eq!(map_color("colour4"), Color::Indexed(4));
        assert_eq!(map_color("color12"), Color::Indexed(12));
        assert_eq!(map_color("#268bd2"), Color::Rgb(0x26, 0x8b, 0xd2));
    }

    #[test]
    fn resolve_layers_the_config_override_over_the_fixed_defaults() {
        // Unset → xmux's own pair, whatever host is displayed: the palette primary lit
        // against its disabled tone, the hover cue on the accent.
        let pal = crate::ui::palette::Palette::default();
        let d = ViewBorderColors::resolve_with_palette("", "", "", &pal);
        assert_eq!(d.active, pal.primary);
        assert_eq!(d.inactive, pal.disabled);
        assert_eq!(d.hover, pal.accent);
        assert_eq!(d, ViewBorderColors::default());

        // Each key overrides its own role and leaves the others at the default.
        let c = ViewBorderColors::resolve_with_palette("red", "", "cyan", &pal);
        assert_eq!(c.active, Color::Red);
        assert_eq!(c.inactive, pal.disabled);
        assert_eq!(c.hover, Color::Cyan);

        // The tmux colour syntax applies to the overrides (`default` = Reset).
        let c = ViewBorderColors::resolve_with_palette("fg=green", "default", "", &pal);
        assert_eq!(c.active, Color::Green);
        assert_eq!(c.inactive, Color::Reset);
    }

    #[test]
    fn map_color_tolerates_fg_prefix_and_case() {
        assert_eq!(
            map_color("fg=blue"),
            Color::Blue,
            "tmux style string drops in verbatim"
        );
        assert_eq!(
            map_color("  Blue "),
            Color::Blue,
            "trimmed and case-insensitive"
        );
        assert_eq!(map_color("fg=#EEE8D5"), Color::Rgb(0xee, 0xe8, 0xd5));
    }
}
