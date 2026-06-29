//! The switcher's status surface: the tree|mux divider, the tree-column footer
//! (help / status / wrapped flash), and the unreachable-host info panel. These
//! own the view-local presentation state ([`Status`]) and read the runtime
//! inventory from `State`; the [`Switcher`](crate::ui::switcher::Switcher) holds a
//! [`Status`] and delegates these draws to it.

use std::collections::HashSet;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::switcher::{fit, wrap_text};

/// The tree|mux divider's three colours, resolved from config (tmux's pane-border
/// options): `active` marks the focused side, `inactive` the unfocused side, and
/// `hover` the drag-resize grab cue. Defaults mirror tmux's own code defaults —
/// `green` / terminal-default / `yellow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DividerColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

impl Default for DividerColors {
    fn default() -> Self {
        DividerColors {
            active: Color::Green,
            inactive: Color::Reset,
            hover: Color::Yellow,
        }
    }
}

/// The switcher's status-surface view state: the divider/footer/host-info draws and
/// their inputs (flash notice, spinner set + frame, auto-hide + hover cues, divider
/// colours, the ssh-config text, and the configured prefix string).
pub struct Status {
    pub(crate) flash: String,
    /// Auto-hide-tree mode (set by the cockpit each frame). Drives the divider glyph:
    /// ║ (double) when on, │ (single) when off — the only on-screen cue, since while
    /// the mode is on but the tree is focused the tree still shows.
    pub(crate) auto_hide: bool,
    /// True while the mouse is hovering the divider rule — the cockpit sets this from
    /// idle motion so the divider highlights as a grab cue for drag-resize.
    pub(crate) divider_hovered: bool,
    /// Session addresses currently connecting / awaiting first output — a braille
    /// spinner glyph renders right of their name in the tree.
    pub(crate) spinner: HashSet<String>,
    pub(crate) spinner_frame: usize,
    /// Raw `~/.ssh/config` text (set once by the cockpit). The right-pane info panel
    /// shows the matching Host/Match stanza for a selected unreachable host. Empty in tests.
    pub(crate) ssh_config_text: String,
    /// The human-readable prefix string (e.g. `"C-g"`, `"C-Space"`) — set once by
    /// the cockpit from config so the help overlay reflects the active binding.
    pub(crate) ui_prefix: String,
    /// The tree|mux divider colours (set once by the cockpit from config; tmux defaults
    /// otherwise). See [`DividerColors`].
    pub(crate) colors: DividerColors,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            flash: String::new(),
            auto_hide: false,
            divider_hovered: false,
            spinner: HashSet::new(),
            spinner_frame: 0,
            ssh_config_text: String::new(),
            ui_prefix: "C-g".into(),
            colors: DividerColors::default(),
        }
    }
}

impl Status {
    /// Replaces the set of session addresses currently connecting / awaiting
    /// first output. The tree draws a braille spinner right of each matching
    /// session name.
    pub(crate) fn set_spinner(&mut self, addresses: HashSet<String>) {
        self.spinner = addresses;
    }

    /// Sets the braille spinner frame index. The cockpit derives it from elapsed
    /// wall-clock time, so the spinner animates on every render rather than once
    /// per animation tick (which can starve under a `%output` flood).
    pub(crate) fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    /// Sets auto-hide-tree mode (the cockpit owns it; the divider glyph reflects it).
    pub(crate) fn set_auto_hide(&mut self, on: bool) {
        self.auto_hide = on;
    }

    /// Sets whether the mouse is hovering the divider (the cockpit derives it from
    /// idle motion); when set, the divider highlights as a drag-resize grab cue.
    pub(crate) fn set_divider_hovered(&mut self, on: bool) {
        self.divider_hovered = on;
    }

    /// Sets the tree|mux divider colours. The cockpit calls this once at startup with
    /// the colours parsed from config's `pane-*-border-style` options; tmux defaults
    /// apply otherwise.
    pub(crate) fn set_divider_colors(&mut self, colors: DividerColors) {
        self.colors = colors;
    }

    /// Sets the prefix string shown in the help overlay. The cockpit calls this once
    /// at startup so the overlay reflects the binding from config's `[ui] prefix`.
    pub(crate) fn set_ui_prefix(&mut self, prefix: String) {
        self.ui_prefix = prefix;
    }

