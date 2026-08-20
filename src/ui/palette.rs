//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and
//! a colour is changed in exactly one place. Values are truecolor RGB (the
//! Catppuccin Mocha scheme) - the same channel the hint bar background already
//! uses; a terminal without truecolor support approximates them.

use ratatui::style::Color;

/// The single accent: the selection bar, the active-window dot, key hints, and
/// popup titles all share it, so "interactive / current" is one colour everywhere.
pub(crate) const ACCENT: Color = Color::Rgb(0x89, 0xb4, 0xfa);
/// Secondary text: hint bar descriptions and panel prose.
pub(crate) const SUBTEXT: Color = Color::Rgb(0xa6, 0xad, 0xc8);
/// Muted furniture: the digit gutter, separators, popup borders, the scrollbar
/// thumb, and settled status text.
pub(crate) const OVERLAY: Color = Color::Rgb(0x6c, 0x70, 0x86);
/// The selection surface: the selected card's (and hovered menu item's)
/// background - a quiet raised surface instead of reverse video.
pub(crate) const SURFACE: Color = Color::Rgb(0x31, 0x32, 0x44);
/// The hint bar background: darker than the surface so the bar reads as chrome,
/// not content.
pub(crate) const BAR_BG: Color = Color::Rgb(0x18, 0x18, 0x25);
/// Level colour: host (soft yellow).
pub(crate) const HOST: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
/// Level colour: session (soft green).
pub(crate) const SESSION: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
/// Level colour: window (soft mauve).
pub(crate) const WINDOW: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
/// In-flight state: the scanning status and the loading spinner.
pub(crate) const PENDING: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
/// Failure state: the unreachable status and error text.
pub(crate) const DANGER: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
