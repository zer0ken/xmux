//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and
//! a colour is changed in exactly one place. Every foreground role is an ANSI-16
//! colour, so the terminal theme resolves the actual hue and the UI recolours with
//! whatever scheme the user runs. The two xmux-own BACKGROUNDS (the selection
//! surface and the hint bar) have no ANSI slot, so they are derived once at startup
//! from the terminal's reported background ([`init_for_terminal_background`], fed
//! by an OSC 11 query) - a small step toward the foreground - and thus follow the
//! theme too. When the query fails (an unsupported terminal, the headless dump
//! path, tests) fixed dark-leaning fallbacks stand in.

use std::sync::OnceLock;

use ratatui::style::Color;

/// The semantic colour set. One field per UI role - callers name the role, never
/// a hue, so the assignments below stay changeable in one place.
pub(crate) struct Palette {
    /// The single accent: the selection bar, key hints, and popup titles all
    /// share it, so "interactive / current" is one colour everywhere.
    pub accent: Color,
    /// Secondary text: hint bar descriptions and panel prose.
    pub subtext: Color,
    /// Muted furniture: the digit gutter, separators, the └/├ connectors, popup
    /// borders, the scrollbar thumb, and settled status text.
    pub overlay: Color,
    /// The selection surface: the selected card's (and hovered menu item's)
    /// background - a quiet raised surface instead of reverse video. Derived
    /// from the terminal background at startup.
    pub surface: Color,
    /// The hint bar background: set off from the terminal background so the bar
    /// reads as chrome, not content. Derived from the terminal background at
    /// startup.
    pub bar_bg: Color,
    /// Level colour: host.
    pub host: Color,
    /// Level colour: mux.
    pub mux: Color,
    /// Level colour: window (the `{index}:{name}` part of the detail line) - the
    /// quietest level, so the session name reads as the detail line's anchor.
    pub window: Color,
    /// Level colour: session.
    pub session: Color,
    /// In-flight state: the scanning status and the loading spinner.
    pub pending: Color,
    /// Failure state: the unreachable status and error text.
    pub danger: Color,
}

/// The role-to-ANSI assignments, with the fixed fallback backgrounds (a dark
/// assumption - the historical default when the terminal cannot be queried).
/// `DarkGray` is ANSI bright black - the theme's own muted tone.
const fn base() -> Palette {
    Palette {
        accent: Color::Blue,
        subtext: Color::DarkGray,
        overlay: Color::DarkGray,
        surface: Color::Rgb(0x31, 0x32, 0x44),
        bar_bg: Color::Rgb(0x18, 0x18, 0x25),
        host: Color::Cyan,
        mux: Color::Green,
        window: Color::DarkGray,
        session: Color::Cyan,
        pending: Color::Yellow,
        danger: Color::Red,
    }
}

/// The set served until (or in place of) [`init_for_terminal_background`].
static FALLBACK: Palette = base();

/// The active set, derived once at startup. Unset (tests, the headless dump
/// path, a failed background query) reads as [`FALLBACK`].
static ACTIVE: OnceLock<Palette> = OnceLock::new();

/// Derives the surface and hint bar backgrounds from the terminal's reported
/// background `(r, g, b)`: each steps toward the opposite end of the lightness
/// range (a raised surface on a dark theme, a lowered one on a light theme), so
/// both stay in the theme's own family. Called once at app startup, before any
/// render; later calls are ignored (`OnceLock`).
pub(crate) fn init_for_terminal_background(bg: (u8, u8, u8)) {
    let (surface, bar_bg) = derive_backgrounds(bg);
    let _ = ACTIVE.set(Palette {
        surface,
        bar_bg,
        ..base()
    });
}

/// The `(surface, bar_bg)` pair for a terminal background: a 13% / 7% step
/// toward white on a dark background, toward black on a light one - the surface
/// visibly raised, the bar only set off.
fn derive_backgrounds(bg: (u8, u8, u8)) -> (Color, Color) {
    let (r, g, b) = bg;
    let dark = (r as u32 + g as u32 + b as u32) / 3 < 128;
    let toward = |c: u8, pct: i32| -> u8 {
        let target: i32 = if dark { 255 } else { 0 };
        (c as i32 + (target - c as i32) * pct / 100) as u8
    };
    let shift = |pct| Color::Rgb(toward(r, pct), toward(g, pct), toward(b, pct));
    (shift(13), shift(7))
}

/// The active palette. The fallback until [`init_for_terminal_background`] runs.
pub(crate) fn get() -> &'static Palette {
    ACTIVE.get().unwrap_or(&FALLBACK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_backgrounds_raises_on_dark_and_lowers_on_light() {
        // A dark background steps toward white: surface farther than the bar.
        let (surface, bar) = derive_backgrounds((0x0c, 0x0c, 0x0c));
        assert_eq!(surface, Color::Rgb(0x2b, 0x2b, 0x2b));
        assert_eq!(bar, Color::Rgb(0x1d, 0x1d, 0x1d));
        // A light background steps toward black.
        let (surface, bar) = derive_backgrounds((0xff, 0xff, 0xff));
        assert_eq!(surface, Color::Rgb(0xde, 0xde, 0xde));
        assert_eq!(bar, Color::Rgb(0xee, 0xee, 0xee));
    }

    #[test]
    fn foreground_roles_are_ansi_indexed() {
        // Every foreground role must stay an ANSI-16 colour so the terminal
        // theme resolves the hue; only the two derived backgrounds may be RGB.
        let p = base();
        for c in [
            p.accent, p.subtext, p.overlay, p.host, p.mux, p.window, p.session, p.pending, p.danger,
        ] {
            assert!(
                !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                "{c:?} does not follow the terminal theme"
            );
        }
    }
}
