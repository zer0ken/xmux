//! The switcher's chrome: the tree|terminal view border, the full-width hint_bar
//! (help / status / wrapped flash), and the unreachable-host info panel. These
//! own the view-local presentation state ([`Chrome`]) and read the runtime
//! inventory from `State`; the [`Switcher`](crate::ui::switcher::Switcher) holds a
//! [`Chrome`] and delegates these draws to it.

use std::collections::HashSet;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::ui::modal::wrap_text;
use crate::ui::switcher::fit;

/// Parses a tmux-style colour token into a ratatui [`Color`], matching tmux/psmux's
/// colour vocabulary so the view border colours can be configured exactly like
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

/// The tree|terminal view border's three colours: `active` marks the focused side,
/// `inactive` the unfocused side, and `hover` the drag-resize grab cue. Resolved in
/// three tiers by [`Self::resolve`] - an explicit xmux config override wins, else the
/// displayed host's live mux `pane-*-border-style`, else the stock default. Defaults
/// mirror tmux's own code defaults - `green` / terminal-default / `yellow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewBorderColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

impl Default for ViewBorderColors {
    fn default() -> Self {
        ViewBorderColors {
            active: Color::Green,
            inactive: Color::Reset,
            hover: Color::Yellow,
        }
    }
}

impl ViewBorderColors {
    /// Layers the three colour sources: an explicit xmux config value (`cfg_*`) wins,
    /// else the fg extracted from the displayed host's live mux style (`mux_active`
    /// / `mux_inactive`, e.g. `fg=blue,bg=default`), else the stock default. `hover`
    /// has no mux source (tmux has no `pane-border-hover-style`), so it is config-or-
    /// default only. An empty config string means "unset" - that is why the config
    /// keys default to empty (see [`crate::config::UiConfig`]); a non-empty stock
    /// default there would mask the mux query.
    pub fn resolve(
        mux_active: &str,
        mux_inactive: &str,
        cfg_active: &str,
        cfg_inactive: &str,
        cfg_hover: &str,
    ) -> Self {
        let d = ViewBorderColors::default();
        let pick = |cfg: &str, muxs: &str, fb: Color| {
            if !cfg.trim().is_empty() {
                map_color(cfg)
            } else {
                let fg = crate::mux::border_fg(muxs);
                if fg.trim().is_empty() {
                    fb
                } else {
                    map_color(&fg)
                }
            }
        };
        ViewBorderColors {
            active: pick(cfg_active, mux_active, d.active),
            inactive: pick(cfg_inactive, mux_inactive, d.inactive),
            hover: if cfg_hover.trim().is_empty() {
                d.hover
            } else {
                map_color(cfg_hover)
            },
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
pub(crate) fn hint_bar_default_style() -> Style {
    Style::default()
        .bg(crate::ui::palette::get().bar_bg)
        .fg(crate::ui::palette::get().bar_fg)
}

/// Parses a `[ui] hint-bar-style` spec into the hint bar [`Style`]. Empty ⇒ the
/// built-in tmux default ([`hint_bar_default_style`]). Otherwise a tmux-style comma
/// list: `bg=<colour>` sets the background, `fg=<colour>` (or a bare colour token) the
/// foreground, using the same colour vocabulary as the view border ([`map_color`], so
/// named colours, `colourN`, `#RRGGBB`, `default`). Unrecognised tokens are ignored.
pub(crate) fn parse_hint_bar_style(spec: &str) -> Style {
    if spec.trim().is_empty() {
        return hint_bar_default_style();
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
/// look, and xmux's default. Accepts the same colour vocabulary as the view
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

/// The hint bar's refusal style: a solid danger-red bar (the active palette's
/// `danger` as the background, the bar's own text slot on top) that breaks hard
/// from the calm default so a refused action reads as an
/// error at a glance, not as more of the key cheatsheet. Every flash today is a
/// refusal, so a shown flash always paints this. Fixed, not configurable: an
/// error must stay legible regardless of any `[ui] hint-bar-style` override.
pub(crate) fn error_flash_style() -> Style {
    Style::default()
        .bg(crate::ui::palette::get().danger)
        .fg(crate::ui::palette::get().bar_fg)
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

/// The switcher's chrome view state: the view border/hint_bar/host-info draws and
/// their inputs (flash, spinner set + frame, auto-hide + hover cues, view border
/// colours, the ssh-config text, the configured prefix string, whether the prefix is
/// currently armed, and the hint bar style).
pub struct Chrome {
    pub(crate) flash: String,
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
    /// Raw `~/.ssh/config` text (set once by the app). The terminal-view info panel
    /// shows the matching Host/Match stanza for a selected unreachable host. Empty in tests.
    pub(crate) ssh_config_text: String,
    /// The human-readable prefix string (e.g. `"C-g"`, `"C-Space"`) - set once by
    /// the app from config so the help modal reflects the active binding.
    pub(crate) ui_prefix: String,
    /// True while the prefix has been pressed and the app is waiting for the command
    /// key (set by the app each frame from the live input state, in either focus). The
    /// hint bar shows the prefix alone until this flips, then the keys it unlocks - so
    /// the cheatsheet appears exactly when it is needed and never competes with the
    /// cards for room.
    pub(crate) armed: bool,
    /// The tree|terminal view border colours (set once by the app from config; tmux defaults
    /// otherwise). See [`ViewBorderColors`].
    pub(crate) colors: ViewBorderColors,
    /// The hint bar's style (set once by the app from `[ui] hint-bar-style`; the tmux
    /// default otherwise). See [`hint_bar_default_style`].
    pub(crate) hint_bar_style: Style,
}

impl Default for Chrome {
    fn default() -> Self {
        Chrome {
            flash: String::new(),
            auto_hide: false,
            view_border_hovered: false,
            spinner: HashSet::new(),
            spinner_frame: 0,
            ssh_config_text: String::new(),
            ui_prefix: "C-g".into(),
            armed: false,
            colors: ViewBorderColors::default(),
            hint_bar_style: hint_bar_default_style(),
        }
    }
}

impl Chrome {
    /// Sets the transient flash message shown in the nav's hint bar (an error
    /// or notice). The next tree key clears it (the switcher's `handle_key`), so the
    /// normal help/status hint bar returns.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.flash = msg.into();
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

    /// Sets the tree|terminal view border colours. The app calls this with a config
    /// baseline at startup, then again per displayed host once its live mux
    /// `pane-*-border-style` is read (see [`ViewBorderColors::resolve`]).
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

    /// Sets the hint bar style. The app calls this once at startup from
    /// `[ui] hint-bar-style` (empty ⇒ the tmux default; see [`parse_hint_bar_style`]).
    pub(crate) fn set_hint_bar_style(&mut self, style: Style) {
        self.hint_bar_style = style;
    }

    /// Sets the raw `~/.ssh/config` text the unreachable-host info panel reads.
    pub(crate) fn set_ssh_config_text(&mut self, text: String) {
        self.ssh_config_text = text;
    }

    /// The vertical rule between the tree (left) and terminal (right). It splits into
    /// a top and bottom half: the accent (green) half marks WHICH view holds focus -
    /// top = tree (left), bottom = terminal (right) - and the other half stays dim. A single
    /// vertical rule cannot lean left/right, so the accent half's position carries the
    /// signal (adapting tmux's active-pane border). Replaces the per-pane box borders.
    /// The glyph also encodes auto-hide-nav mode: ║ (double) when on, │ when off - so
    /// a visible tree that will vanish on blur is distinguishable from a pinned one.
    pub(crate) fn render_view_border(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        let active = self.colors.active;
        let inactive = self.colors.inactive;
        // Top layout: the view border runs HORIZONTALLY between the top tree and the
        // bottom terminal. Split left/right to cue focus (left lit = tree focus, right =
        // terminal focus), mirroring the vertical rule's top/bottom split.
        if area.width > area.height {
            let g = if self.view_border_hovered {
                "━"
            } else if self.auto_hide {
                "═"
            } else {
                "─"
            };
            let n = area.width;
            let cells: Vec<Span> = if self.view_border_hovered {
                let s = Style::default().fg(self.colors.hover);
                (0..n).map(|_| Span::styled(g, s)).collect()
            } else if n <= 1 {
                vec![Span::styled(g, Style::default().fg(active))]
            } else {
                let left_cols = n.div_ceil(2);
                let (left, right) = if terminal_focused {
                    (inactive, active)
                } else {
                    (active, inactive)
                };
                (0..n)
                    .map(|x| {
                        let c = if x < left_cols { left } else { right };
                        Span::styled(g, Style::default().fg(c))
                    })
                    .collect()
            };
            frame.render_widget(Paragraph::new(Line::from(cells)), area);
            return;
        }
        let glyph = if self.auto_hide { "║" } else { "│" };
        // Hover (mouse over the rule, no button): box-drawing rules have no bold form
        // (the BOLD modifier does not thicken them), so swap the glyph itself to the
        // HEAVY vertical (┃) for a genuinely thicker line and recolour it with the
        // configured hover colour (tmux's `pane-border-hover-style`) - same single rule,
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
        let colors: Vec<Color> = if area.height <= 1 {
            // Too short to split: show the active-marker color in the single cell.
            vec![active; area.height as usize]
        } else {
            let top_rows = area.height.div_ceil(2); // top takes the extra row on odd heights
            let (top, bottom) = if terminal_focused {
                (inactive, active) // terminal focused → accent on the bottom (terminal side)
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

    /// The terminal-view info panel for a selected unreachable host: the failure reason
    /// and the host's `~/.ssh/config` stanza, so the user can see WHY the control
    /// connection failed without leaving the app.
    pub(crate) fn render_host_info(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        source: &str,
    ) {
        let alias = source.to_string();
        let reason = state
            .groups
            .iter()
            .find(|g| g.source == alias)
            .and_then(|g| g.err.clone())
            .unwrap_or_else(|| "connection closed".into());
        let mut lines = vec![
            Line::from(Span::styled(
                format!(" ⚠ {alias} unreachable"),
                Style::default().fg(crate::ui::palette::get().danger),
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

    /// The terminal-view landing panel for a selected REACHABLE host that has no
    /// sessions yet: names the host and the keys to start one or rescan, so a
    /// freshly-reachable but empty host offers a next step rather than a blank grid.
    pub(crate) fn render_host_landing(&self, frame: &mut Frame, area: Rect, source: &str) {
        let p = &self.ui_prefix;
        let accent = Style::default().fg(crate::ui::palette::get().accent);
        let prose = Style::default().fg(crate::ui::palette::get().subtext);
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {source}"),
                accent.add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled("  reachable, no sessions yet", prose)),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("  {p} n"), accent),
                Span::styled("  start a new session", prose),
            ]),
            Line::from(vec![
                Span::styled(format!("  {p} r"), accent),
                Span::styled("  rescan this host", prose),
            ]),
        ];
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    /// The hint bar's logical text, fit to `width`. Modeled on zellij's status bar:
    /// at rest it shows only the prefix, so the nav's bottom row is a quiet reminder
    /// of the one key that opens everything; once the prefix is ARMED it becomes the
    /// list of keys that prefix unlocks, which is the moment the user needs it. The
    /// transient states outrank both, in order: a flash (a refusal), the scan
    /// progress, then the active filter. A flash is returned raw - it may exceed
    /// `width`; [`Self::hint_bar_lines`] wraps it so it never clips.
    pub(crate) fn hint_bar_text(&self, width: u16, state: &crate::state::State) -> String {
        // Use the active prefix so the hint_bar matches the user's configured binding.
        let p = &self.ui_prefix;
        if !self.flash.is_empty() {
            format!(" ⚠ {}", self.flash)
        } else if self.armed {
            // The prefix is held: name what it unlocks. Longest-first so a narrow nav
            // drops the rarer chords rather than clipping mid-word.
            fit(
                &[
                    format!(" {p} 0-9 jump to a session · n new session · r rescan · / filter · Tab terminal · t hide · h/l size · ? help · q quit"),
                    format!(" {p} 0-9 jump · n new · r rescan · Tab term · t hide · ? help · q quit"),
                    format!(" {p} 0-9 · n · r · Tab · t · h/l · ? · q"),
                    format!(" {p} 0-9 n r Tab t ? q"),
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
            // At rest: the prefix alone. Everything else is one keypress away.
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
    /// the [`error_flash_style`] (every flash is a refusal); otherwise the configured
    /// status style. Split from [`Self::render_hint_bar`] so the choice is unit-testable
    /// without a backend.
    pub(crate) fn hint_bar_render_style(&self) -> Style {
        if self.flash.is_empty() {
            self.hint_bar_style
        } else {
            error_flash_style()
        }
    }

    /// One hint-bar line as styled spans: each ` · `-separated segment's leading key
    /// token (two tokens when the first is the prefix, e.g. `C-g n`) gets the accent,
    /// the separators go muted, and the rest inherits the bar's base style. Purely
    /// presentational - the text is exactly the [`Self::hint_bar_lines`] line, so the
    /// fit / wrap behaviour is untouched.
    fn hint_bar_line_spans(&self, line: String) -> Line<'static> {
        let accent = Style::default().fg(crate::ui::palette::get().accent);
        let sep_style = Style::default().fg(crate::ui::palette::get().overlay);
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

    pub(crate) fn render_hint_bar(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        fill: BarFill,
    ) {
        let lines = self.hint_bar_lines(area.width, state);
        // Key tokens get the accent only on the built-in default style with no flash
        // showing: a `[ui] hint-bar-style` override keeps its exact colours (uniform,
        // as configured), and a flash stays solid error-red.
        let width = lines
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let styled = self.flash.is_empty() && self.hint_bar_style == hint_bar_default_style();
        let text = if styled {
            Text::from(
                lines
                    .into_iter()
                    .map(|l| self.hint_bar_line_spans(l))
                    .collect::<Vec<_>>(),
            )
        } else {
            Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>())
        };
        // The hint bar is a solid status bar: the configured status style
        // (`hint_bar_default_style` / the `[ui] hint-bar-style` override) normally, or the
        // `error_flash_style` while a refusal flash shows. The style fills the whole area,
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
        frame.render_widget(
            Paragraph::new(text).style(self.hint_bar_render_style()),
            painted,
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
        // Empty (and whitespace-only) ⇒ the built-in tmux default (yellowgreen / gray5).
        assert_eq!(parse_hint_bar_style(""), hint_bar_default_style());
        assert_eq!(parse_hint_bar_style("   "), hint_bar_default_style());
        // bg=/fg= tokens set the two colours (tmux status-style vocabulary).
        let s = parse_hint_bar_style("bg=blue,fg=white");
        assert_eq!(s.bg, Some(Color::Blue));
        assert_eq!(s.fg, Some(Color::White));
        // A bare colour token is the foreground (tmux convention).
        assert_eq!(parse_hint_bar_style("red").fg, Some(Color::Red));
    }

    #[test]
    fn hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed() {
        let mut c = Chrome::default();
        let state = crate::state::State::default();
        // At rest: the prefix alone. That is the whole resting cheatsheet.
        assert_eq!(c.hint_bar_text(80, &state).trim(), "C-g");
        // Armed: the keys the prefix unlocks.
        c.set_armed(true);
        let armed = c.hint_bar_text(120, &state);
        assert!(armed.starts_with(" C-g "), "{armed:?}");
        for key in ["n new", "r rescan", "? help", "q quit"] {
            assert!(armed.contains(key), "armed bar lists {key:?}: {armed:?}");
        }
        // A flash outranks the armed cheatsheet: a refusal must not be hidden by it.
        c.flash("host unreachable");
        assert!(c.hint_bar_text(120, &state).contains("host unreachable"));
    }

    #[test]
    fn flash_paints_the_error_style_not_the_status_style() {
        let mut c = Chrome::default();
        assert_eq!(
            c.hint_bar_render_style(),
            c.hint_bar_style,
            "with no flash the bar keeps the configured status style"
        );
        c.flash("cannot kill a host");
        assert_eq!(
            c.hint_bar_render_style(),
            error_flash_style(),
            "a refusal flash paints the distinct error style"
        );
        assert_ne!(
            error_flash_style(),
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
    fn resolve_layers_override_over_mux_over_default() {
        // Mux value used when config is unset; the comma-separated `fg=` is extracted.
        // hover has no mux source, so it falls to the default (yellow).
        let c = ViewBorderColors::resolve("fg=blue,bg=default", "fg=white", "", "", "");
        assert_eq!(c.active, Color::Blue);
        assert_eq!(c.inactive, Color::White);
        assert_eq!(c.hover, Color::Yellow);

        // An explicit config override wins over the mux value.
        let c = ViewBorderColors::resolve("fg=blue", "fg=white", "red", "green", "cyan");
        assert_eq!(c.active, Color::Red);
        assert_eq!(c.inactive, Color::Green);
        assert_eq!(c.hover, Color::Cyan);

        // Everything empty / unavailable → the stock default (the no-regression guarantee).
        assert_eq!(
            ViewBorderColors::resolve("", "", "", "", ""),
            ViewBorderColors::default()
        );

        // Bare + `default` tokens resolve too (`default` → terminal default = Reset).
        let c = ViewBorderColors::resolve("green", "default", "", "", "");
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
