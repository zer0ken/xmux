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
use crate::state::{Chrome, FlashKind};
pub use crate::state::{SourceReach, ViewBorderColors};
use crate::ui::modal::{wrap_text, Modal};
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
/// The defaults are xmux's own and the same on every source: the palette's `primary`
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
/// error at a glance, not as more of the key cheatsheet. Every error flash paints
/// this. Fixed, not configurable: an error must stay legible regardless of any
/// `[ui] hint-bar-style` override.
pub(crate) fn error_flash_style(palette: &crate::ui::palette::Palette) -> Style {
    Style::default().bg(palette.error).fg(palette.bar_fg)
}

/// The hint bar's notice style: the bar's own background with its key accent as the
/// text. A notice tells the user something worth acting on (a newer release) without
/// anything having gone wrong, so it reads apart from the cheatsheet but never as the
/// error bar.
pub(crate) fn notice_flash_style(palette: &crate::ui::palette::Palette) -> Style {
    Style::default().bg(palette.bar_bg).fg(palette.bar_accent)
}

/// What a flash is about, which decides how the bar paints it.
/// How much of its row the hint bar paints.
///
/// At rest the bar is the prefix indicator, a label sized to what it says, so a column's
/// bottom row and a band's seam keep the rest of their cells. Arming the prefix takes the
/// whole row, because the cheatsheet has to be readable over whatever it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarFill {
    /// The whole rect: a solid bar. What an armed or flashing bar always uses.
    Row,
    /// The text plus a cell of padding, on its own background: the resting label.
    Content,
}

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
}

impl ViewScreen {
    /// The state word under the headline. The two SETTLED HOST states read theirs from
    /// the one source the nav cards read, so a card and the screen reached from it can
    /// never name the same state two ways; the self-session state is not a host state and
    /// names itself.
    fn word(self) -> &'static str {
        match self {
            ViewScreen::SelfSession => "running xmux",
            ViewScreen::Login => crate::ui::tree::host_state_word(false, true, false, true),
            ViewScreen::ListFailed => crate::ui::tree::host_state_word(false, false, true, true),
            ViewScreen::Unreachable => crate::ui::tree::host_state_word(false, false, false, true),
            ViewScreen::Empty => crate::ui::tree::host_state_word(false, false, false, false),
        }
    }
}

/// How many times in a row this source has failed, in words.
///
/// It separates a host that just dropped from one that has not answered all session -
/// two different problems that one error message reads identically for. No clock is
/// involved and none is wanted: the sweep re-probes every host every couple of seconds,
/// so a shown failure is always seconds old and an age row would say the same thing
/// every time it was read.
fn failure_run_words(runs: u32) -> String {
    match runs {
        0 | 1 => "first failure".to_string(),
        n => format!("{n} in a row"),
    }
}

/// The OTHER sources on `source`'s machine, each with what it last answered, in the
/// inventory's own order.
///
/// A machine serving several muxes gets one source per mux, and they fail
/// independently: this is what says whether the machine or the mux is the thing that is
/// down. Empty when the machine serves this source alone, and then the screen carries no
/// such row rather than an empty one.
fn siblings(
    state: &crate::state::State,
    source: &str,
    label: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let machine = crate::session::machine_of(source);
    state
        .groups
        .iter()
        .filter(|g| g.source != source && crate::session::machine_of(&g.source) == machine)
        .map(|g| {
            let failure = g.failure();
            let word = if state.scanning.contains(&g.source) {
                "still scanning".to_string()
            } else if g.err.is_some() {
                crate::ui::tree::host_state_word(
                    false,
                    failure == Some(crate::model::FailureKind::Blocked),
                    failure == Some(crate::model::FailureKind::ListFailed),
                    true,
                )
                .to_string()
            } else {
                match g.sessions.len() {
                    0 => crate::ui::tree::host_state_word(false, false, false, false).to_string(),
                    1 => "1 session".to_string(),
                    n => format!("{n} sessions"),
                }
            };
            format!("{} · {word}", label(&g.source))
        })
        .collect()
}

