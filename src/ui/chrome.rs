//! The switcher's chrome: the tree|terminal view border, the full-width hint_bar
//! (help / status / wrapped flash), and the host screens that fill the terminal-view
//! region in place of a mux. These own the view-local presentation state ([`Chrome`])
//! and read the runtime inventory from `State`; the
//! [`Switcher`](crate::ui::switcher::Switcher) holds a [`Chrome`] and delegates these
//! draws to it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewBorderColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum FlashKind {
    /// A refused action or a failure: the error bar and the `⚠` mark.
    #[default]
    Error,
    /// Information that is not a failure: the notice style and no mark.
    Notice,
}

/// How much of its row the hint bar paints.
///
/// The bar is a status bar where it owns its row, and a label where it does not: the
/// portrait band's bar shares one row with the horizontal scrollbar, so at rest it paints
/// its glyphs alone and leaves the thumb showing. Arming the prefix takes the whole row
/// back, because the cheatsheet has to be readable over whatever it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarFill {
    /// The whole rect: a solid bar. What an armed or flashing bar always uses, and what
    /// the side column's own status row uses.
    Row,
    /// The text plus a cell of padding, on its own background: the portrait band's bar
    /// shares its row with the horizontal scrollbar, so it takes only the cells it needs
    /// and leaves the rest of the row to the thumb.
    Content,
}

/// The mark a BLOCKED host wears on its nav card, flush after the host name. A blocked
/// host is a failure the user can act on (the login pane), so it keeps the warning
/// colour like the unreachable `⚠`. One column wide: a card's columns are laid out in
/// cells, and a wide glyph here would shift every column after it.
pub(crate) const BLOCK_MARK: &str = "?";

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
            ViewScreen::Login => crate::ui::tree::host_state_word(true, true),
            other => crate::ui::tree::host_state_word(false, other == ViewScreen::Unreachable),
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
            let blocked = g.failure() == Some(crate::model::FailureKind::Blocked);
            let word = if state.scanning.contains(&g.source) {
                "still scanning".to_string()
            } else if g.err.is_some() {
                crate::ui::tree::host_state_word(blocked, true).to_string()
            } else {
                match g.sessions.len() {
                    0 => crate::ui::tree::host_state_word(false, false).to_string(),
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceReach {
    /// The command a session listing spawns, spelled so it can be run by hand. The one
    /// datum that turns "it failed" into something the user can reproduce outside xmux.
    pub probe: String,
    /// The machine and how it is addressed, with the connect budget that applies to it.
    pub machine: String,
    /// The mux binary asked for on that machine.
    pub mux: String,
    /// What that mux is CALLED, which is the name every surface shows: the binary above is
    /// what was asked for, and the two part company wherever a binary is an alias or a
    /// path. One spelling, so a card and the screen reached from it cannot name one mux two
    /// ways.
    pub kind: String,
    /// The socket / ControlMaster path the mux is addressed through. Empty ⇒ no row,
    /// which is the honest answer for a machine addressed without one.
    pub socket: String,
}

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
            ScreenCell::Key(_) => Style::default().add_modifier(Modifier::BOLD),
            ScreenCell::Label(_) => Style::default().fg(palette.decoration),
            ScreenCell::Continued | ScreenCell::Gap => Style::default(),
        }
    }
}

/// How long a flash stays up with nothing pressed. A refusal is about something that
/// already happened, so a bar holding one forever keeps the nav's own help text off
/// screen over a message that has stopped being news. Ten seconds reads a wrapped line
/// twice over.
pub(crate) const FLASH_TTL: Duration = Duration::from_secs(10);

