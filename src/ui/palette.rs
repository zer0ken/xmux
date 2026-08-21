//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and
//! a colour is changed in exactly one place. Two variants exist - a dark-terminal
//! set and a light-terminal set (Catppuccin Mocha / Latte values) - picked once at
//! startup from the terminal's reported background ([`init_for_dark_terminal`],
//! fed by an OSC 11 query); everything else reads the active set through [`get`].
//! Values are truecolor RGB - the same channel the hint bar background already
//! uses; a terminal without truecolor support approximates them.

use std::sync::OnceLock;

use ratatui::style::Color;

/// The semantic colour set. One field per UI role - callers name the role, never
/// a hue, so the dark and light variants stay interchangeable.
pub(crate) struct Palette {
    /// The single accent: the selection bar, the active-window dot, key hints, and
    /// popup titles all share it, so "interactive / current" is one colour everywhere.
    pub accent: Color,
    /// Secondary text: hint bar descriptions and panel prose.
    pub subtext: Color,
    /// Muted furniture: the digit gutter, separators, popup borders, the scrollbar
    /// thumb, and settled status text.
    pub overlay: Color,
    /// The selection surface: the selected card's (and hovered menu item's)
    /// background - a quiet raised surface instead of reverse video.
    pub surface: Color,
    /// The hint bar background: set off from the terminal background so the bar
    /// reads as chrome, not content.
    pub bar_bg: Color,
    /// Level colour: host.
    pub host: Color,
    /// Level colour: mux.
    pub mux: Color,
    /// Level colour: session.
    pub session: Color,
    /// Level colour: window (the `{index}:{name}` part of the detail line) - the
    /// quietest level, so the session name reads as the detail line's anchor.
    pub window: Color,
    /// In-flight state: the scanning status and the loading spinner.
    pub pending: Color,
    /// Failure state: the unreachable status and error text.
    pub danger: Color,
}

/// The dark-terminal set (Catppuccin Mocha).
static DARK: Palette = Palette {
    accent: Color::Rgb(0x89, 0xb4, 0xfa),
    subtext: Color::Rgb(0xa6, 0xad, 0xc8),
    overlay: Color::Rgb(0x6c, 0x70, 0x86),
    surface: Color::Rgb(0x31, 0x32, 0x44),
    bar_bg: Color::Rgb(0x18, 0x18, 0x25),
    host: Color::Rgb(0xf9, 0xe2, 0xaf),
    mux: Color::Rgb(0xa6, 0xe3, 0xa1),
    session: Color::Rgb(0xcb, 0xa6, 0xf7),
    window: Color::Rgb(0xa6, 0xad, 0xc8),
    pending: Color::Rgb(0xf9, 0xe2, 0xaf),
    danger: Color::Rgb(0xf3, 0x8b, 0xa8),
};

/// The light-terminal set (Catppuccin Latte). The pastel Mocha hues wash out on a
/// light background (a pale-yellow host name on cream is unreadable), so this set
/// deepens every role to the Latte values.
static LIGHT: Palette = Palette {
    accent: Color::Rgb(0x1e, 0x66, 0xf5),
    subtext: Color::Rgb(0x6c, 0x6f, 0x85),
    overlay: Color::Rgb(0x9c, 0xa0, 0xb0),
    surface: Color::Rgb(0xcc, 0xd0, 0xda),
    bar_bg: Color::Rgb(0xe6, 0xe9, 0xef),
    host: Color::Rgb(0xdf, 0x8e, 0x1d),
    mux: Color::Rgb(0x40, 0xa0, 0x2b),
    session: Color::Rgb(0x88, 0x39, 0xef),
    window: Color::Rgb(0x6c, 0x6f, 0x85),
    pending: Color::Rgb(0xdf, 0x8e, 0x1d),
    danger: Color::Rgb(0xd2, 0x0f, 0x39),
};

/// The active set, chosen once at startup. Unset (tests, the headless dump path,
/// a failed background query) reads as dark - the historical default.
static ACTIVE: OnceLock<&'static Palette> = OnceLock::new();

/// Picks the active palette from the terminal's background. Called once at app
/// startup, before any render; later calls are ignored (`OnceLock`).
pub(crate) fn init_for_dark_terminal(dark: bool) {
    let _ = ACTIVE.set(if dark { &DARK } else { &LIGHT });
}

/// The active palette. Dark until [`init_for_dark_terminal`] says otherwise.
pub(crate) fn get() -> &'static Palette {
    ACTIVE.get().copied().unwrap_or(&DARK)
}