/// How xmux reaches one source, in the words the unreachable screen prints.
///
/// Resolved once at startup from that source's own config, because how a source is
/// REACHED cannot change under a run - only whether it answers can. Every field is
/// already words: the screen prints them and nothing branches on any of them, which is
/// what keeps this layer blind to which machine kind or which mux a source is.
/// The left cell of a host-screen row: what the row is about, and how it reads.
enum ScreenCell {
    /// A key the user can press on this screen. Bold and nothing else, the help modal's
    /// own key column, so a key reads as a key wherever it is offered.
    Key(String),
    /// The name of the datum beside it, muted so the datum itself reads first.
    Label(&'static str),
    /// No cell: the value continues the row above, hanging under the same rule, so a
    /// multi-line value stays one row rather than becoming a run of nameless ones.
    Continued,
    /// No row at all - the blank line parting two blocks of them.
    Gap,
}

impl ScreenCell {
    fn text(&self) -> &str {
        match self {
            ScreenCell::Key(k) => k,
            ScreenCell::Label(l) => l,
            ScreenCell::Continued | ScreenCell::Gap => "",
        }
    }

    fn style(&self, palette: &crate::ui::palette::Palette) -> Style {
        match self {
            ScreenCell::Key(_) => crate::ui::palette::interaction_key_style(),
            ScreenCell::Label(_) => Style::default().fg(palette.decoration),
            ScreenCell::Continued | ScreenCell::Gap => Style::default(),
        }
    }
}

impl Default for Chrome {
    fn default() -> Self {
        let palette = crate::ui::palette::Palette::default();
        Chrome {
            flash: String::new(),
            flash_until: None,
            flash_kind: FlashKind::Error,
            auto_hide: false,
            view_border_hovered: false,
            spinner: HashSet::new(),
            spinner_frame: 0,
            login_defaults: HashMap::new(),
            ssh_stanzas: HashMap::new(),
            roster_providers: HashMap::new(),
            source_reach: HashMap::new(),
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
    /// host or mux, and the hint bar style (`[ui] hint-bar-style`, else the tmux default).
    pub(crate) fn apply_palette(
        &mut self,
        ui: &crate::provision::config::UiConfig,
        palette: &crate::ui::palette::Palette,
    ) {
        self.colors = ViewBorderColors::resolve_with_palette(
            &ui.view_active_border_style,
            &ui.view_border_style,
            &ui.view_border_hover_style,
            palette,
        );
        self.hint_bar_style = parse_hint_bar_style(&ui.hint_bar_style, palette);
    }

    /// The rule between the tree and the terminal view. The whole rule uses the active
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
        // Band layout: the view border runs horizontally between the nav band and the
        // terminal. It uses one colour across its full length, like the vertical rule.
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
        let glyph = if self.auto_hide { "║" } else { "│" };
        // Hover (mouse over the rule, no button): box-drawing rules have no bold form
        // (the BOLD modifier does not thicken them), so swap the glyph itself to the
        // HEAVY vertical (┃) for a genuinely thicker line and recolour it with the
        // configured hover colour (`[ui] view-border-hover-style`) - same single rule,
        // just thicker + lit, as the grab cue.
        if self.view_border_hovered {
            let style = Style::default().fg(self.colors.hover);
            let bars = Text::from(
                (0..area.height)
                    .map(|_| Line::from(Span::styled("┃", style)))
                    .collect::<Vec<_>>(),
            );
            frame.render_widget(Paragraph::new(bars), area);
            return;
        }
        let bars = Text::from(
            (0..area.height)
                .map(|_| Line::from(Span::styled(glyph, Style::default().fg(color))))
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Paragraph::new(bars), area);
    }

    /// Thickens the stretch of a side nav's seam beside the cards on screen when the list
    /// overflows: the seam's own heavy glyph in the seam's own colour, so the overflow is
    /// read off the one line the nav draws. The hover cue already thickens the whole seam,
    /// so the stretch is not drawn over it.
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
    /// One screen, two states, so a reader of either reads the other: the host's name as
    /// the headline, under it the same status word its nav card carries, then the rows
    /// that apply to it. A row is the help modal's row borrowed whole - a right-aligned
    /// left cell, the `│` rule, the value - so a key offered on a screen looks like a key
    /// offered anywhere else, and a datum's name stays quieter than the datum.
    pub(crate) fn render_view_screen(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        view: ViewScreenRender<'_>,
        palette: &crate::ui::palette::Palette,
    ) {
        let lines = self.view_screen_lines(
            state,
            view.address,
            view.kind,
            area.width,
            view.focused,
            palette,
        );
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    /// The name a view screen carries at its top, in the grammar the nav cards use:
    /// `{host}/{mux}` for a host's screen, and that with the session under it for the
    /// session xmux is itself running in. What arrives is the [`crate::session::Address`]
    /// the screen was reached by, which is the source id and, for the session screen, its
    /// session name - the two halves are already separate, so nothing is re-split.
    fn headline(&self, address: &crate::session::Address, kind: ViewScreen) -> String {
        if address.source.is_empty() {
            return String::new();
        }
        match kind {
            ViewScreen::SelfSession => {
                if address.session.is_empty() {
                    self.source_label(&address.source)
                } else {
                    format!(
                        "{}{}{}",
                        self.source_label(&address.source),
                        crate::session::MUX_LABEL_SEP,
                        address.session
                    )
                }
            }
            // An EMPTY host answered - it has no session, which is itself an answer
            // through its mux - so its screen names the pair. An unreachable or blocked
            // host answered nothing, so its screen reads the host alone unless the id
            // names the mux. A listing failure keeps the confirmed mux that answered.
            ViewScreen::Unreachable
            | ViewScreen::Login
            | ViewScreen::ListFailed
            | ViewScreen::Empty => {
                self.source_label_when(&address.source, matches!(kind, ViewScreen::Empty))
            }
        }
    }

    /// The lines of [`render_view_screen`](Self::render_view_screen). Split out because
    /// the layout IS the list of rows: both states build one, so neither can drift into a
    /// paragraph of its own shape.
    fn view_screen_lines(
        &self,
        state: &crate::state::State,
        address: &crate::session::Address,
        kind: ViewScreen,
        width: u16,
        focused: bool,
        palette: &crate::ui::palette::Palette,
    ) -> Vec<Line<'static>> {
        let pal = palette;
        let p = &self.ui_prefix;
        let source = address.source.as_str();
        // The rows in reading order: WHY the state is what it is, then what to press
        // about it. An unreachable host's why is the reason its own transport gave plus
        // the ssh stanza it was reached through, which is what a fix needs; a reachable
        // empty host has no why, so its screen is the keys alone.
        let mut rows: Vec<(ScreenCell, String)> = Vec::new();
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
            // its own rule (see below), because a datum the user came here to read is
            // worth more than a tidy column.
            let login_report = state.login_reports.get(crate::session::machine_of(source));
            let reason = login_report
                .and_then(|report| report.connect.reason().map(str::to_string))
                .or_else(|| {
                    state
                        .groups
                        .iter()
                        .find(|g| g.source == source)
                        .and_then(|g| g.err.clone())
                })
                .unwrap_or_else(|| "connection closed".into());
            rows.push((ScreenCell::Label("reason"), reason));
            if let Some(registration) = state
                .registration_reports
                .get(crate::session::machine_of(source))
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
            if let Some(runs) = state.failure_runs.get(source) {
                rows.push((ScreenCell::Label("failures"), failure_run_words(*runs)));
            }
            rows.push((ScreenCell::Gap, String::new()));
            // What was asked, and of what. The mux and the machine are separate rows
            // because they are the two independent things that can be wrong: the box may
            // be up with no such mux on it, or the mux fine behind a box that cannot be
            // reached.
            if let Some(reach) = self.source_reach.get(source) {
                if !reach.mux.is_empty() {
                    rows.push((ScreenCell::Label("mux"), reach.mux.clone()));
                }
                if !reach.machine.is_empty() {
                    rows.push((ScreenCell::Label("machine"), reach.machine.clone()));
                }
                if !reach.socket.is_empty() {
                    rows.push((ScreenCell::Label("socket"), reach.socket.clone()));
                }
                if !reach.probe.is_empty() {
                    rows.push((ScreenCell::Label("probe"), reach.probe.clone()));
                }
            }
            // WHERE this host came from, between what failed and how it is configured. A
            // host that fails is worth nothing if the user cannot tell why it is on the
            // list at all: a tailnet peer they never wrote down reads as a mystery until
            // the row names the provider that offered it, which is also the provider
            // they would turn off.
            if let Some(provider) = self
                .roster_providers
                .get(crate::session::machine_of(source))
            {
                rows.push((ScreenCell::Label("provider"), provider.clone()));
            }
            let stanza = self
                .ssh_stanzas
                .get(crate::session::machine_of(source))
                .map(String::as_str)
                .unwrap_or_default();
            if stanza.is_empty() {
                rows.push((
                    ScreenCell::Label("ssh config"),
                    "(no matching entry)".into(),
                ));
            } else {
                for (i, l) in stanza.lines().enumerate() {
                    let cell = if i == 0 {
                        ScreenCell::Label("ssh config")
                    } else {
                        ScreenCell::Continued
                    };
                    rows.push((cell, l.trim_end().to_string()));
                }
            }
            // The other muxes on the SAME machine, each with what it answered. This is
            // the one row that tells the user which half is broken without leaving the
            // screen: a sibling serving sessions says the box is up and this mux is not.
            for (i, sib) in siblings(state, source, &|s| self.source_label(s))
                .into_iter()
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
        } else {
            if let Some(registration) = state
                .registration_reports
                .get(crate::session::machine_of(source))
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
                    rows.push((ScreenCell::Gap, String::new()));
                }
            }
            // Creating under an unreachable host is refused, so `n` is offered only where
            // it can actually run.
            rows.push((
                ScreenCell::Key(format!("{p} n")),
                "start a new session".into(),
            ));
        }
        if kind != ViewScreen::SelfSession {
            rows.push((
                ScreenCell::Key(format!("{p} r")),
                "re-scan every host".into(),
            ));
        }