/// The switcher's chrome view state: the view border/hint_bar/host-screen draws and
/// their inputs (flash, spinner set + frame, auto-hide + hover cues, view border
/// colours, the ssh-config text, the configured prefix string, whether the prefix is
/// currently armed, and the hint bar style).
pub struct Chrome {
    pub(crate) flash: String,
    /// When the flash stops showing itself, or `None` when nothing is flashing.
    flash_until: Option<Instant>,
    /// Whether the current flash is an error or a notice.
    flash_kind: FlashKind,
    /// Auto-hide-tree mode (set by the app each frame). Drives the view border glyph:
    /// ║ (double) when on, │ (single) when off - the only on-screen cue, since while
    /// the mode is on but the tree is focused the tree still shows.
    pub(crate) auto_hide: bool,
    /// True while the mouse is hovering the view border rule - the app sets this from
    /// idle motion so the view border highlights as a grab cue for drag-resize.
    pub(crate) view_border_hovered: bool,
    /// Session addresses currently connecting / awaiting first output - a braille
    /// spinner glyph renders right of their name in the tree.
    pub(crate) spinner: HashSet<String>,
    pub(crate) spinner_frame: usize,
    login_defaults: HashMap<String, crate::provision::env::LoginDefaults>,
    ssh_stanzas: HashMap<String, String>,
    /// What offered each host to the roster, keyed by HOST name and already reduced to
    /// the words to print (set once by the app). The unreachable host screen names it.
    /// Empty in tests, where the row is then absent rather than blank.
    pub(crate) roster_providers: HashMap<String, String>,
    /// How xmux reaches each source, keyed by SOURCE id (set once by the app). The
    /// unreachable screen states it: a host that failed is worth little without what was
    /// asked of it and how. See [`SourceReach`].
    pub(crate) source_reach: HashMap<String, SourceReach>,
    /// The log file every dispatched command and its result is written to (set once by the
    /// app). The unreachable screen names the path, so the full history of what was run
    /// is findable rather than being something the user has to know about.
    pub(crate) log_path: String,
    /// The human-readable prefix string (e.g. `"C-g"`, `"C-Space"`) - set once by
    /// the app from config so the help modal reflects the active binding.
    pub(crate) ui_prefix: String,
    /// True while the prefix has been pressed and the app is waiting for the command
    /// key (set by the app each frame from the live input state, in either focus). The
    /// resting hint bar shows the prefix and collapse button until this flips, then the
    /// floating bar shows the keys it unlocks. The cheatsheet appears exactly when it is
    /// needed and never competes with the cards for room.
    pub(crate) armed: bool,
    /// The side the nav is attached to this frame (set by the app each frame from the
    /// runtime's resolved position). The cheatsheet's focus segment names the arrow
    /// pair the placement makes active.
    pub(crate) nav_position: crate::ui::switcher::NavPosition,
    /// The tree|terminal view border colours (set once by the app from config; tmux defaults
    /// otherwise). See [`ViewBorderColors`].
    pub(crate) colors: ViewBorderColors,
    /// The hint bar's style (set once by the app from `[ui] hint-bar-style`; the tmux
    /// default otherwise). See [`hint_bar_default_style`].
    pub(crate) hint_bar_style: Style,
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