    /// Sets the raw `~/.ssh/config` text the unreachable-host info panel reads.
    pub(crate) fn set_ssh_config_text(&mut self, text: String) {
        self.ssh_config_text = text;
    }

    /// The vertical rule between the tree (left) and terminal (right). It splits into
    /// a top and bottom half: the accent (green) half marks WHICH pane holds focus —
    /// top = tree (left), bottom = mux (right) — and the other half stays dim. A single
    /// vertical rule cannot lean left/right, so the accent half's position carries the
    /// signal (adapting tmux's active-pane border). Replaces the per-pane box borders.
    /// The glyph also encodes auto-hide-tree mode: ║ (double) when on, │ when off — so
    /// a visible tree that will vanish on blur is distinguishable from a pinned one.
    pub(crate) fn render_divider(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        let active = self.colors.active;
        let inactive = self.colors.inactive;
        let glyph = if self.auto_hide { "║" } else { "│" };
        // Hover (mouse over the rule, no button): box-drawing rules have no bold form
        // (the BOLD modifier does not thicken them), so swap the glyph itself to the
        // HEAVY vertical (┃) for a genuinely thicker line and recolour it with the
        // configured hover colour (tmux's `pane-border-hover-style`) — same single rule,
        // just thicker + lit, as the grab cue.
        if self.divider_hovered {
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
                (inactive, active) // mux focused → accent on the bottom (mux side)
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

    /// The right-pane info panel for a selected unreachable host: the failure reason
    /// and the host's `~/.ssh/config` stanza, so the user can see WHY the control
    /// connection failed without leaving the cockpit.
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
                Style::default().fg(Color::Yellow),
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

    /// The footer's logical text (confirm / flash / scanning / filter / help), fit to
    /// `width`. A flash is returned raw — it may exceed `width`; [`Self::footer_lines`]
    /// wraps it so it never clips.
    pub(crate) fn footer_text(&self, width: u16, state: &crate::state::State) -> String {
        // Use the active prefix so the footer matches the user's configured binding.
        let p = &self.ui_prefix;
        if !self.flash.is_empty() {
            format!(" {}", self.flash)
        } else if !state.scanning.is_empty() {
            // A subtle global indicator while host probes are in flight; clears
            // (falls through to the help line) once every host has settled.
            let total = state.groups.len();
            let done = total.saturating_sub(state.scanning.len());
            fit(
                &[
                    format!(" ⟳ scanning hosts {done}/{total}… · {p} q quit · {p} ? help"),
                    format!(" ⟳ scanning {done}/{total}…"),
                ],
                width,
            )
        } else if !state.filter.is_empty() {
            // The active filter has no border title to live in any more, so it
            // shows in the footer (with how to clear it).
            fit(
                &[
                    format!(
                        " filter: {} · / edit · Esc clear · {p} ? help · {p} q quit",
                        state.filter
                    ),
                    format!(" filter: {}", state.filter),
                ],
                width,
            )
        } else {
            fit(
                &[
                    format!(" ↑/↓ move · Enter/{p}→ focus mux · / filter · n new · R rename · x kill · r refresh · {p} ? help · {p} q quit"),
                    format!(" ↑/↓ move · Enter focus mux · / filter · n new · x kill · {p} ? help · {p} q quit"),
                    format!(" move · Enter focus mux · / filter · {p} ? help · {p} q quit"),
                    format!(" Enter focus mux · {p} ? help · {p} q quit"),
                    format!(" {p} ? help · {p} q quit"),
                ],
                width,
            )
        }
    }

    /// The footer text split into the lines to render. The fit-based text is always one
    /// line; only a flash (an arbitrary error/notice) may exceed `width`, so it wraps
    /// across the narrow tree-column footer rather than clipping.
    pub(crate) fn footer_lines(&self, width: u16, state: &crate::state::State) -> Vec<String> {
        let text = self.footer_text(width, state);
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

    pub(crate) fn render_footer(&self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        let lines = self.footer_lines(area.width, state);
        let text = Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>());
        frame.render_widget(Paragraph::new(text), area);
    }
}