        // One column width for keys and labels alike: every row of a screen meets the
        // same rule, whichever kind of cell it carries.
        let cw = rows
            .iter()
            .map(|(c, _)| c.text().chars().count())
            .max()
            .unwrap_or(0);
        // A value too wide for its column hangs under the SAME rule rather than
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

        let rule = Span::styled("│ ", Style::default().fg(pal.decoration));
        let state_style = Style::default().fg(match kind {
            ViewScreen::Unreachable => pal.error,
            ViewScreen::Login => pal.warning,
            ViewScreen::ListFailed => pal.primary,
            ViewScreen::Empty | ViewScreen::SelfSession => pal.decoration,
        });
        let headline = format!(" {}", self.headline(address, kind));
        let mut out = vec![
            Line::from(""),
            Line::from(Span::styled(
                headline,
                Style::default()
                    .fg(pal.secondary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(format!(" {}", kind.word()), state_style)),
        ];
        // The login pane OWNS the connection values: they sit at the panel's top,
        // edited in place from the terminal view (no modal, no nav). The focused element
        // shows a cursor only while the terminal view is focused, so the pane says
        // whether it is taking keys.
        if kind == ViewScreen::Login {
            let running = state.login_run.as_ref().is_some_and(|l| l.source == source);
            let defaults = self.login_defaults(source);
            let draft = state.login.as_ref().filter(|d| d.source == source);
            let fallback = crate::state::LoginDraft {
                address: defaults.address.value.clone(),
                port: defaults.port.value.clone(),
                username: defaults.username.value.clone(),
                default_address: defaults.address.value.clone(),
                default_port: defaults.port.value.clone(),
                default_username: defaults.username.value.clone(),
                ..Default::default()
            };
            let d = draft.unwrap_or(&fallback);
            // No cursor while the login runs: the pane takes no keys then, and a cursor
            // would say it does.
            let cursor = |active: bool| {
                if active && focused && !running {
                    "▊"
                } else {
                    ""
                }
            };
            let label = |text: String| {
                Span::styled(
                    format!(" {text:>cw$} "),
                    Style::default().fg(pal.decoration),
                )
            };
            // A field carries its own emptiness: a required one is marked in its label,
            // and an optional one says so in the space its value would occupy, so the
            // pane never needs a legend to be read.
            let field = |name: &str,
                         required: bool,
                         value: &str,
                         mask: bool,
                         active: bool,
                         provenance: &str| {
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
                Line::from(vec![
                    label(format!("{name}{}", if required { "*" } else { "" })),
                    rule.clone(),
                    Span::styled(
                        format!(
                            "{text}{}{}",
                            cursor(active),
                            if provenance.is_empty() {
                                String::new()
                            } else {
                                format!("  {provenance}")
                            }
                        ),
                        style,
                    ),
                ])
            };
            let choice = |name: &str, mark: &str, text: &str, active: bool| {
                Line::from(vec![
                    label(name.to_string()),
                    rule.clone(),
                    Span::styled(
                        format!(
                            "{mark}{}{text}{}",
                            if mark.is_empty() { "" } else { " " },
                            cursor(active)
                        ),
                        if active {
                            Style::default().fg(pal.secondary)
                        } else {
                            Style::default().fg(pal.decoration)
                        },
                    ),
                ])
            };
            use crate::state::{LoginFocus, Remember};
            let provenance = |value: &str, original: &str, resolved: &'static str| {
                if value == original {
                    resolved
                } else {
                    "edited"
                }
            };
            out.push(Line::from(""));
            out.push(field(
                "address",
                true,
                &d.address,
                false,
                d.focus == LoginFocus::Address,
                provenance(&d.address, &d.default_address, defaults.address.provenance),
            ));
            out.push(field(
                "port",
                true,
                &d.port,
                false,
                d.focus == LoginFocus::Port,
                provenance(&d.port, &d.default_port, defaults.port.provenance),
            ));
            out.push(field(
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
            ));
            out.push(field(
                "password",
                false,
                &d.password,
                true,
                d.focus == LoginFocus::Password,
                "",
            ));
            // The remember choice appears only once a value differs from what ssh would
            // have used: a stanza repeating what ssh already resolves records nothing.
            if d.changed() {
                out.push(Line::from(""));
                let pick = |on: bool| if on { "(•)" } else { "( )" };
                out.push(choice(
                    "remember",
                    pick(d.remember == Remember::Nothing),
                    "nothing",
                    d.focus == LoginFocus::RememberNothing,
                ));
                out.push(choice(
                    "",
                    pick(d.remember == Remember::SshConfig),
                    "write address, port, username to ssh config",
                    d.focus == LoginFocus::RememberSshConfig,
                ));
            }
            out.push(Line::from(""));
            out.push(choice(
                "pubkey",
                if d.pubkey { "[x]" } else { "[ ]" },
                "register my public key on this host",
                d.focus == LoginFocus::Pubkey,
            ));
            out.push(Line::from(""));
            // A login under way replaces the button it was started from. The pane keeps
            // every value, so what the user sees is the thing they submitted, still
            // theirs, with the one thing they can now say about it.
            if running {
                out.push(choice("", "", "logging in…  esc to stop", false));
            } else {
                out.push(choice("", "", "[ login ]", d.focus == LoginFocus::Submit));
            }
        }
        out.push(Line::from(""));
        for (cell, value) in rows {
            if matches!(cell, ScreenCell::Gap) {
                out.push(Line::from(""));
                continue;
            }
            out.push(Line::from(vec![
                Span::styled(format!(" {:>cw$} ", cell.text()), cell.style(palette)),
                rule.clone(),
                Span::raw(value),
            ]));
        }
        out
    }