    /// Sets the transient error flash shown in the nav's hint bar. The next tree key
    /// clears it (the switcher's `handle_key`), and [`FLASH_TTL`] clears it for a user
    /// who presses nothing, so the normal help/status hint bar returns either way.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.show_flash(msg.into(), FlashKind::Error);
    }

    /// Sets a transient notice in the nav's hint bar: the same life as a flash, painted
    /// as information rather than as an error.
    pub(crate) fn notice(&mut self, msg: impl Into<String>) {
        self.show_flash(msg.into(), FlashKind::Notice);
    }

    fn show_flash(&mut self, msg: String, kind: FlashKind) {
        self.flash = msg;
        self.flash_kind = kind;
        self.flash_until = Some(Instant::now() + FLASH_TTL);
    }

    /// Takes the flash down, however it came to be over: a key, or its own life.
    pub(crate) fn clear_flash(&mut self) {
        self.flash.clear();
        self.flash_until = None;
    }

    /// Drops a flash that has been up for its whole life, and says whether the bar
    /// changed, so a caller repaints only when it did.
    pub(crate) fn expire_flash(&mut self, now: Instant) -> bool {
        match self.flash_until {
            Some(until) if now >= until => {
                self.clear_flash();
                true
            }
            _ => false,
        }
    }

    /// Replaces the set of session addresses currently connecting / awaiting
    /// first output. The tree draws a braille spinner right of each matching
    /// session name.
    pub(crate) fn set_spinner(&mut self, addresses: HashSet<String>) {
        self.spinner = addresses;
    }

    /// Sets the braille spinner frame index. The app derives it from elapsed
    /// wall-clock time, so the spinner animates on every render rather than once
    /// per animation tick (which can starve under a `%output` flood).
    pub(crate) fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    /// Sets auto-hide-nav mode (the app owns it; the view border glyph reflects it).
    pub(crate) fn set_auto_hide(&mut self, on: bool) {
        self.auto_hide = on;
    }

    /// Sets whether the mouse is hovering the view border (the app derives it from
    /// idle motion); when set, the view border highlights as a drag-resize grab cue.
    pub(crate) fn set_view_border_hovered(&mut self, on: bool) {
        self.view_border_hovered = on;
    }

    /// Replaces the tree|terminal view border colours with a resolved set.
    #[cfg(test)]
    pub(crate) fn set_view_border_colors(&mut self, colors: ViewBorderColors) {
        self.colors = colors;
    }

    /// Sets the prefix string shown in the help modal. The app calls this once
    /// at startup so the help modal reflects the binding from config's `[ui] prefix`.
    pub(crate) fn set_ui_prefix(&mut self, prefix: String) {
        self.ui_prefix = prefix;
    }

    /// Sets whether the prefix is armed (pressed, awaiting its command key). The app
    /// calls this each frame from the live input state; the hint bar reads it to swap
    /// between the resting prefix indicator and the unlocked-keys cheatsheet.
    pub(crate) fn set_armed(&mut self, armed: bool) {
        self.armed = armed;
    }

    /// Sets the nav's attachment side. The app calls this each frame from the runtime's
    /// resolved position; the cheatsheet reads it to name the active arrow pair.
    pub(crate) fn set_nav_position(&mut self, position: crate::ui::switcher::NavPosition) {
        self.nav_position = position;
    }

    /// Sets what offered each host to the roster. The app calls this once at startup
    /// with the assembled roster; a host missing from the map simply shows no such row,
    /// which is the honest answer for one nothing recorded.
    pub(crate) fn set_roster_providers(&mut self, providers: HashMap<String, String>) {
        self.roster_providers = providers;
    }

    /// Sets the resolved login values and matching ssh stanzas the chrome renders.
    pub(crate) fn set_login_defaults(
        &mut self,
        defaults: HashMap<String, crate::provision::env::LoginDefaults>,
        stanzas: HashMap<String, String>,
    ) {
        self.login_defaults = defaults;
        self.ssh_stanzas = stanzas;
    }

    /// What ssh WOULD use to reach `source`, as the login pane's starting values: the
    /// address, the port, and the username.
    ///
    /// An effective ssh address, port, or user wins when present. Missing values fall back
    /// to the provider address or host name, port 22, and this machine's account name.
    /// A pane that opened on a failure therefore opens showing the effective connection
    /// values, and the user changes the part that was wrong.
    pub(crate) fn login_defaults(&self, source: &str) -> (String, String, String) {
        let host = crate::session::machine_of(source);
        self.login_defaults
            .get(host)
            .cloned()
            .unwrap_or_else(|| (host.to_string(), "22".into(), String::new()))
    }

    /// Sets how xmux reaches each source, keyed by source id. The app calls this once at
    /// startup from the assembled source list; a source missing from the map shows the
    /// rows it has and no blanks for the rest.
    /// What the mux on `source` is CALLED. The resolved reach answers it; a source id that
    /// carries its own mux (a machine serving several) is the fallback, for the paths that
    /// have a list of sources and no resolved reach yet. Empty while neither knows, which
    /// is the state a card turns a spinner for.
    pub(crate) fn source_mux<'a>(&'a self, source: &'a str) -> &'a str {
        match self.source_reach.get(source) {
            Some(reach) if !reach.kind.is_empty() => &reach.kind,
            _ => crate::session::mux_of(source),
        }
    }

    /// How `source` is SHOWN: `{host}/{mux}`, the one grammar the pair is read in.
    pub(crate) fn source_label(&self, source: &str) -> String {
        crate::session::source_label(crate::session::machine_of(source), self.source_mux(source))
    }

    /// The same label, for a surface that knows whether the host ANSWERED. A mux no
    /// answer confirmed is left off, so the label never puts a guess where every other
    /// one carries a fact.
    pub(crate) fn source_label_when(&self, source: &str, answered: bool) -> String {
        let mux = if crate::session::mux_may_be_named(source, answered) {
            self.source_mux(source)
        } else {
            ""
        };
        crate::session::source_label(crate::session::machine_of(source), mux)
    }

    pub(crate) fn set_source_reach(&mut self, reach: HashMap<String, SourceReach>) {
        self.source_reach = reach;
    }

    /// Sets the log file path the unreachable screen names. The app calls this once at
    /// startup with the file logging actually opened.
    pub(crate) fn set_log_path(&mut self, path: String) {
        self.log_path = path;
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
            // through its mux - so its screen names the pair. The two failure states
            // answered nothing, so theirs reads the host alone unless the id names the
            // mux, which is the name the user types for it.
            ViewScreen::Unreachable | ViewScreen::Login | ViewScreen::Empty => {
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
        } else if kind == ViewScreen::Unreachable || kind == ViewScreen::Login {
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
                address: defaults.0.clone(),
                port: defaults.1.clone(),
                username: defaults.2.clone(),
                default_address: defaults.0.clone(),
                default_port: defaults.1.clone(),
                default_username: defaults.2.clone(),
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
            let field = |name: &str, required: bool, value: &str, mask: bool, active: bool| {
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
                    Span::styled(format!("{text}{}", cursor(active)), style),
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
            out.push(Line::from(""));
            out.push(field(
                "address",
                true,
                &d.address,
                false,
                d.focus == LoginFocus::Address,
            ));
            out.push(field(
                "port",
                true,
                &d.port,
                false,
                d.focus == LoginFocus::Port,
            ));
            out.push(field(
                "username",
                true,
                &d.username,
                false,
                d.focus == LoginFocus::Username,
            ));
            out.push(field(
                "password",
                false,
                &d.password,
                true,
                d.focus == LoginFocus::Password,
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
    /// with the collapse button painted separately at the row's far end. Once the prefix
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
                FlashKind::Error => format!(" ⚠ {}", self.flash),
                FlashKind::Notice => format!(" {}", self.flash),
            }
        } else if let Some(Modal::Input(input)) = &state.modal {
            crate::ui::modal::input_hint_text(input, width)
        } else if self.armed {
            // The prefix is held: name what it unlocks. Longest-first so a narrow nav
            // drops the rarer chords rather than clipping mid-word.
            // Order: focus nav, focus terminal, jump, new, hide, position, rescan, help,
            // quit. The focus rows name the arrow PAIR the current placement makes
            // active (the pair facing the terminal's side names the terminal), and the
            // resize keys are left out (the help modal has them).
            let focus = if self.nav_position.forward_arrows_face_terminal() {
                "←/↑ focus nav · →/↓ focus terminal"
            } else {
                "→/↓ focus nav · ←/↑ focus terminal"
            };
            fit(
                &[
                    format!(" {p} · {focus} · 1-9 jump to a session · n new session · t hide nav · p nav position · r rescan · ? help · q quit"),
                    format!(" {p} · {focus} · 1-9 jump to · n new · t hide · p position · r rescan · ? help · q quit"),
                    format!(" {p} · {focus} · 1-9 · n · t · p · r · ? · q"),
                    format!(" {p} · ←/↑ · →/↓ · 1-9 · n · t · p · r · ? · q"),
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
                    format!(" filter: {} · / edit · Esc clear", state.filter),
                    format!(" filter: {}", state.filter),
                ],
                width,
            )
        } else {
            // At rest the text portion is the prefix. The renderer adds the button.
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
        let accent = Style::default()
            .fg(palette.bar_accent)
            .add_modifier(Modifier::BOLD);
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

    /// Paints the resting prefix and the collapse button across a collapsed nav's whole
    /// hint bar. Transient bars are handled by the ordinary floating-bar path instead.
    pub(crate) fn render_collapsed_hint_bar(
        &self,
        frame: &mut Frame,
        area: Rect,
        position: crate::ui::switcher::NavPosition,
        palette: &crate::ui::palette::Palette,
    ) {
        frame.render_widget(Clear, area);
        let line = self.hint_bar_line_spans(format!(" {}", self.ui_prefix), palette);
        frame.render_widget(
            Paragraph::new(line).style(self.hint_bar_render_style(palette)),
            area,
        );
        self.render_collapse_button(frame, area, position, true, palette);
    }

    /// Paints the collapse/expand token at the far end of a nav-local hint bar.
    pub(crate) fn render_collapse_button(
        &self,
        frame: &mut Frame,
        hint_bar: Rect,
        position: crate::ui::switcher::NavPosition,
        collapsed: bool,
        palette: &crate::ui::palette::Palette,
    ) {
        let rect = crate::ui::switcher::collapse_button_rect(hint_bar, position, collapsed);
        if rect.is_empty() {
            return;
        }
        let token = crate::ui::switcher::collapse_button_token(position, collapsed);
        let token_style = if self.hint_bar_style == hint_bar_default_style(palette) {
            Style::default()
                .fg(palette.bar_accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(token, token_style)))
                .style(self.hint_bar_render_style(palette)),
            rect,
        );
    }

    /// How many cells a [`BarFill::Content`] bar paints, so whatever else is on the row
    /// (the portrait flow's scrollbar) can start where the bar stops instead of being
    /// painted over.
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
    /// notice style without the `⚠` mark, while an error keeps both.
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
        assert!(!text.contains('⚠'), "{text:?}");
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
        assert!(c.hint_bar_text(80, &state).contains('⚠'));
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
        // At rest the logical text is the prefix; the switcher paints the button.
        assert_eq!(c.hint_bar_text(80, &state).trim(), "C-g");
        // Armed: the keys the prefix unlocks. Wide enough for the full descriptions,
        // the rows run in the bar's fixed order (focus nav, focus terminal, jump, new,
        // hide, rescan, filter, help, quit) and the focus rows use arrow symbols that
        // point at the view they focus.
        c.set_armed(true);
        let full = c.hint_bar_text(400, &state);
        assert!(full.starts_with(" C-g "), "{full:?}");
        let order = [
            "←/↑ focus nav",
            "→/↓ focus terminal",
            "1-9 jump to a session",
            "n new session",
            "t hide nav",
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
        // (the pair segment rides every rung). The full line is ~141 cells, so a 120-wide
        // bar forces the middle rung, whose focus rows keep the full pair wording.
        let armed = c.hint_bar_text(120, &state);
        assert!(
            armed.contains("→/↓ focus terminal"),
            "short bar keeps focus-terminal: {armed:?}"
        );
        for key in ["n new", "r rescan", "? help", "q quit"] {
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