    /// The hint bar's logical text, fit to `width`. At rest this text is only the prefix,
    /// the nav's prefix indicator. Once the prefix
    /// is armed, the text becomes the list of keys that prefix unlocks. An
    /// open input outranks everything: the bar BECOMES the input line (feature name,
    /// guide text, and the windowed buffer), so what is being typed is what the bar
    /// says. The transient states outrank the rest, in order: a flash (a refusal),
    /// the scan progress, then the active filter. A flash is returned raw - it may
    /// exceed `width`; [`Self::hint_bar_lines`] wraps it so it never clips.
    pub(crate) fn hint_bar_text(&self, width: u16, state: &crate::state::State) -> String {
        // Use the active prefix so the hint_bar matches the user's configured binding.
        let p = &self.ui_prefix;
        if !self.flash.is_empty() {
            // A flash outranks even an open input: a dead jump number flashed its range
            // while leaving the input open, so the range must show over the input line.
            match self.flash_kind {
                FlashKind::Error => format!(" ✗ {}", self.flash),
                FlashKind::Notice => format!(" {}", self.flash),
            }
        } else if let Some(Modal::Input(input)) = &state.modal {
            crate::ui::modal::input_hint_text(input, width)
        } else if self.armed {
            // The prefix is held: name what it unlocks. Longest-first so a narrow nav
            // drops the rarer chords rather than clipping mid-word.
            // Order: focus nav, focus terminal, jump, new, filter, hide, collapse, position,
            // rescan, help, quit. The focus rows name the arrow PAIR the current placement makes
            // active (the pair facing the terminal's side names the terminal), and the
            // resize keys are left out (the help modal has them).
            let focus = if self.nav_position.forward_arrows_face_terminal() {
                "←/↑ focus nav · →/↓ focus terminal"
            } else {
                "→/↓ focus nav · ←/↑ focus terminal"
            };
            fit(
                &[
                    format!(" {p} · {focus} · 1-9 jump to a session · n new session · / filter · t hide nav · z collapse nav · p nav position · r rescan · ? help · q quit"),
                    format!(" {p} · {focus} · 1-9 jump to · n new · / filter · t hide · z collapse · p position · r rescan · ? help · q quit"),
                    format!(" {p} · {focus} · 1-9 · n · / · t · z · p · r · ? · q"),
                    format!(" {p} · ←/↑ · →/↓ · 1-9 · n · / · t · z · p · r · ? · q"),
                    format!(" {p}…"),
                ],
                width,
            )
        } else if !state.scanning.is_empty() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the resting prefix) once every host has settled. It
            // turns the SAME spinner the scanning cards do, on the same frame, so the
            // bar and the cards read as one thing still loading.
            let total = state.groups.len();
            let done = total.saturating_sub(state.scanning.len());
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
            // The active filter has no border title to live in any more, so it
            // shows in the hint_bar (with how to clear it).
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
        if self.flash.is_empty() {
            return vec![text];
        }
        wrap_text(text.trim_start(), width.saturating_sub(1))
            .into_iter()
            .map(|l| format!(" {l}"))
            .collect()
    }

    /// The style the hint bar paints with this frame. While a flash is showing it is
    /// the [`error_flash_style`] for an error or the [`notice_flash_style`] for a
    /// notice; otherwise the configured status style. Split from
    /// [`Self::render_hint_bar`] so the choice is unit-testable without a backend.
    pub(crate) fn hint_bar_render_style(&self, palette: &crate::ui::palette::Palette) -> Style {
        if self.flash.is_empty() {
            self.hint_bar_style
        } else {
            match self.flash_kind {
                FlashKind::Error => error_flash_style(palette),
                FlashKind::Notice => notice_flash_style(palette),
            }
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
    ) -> Line<'static> {
        // The bar's OWN accent, not the card accent: the keys sit on `bar_bg`, a
        // surface the card accent may not read on (see `Palette::bar_accent`). The keys
        // are also BOLD, so a key reads as a key wherever it is offered (the help modal's
        // key column and the host-screen rows are bold the same way).
        let accent = crate::ui::palette::interaction_key_style().fg(palette.bar_accent);
        let sep_style = Style::default().fg(palette.decoration);
        let mut spans: Vec<Span> = Vec::new();
        for (i, seg) in line.split(" · ").enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", sep_style));
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

    /// The version the expanded bar pins to its far right: `xmux v<version>`, built from the
    /// crate's own name and version so it always matches what `xmux --version` reports.
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
        // An open input owns the bar outright: the bar BECOMES the input line (see
        // [`Self::hint_bar_text`]), painted as the status bar with a reversed-block
        // caret. A flash outranks it - a dead jump number flashes its range while
        // leaving the input open, so the range must show over the input line - and
        // falls through to the flash path below, exactly as [`Self::hint_bar_text`]
        // orders them.
        if self.flash.is_empty() {
            if let Some(Modal::Input(input)) = &state.modal {
                let line = crate::ui::modal::input_hint_line(input, area.width, palette);
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new(line).style(self.hint_bar_render_style(palette)),
                    area,
                );
                return;
            }
        }
        // While the prefix is HELD the bar is expanded, and it pins its name and version to
        // the far right: a cheap build pointer that never crowds the cheatsheet. A flash is
        // a refusal and must own the whole row, so it displaces the version. The version
        // only appears on a solid (Row) bar, which is exactly what an armed bar always is.
        let version = if self.armed && self.flash.is_empty() && fill == BarFill::Row {
            let label = self.version_label();
            let gap = 2; // a two-cell breathing room between the cheatsheet and the label
            (label, gap)
        } else {
            (String::new(), 0)
        };
        let right_margin = 1; // a one-cell margin between the label and the far right edge
        let version_w = version.0.chars().count() as u16 + version.1 + right_margin;
        let text_w = area.width.saturating_sub(version_w);
        let lines = self.hint_bar_lines(text_w, state);
        // Key tokens get the accent only on the built-in default style with no flash
        // showing: a `[ui] hint-bar-style` override keeps its exact colours (uniform,
        // as configured), and a flash keeps the one solid style of its kind.
        let width = lines
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let styled =
            self.flash.is_empty() && self.hint_bar_style == hint_bar_default_style(palette);
        let text = if styled {
            Text::from(
                lines
                    .into_iter()
                    .map(|l| self.hint_bar_line_spans(l, palette))
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
        // An armed bar floats over the live grid, so without this the grid's own
        // characters survive in the columns the bar's text does not reach and the bar
        // reads as text spilled across the screen instead of a bar covering it.
        let painted = match fill {
            BarFill::Row => area,
            BarFill::Content => Self::bar_content_rect(area, width),
        };
        frame.render_widget(Clear, painted);
        // The cheatsheet takes the left of the bar; the version the rightmost cells. The
        // cheatsheet was fit to `text_w`, so painting it across the whole bar fills the gap
        // with the status background while the label sits clear of the text at the right.
        frame.render_widget(
            Paragraph::new(text).style(self.hint_bar_render_style(palette)),
            painted,
        );
        if !version.0.is_empty() {
            let vw = version.0.chars().count() as u16;
            let vrect = Rect {
                x: painted.x + painted.width.saturating_sub(vw + right_margin),
                y: painted.y,
                width: vw,
                height: painted.height,
            };
            let white = Style::default().fg(Color::White);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(version.0, white)))
                    .style(self.hint_bar_render_style(palette)),
                vrect,
            );
        }
    }

    /// Paints the resting prefix across a collapsed nav's indicator. Transient bars are
    /// handled by the ordinary floating-bar path instead.
    pub(crate) fn render_collapsed_hint_bar(
        &self,
        frame: &mut Frame,
        area: Rect,
        palette: &crate::ui::palette::Palette,
    ) {
        frame.render_widget(Clear, area);
        let line = self.hint_bar_line_spans(format!(" {}", self.ui_prefix), palette);
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
    /// A notice is information, not a failure: it has a flash's life but paints the
    /// notice style without the `✗` mark, while an error keeps both.
    #[test]
    fn a_notice_paints_apart_from_an_error() {
        let state = crate::state::State::default();
        let mut c = Chrome::default();
        let palette = crate::ui::palette::Palette::default();
        c.notice("xmux 9.9.9 is available");
        assert_eq!(
            c.hint_bar_render_style(&palette),
            notice_flash_style(&palette)
        );
        let text = c.hint_bar_text(80, &state);
        assert!(!text.contains('✗'), "{text:?}");
        assert!(text.contains("xmux 9.9.9 is available"), "{text:?}");
        assert!(
            c.expire_flash(Instant::now() + FLASH_TTL),
            "a notice has the same life"
        );

        c.flash("boom");
        assert_eq!(
            c.hint_bar_render_style(&palette),
            error_flash_style(&palette)
        );
        assert!(c.hint_bar_text(80, &state).contains('✗'));
        assert_ne!(notice_flash_style(&palette), error_flash_style(&palette));
    }

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
        // A flash outranks an open input: a dead jump number flashes its range while
        // leaving the input open, so the range must show over the input line; once the
        // flash clears, the input line takes the bar back.
        use crate::ui::modal::{Input, InputMode, Modal};
        let mut c = Chrome::default();
        let state = crate::state::State {
            modal: Some(Modal::Input(Box::new(Input::new(
                InputMode::Filter,
                " filter sessions".into(),
                "xm".into(),
                None,
            )))),
            ..Default::default()
        };
        let t = c.hint_bar_text(60, &state);
        assert!(
            t.contains("[filter] filter sessions: xm"),
            "the bar reads the input line: {t:?}"
        );
        // A flash displaces the input while it lasts.
        c.flash("no session 9 (1 - 4)");
        let t2 = c.hint_bar_text(60, &state);
        assert!(
            t2.contains("no session 9 (1 - 4)"),
            "the flash shows over the input: {t2:?}"
        );
        // The next key clears the flash and the input line returns.
        c.flash.clear();
        let t3 = c.hint_bar_text(60, &state);
        assert!(
            t3.contains("[filter] filter sessions"),
            "the input line returns once the flash clears: {t3:?}"
        );
    }

    #[test]
    fn hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed() {
        let mut c = Chrome::default();
        let state = crate::state::State::default();
        // At rest the logical text is the prefix alone.
        assert_eq!(c.hint_bar_text(80, &state).trim(), "C-g");
        // Armed: the keys the prefix unlocks. Wide enough for the full descriptions,
        // the rows run in the bar's fixed order (focus nav, focus terminal, jump, new,
        // filter, hide, collapse, position, rescan, help, quit) and the focus rows use arrow
        // symbols that
        // point at the view they focus.
        c.set_armed(true);
        let full = c.hint_bar_text(400, &state);
        assert!(full.starts_with(" C-g "), "{full:?}");
        let order = [
            "←/↑ focus nav",
            "→/↓ focus terminal",
            "1-9 jump to a session",
            "n new session",
            "/ filter",
            "t hide nav",
            "z collapse nav",
            "p nav position",
            "r rescan",
            "? help",
            "q quit",
        ];
        let mut last = 0;
        for seg in order {
            let pos = full
                .find(seg)
                .unwrap_or_else(|| panic!("armed bar lists {seg:?}: {full:?}"));
            assert!(
                pos > last,
                "armed bar order keeps {seg:?} after the previous: {full:?}"
            );
            last = pos;
        }
        // A narrower bar drops to short descriptions while keeping the focus guidance
        // (the pair segment rides every rung). The full line is ~166 cells, so a 145-wide
        // bar forces the middle rung, whose focus rows keep the full pair wording.
        let armed = c.hint_bar_text(145, &state);
        assert!(
            armed.contains("→/↓ focus terminal"),
            "short bar keeps focus-terminal: {armed:?}"
        );
        for key in [
            "n new",
            "/ filter",
            "z collapse",
            "r rescan",
            "? help",
            "q quit",
        ] {
            assert!(armed.contains(key), "armed bar lists {key:?}: {armed:?}");
        }
        // A flash outranks the armed cheatsheet: a refusal must not be hidden by it.
        c.flash("host unreachable");
        assert!(c.hint_bar_text(120, &state).contains("host unreachable"));
    }

    #[test]
    fn the_armed_cheatsheet_names_the_arrow_pair_the_placement_makes_active() {
        let mut c = Chrome::default();
        let state = crate::state::State::default();
        c.set_armed(true);
        // Default (left column): ←/↑ name the nav, →/↓ the terminal.
        let left = c.hint_bar_text(400, &state);
        assert!(left.contains("←/↑ focus nav"), "{left:?}");
        assert!(left.contains("→/↓ focus terminal"), "{left:?}");
        // Pinned right, the whole pair mirrors and the bar says so.
        c.set_nav_position(crate::ui::switcher::NavPosition::Right);
        let right = c.hint_bar_text(400, &state);
        assert!(right.contains("→/↓ focus nav"), "{right:?}");
        assert!(right.contains("←/↑ focus terminal"), "{right:?}");
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
        // Unset → xmux's own pair, whatever source is displayed: the palette primary lit
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
