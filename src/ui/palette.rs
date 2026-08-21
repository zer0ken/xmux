//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and
//! a colour is changed in exactly one place. Every foreground role is an ANSI-16
//! colour, so the terminal theme resolves the actual hue and the UI recolours with
//! whatever scheme the user runs.
//!
//! The two xmux-own BACKGROUNDS (the selection surface and the hint bar) have no ANSI
//! slot: "one step off the background" is not a colour a 16-slot palette can name. So
//! they are derived from the terminal's reported background when it answers an OSC 11
//! query - a small step toward the foreground, staying in the theme's own family.
//!
//! When it does not answer, the SELECTION SURFACE stays the terminal's own background
//! and paints nothing. A surface is the one colour xmux cannot guess: it sits UNDER
//! text whose every hue is the theme's, so a fixed RGB is wrong on every theme it was
//! not chosen for - and Windows Terminal answers no colour query, so the guess was not
//! a rare fallback there but the permanent state. A user who wants a surface anyway
//! names one with `[ui] selection-style`.
//!
//! The hint bar keeps a fixed dark tone when unqueried. It is a solid bar of chrome
//! rather than a wash under themed text, so a dark bar reads as a bar on either kind of
//! theme; `[ui] hint-bar-style` overrides it.

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
    /// background - a quiet raised surface instead of reverse video. Derived from the
    /// terminal background when it is known, `Reset` (the terminal's own background, so
    /// no surface) when it is not, and whatever `[ui] selection-style` names when the
    /// user names one.
    pub surface: Color,
    /// The hint bar background: set off from the terminal background so the bar
    /// reads as chrome, not content. Derived from the terminal background when it is
    /// known, a fixed dark tone when it is not (a solid bar reads as a bar on either
    /// kind of theme). `[ui] hint-bar-style` overrides it.
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

/// The role-to-ANSI assignments. `surface` is `Reset` - the terminal's own background -
/// so an un-queried, un-configured run paints no surface under the cards rather than a
/// colour picked for somebody else's theme; `bar_bg` keeps a fixed dark tone because the
/// hint bar is a solid bar of chrome, not a wash under themed text. `DarkGray` is ANSI
/// bright black, the theme's own muted tone.
const fn base() -> Palette {
    Palette {
        accent: Color::Blue,
        subtext: Color::DarkGray,
        overlay: Color::DarkGray,
        surface: Color::Reset,
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

/// Installs this run's palette, once, before any render; later calls are ignored
/// (`OnceLock`).
///
/// `bg` is the terminal's reported background, or `None` when it did not answer. When it
/// is known both backgrounds step toward the opposite end of the lightness range (a
/// raised surface on a dark theme, a lowered one on a light theme) so they stay in the
/// theme's own family. When it is not, they stay `Reset` and nothing is painted.
///
/// `selection_bg` is `[ui] selection-style`, and it wins either way: a user who names a
/// surface gets exactly it, whether or not the terminal answered.
pub(crate) fn init(bg: Option<(u8, u8, u8)>, selection_bg: Option<Color>) {
    let mut p = base();
    if let Some(bg) = bg {
        let (surface, bar_bg) = derive_backgrounds(bg);
        p.surface = surface;
        p.bar_bg = bar_bg;
    }
    if let Some(c) = selection_bg {
        p.surface = c;
    }
    let _ = ACTIVE.set(p);
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

/// Asks the terminal for its background over an OSC 11 round-trip. `None` when the
/// terminal does not answer (an unsupported terminal, a timeout, a pipe). Must run
/// BEFORE raw mode / the alternate screen: the query library manages the terminal
/// itself. Lives here so the app and `xmux doctor` ask the same question the same way.
pub(crate) fn query_terminal_background() -> Option<(u8, u8, u8)> {
    // Channels are the full u16 range; the high byte is the 8-bit value.
    terminal_colorsaurus::background_color(terminal_colorsaurus::QueryOptions::default())
        .ok()
        .map(|bg| ((bg.r >> 8) as u8, (bg.g >> 8) as u8, (bg.b >> 8) as u8))
}

/// A human line describing where the selection surface's colour comes from, for
/// `xmux doctor`. `bg` is the terminal's reported background (`None` when it did not
/// answer) and `selection_bg` is `[ui] selection-style`.
///
/// Worth reporting because the answer is invisible from the screen: a terminal that
/// answers gets a surface stepped out of its OWN background, one that does not gets no
/// surface at all, and a configured colour overrides both. "My colours look wrong" has
/// no other way to be told apart from "my theme is unusual".
pub(crate) fn source_report(bg: Option<(u8, u8, u8)>, selection_bg: Option<Color>) -> String {
    let reported = match bg {
        Some((r, g, b)) => format!("terminal background: #{r:02x}{g:02x}{b:02x} (queried)"),
        None => {
            "terminal background: UNKNOWN (this terminal does not answer the query)".to_string()
        }
    };
    // The override decides the surface outright, so it is stated instead of the
    // derivation rather than after it.
    if let Some(c) = selection_bg {
        return format!(
            "{reported} — selection surface set by [ui] selection-style: {}",
            describe(c)
        );
    }
    match bg {
        Some(bg) => format!(
            "{reported} — selection surface derived from it: {}",
            describe(derive_backgrounds(bg).0)
        ),
        None => format!(
            "{reported} — no selection surface is painted; name one with [ui] selection-style"
        ),
    }
}

/// A colour as a user would write it in config: `#rrggbb` for a true colour, the ANSI
/// role's own name otherwise. `Color`'s `Debug` spells an RGB triple as
/// `Rgb(45, 79, 107)`, which is not a value anyone can paste back.
fn describe(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => "the terminal's own background".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_says_where_the_surface_colour_came_from() {
        // The whole point of the line: which of the three sources is in effect is
        // invisible on screen, and it decides whether the card background follows the
        // theme at all.
        let queried = source_report(Some((0x1e, 0x1e, 0x2e)), None);
        assert!(queried.contains("#1e1e2e"), "{queried}");
        assert!(queried.contains("queried"), "{queried}");
        let unknown = source_report(None, None);
        assert!(unknown.contains("UNKNOWN"), "{unknown}");
        assert!(
            unknown.contains("no selection surface is painted"),
            "says what it paints instead of naming a colour: {unknown}"
        );
        // An override decides the surface outright, so the line says so instead of
        // still claiming nothing is painted - and it spells the colour the way config
        // does, not as a Debug triple.
        let named = source_report(None, Some(Color::Rgb(0x2d, 0x4f, 0x6b)));
        assert!(named.contains("set by [ui] selection-style"), "{named}");
        assert!(named.contains("#2d4f6b"), "{named}");
        assert!(
            !named.contains("no selection surface"),
            "the override replaces that clause: {named}"
        );
    }

    #[test]
    fn an_unqueried_run_paints_no_surface_under_the_cards() {
        // The defect this fixes: the surface sits UNDER text whose every hue is the
        // theme's, so a fixed RGB is wrong on every theme it was not chosen for - and
        // Windows Terminal answers no colour query, so the guess was the permanent state
        // there rather than a rare fallback. The hint bar is a solid bar of chrome
        // instead, so it keeps its fixed tone.
        let p = base();
        assert_eq!(p.surface, Color::Reset);
        assert!(matches!(p.bar_bg, Color::Rgb(..)));
    }

    #[test]
    fn a_named_selection_surface_wins_over_a_queried_one() {
        // `init` is a `OnceLock`, so this exercises the composition directly rather than
        // installing a palette a sibling test would then see.
        let derived = derive_backgrounds((0x0c, 0x0c, 0x0c)).0;
        assert_ne!(derived, Color::Blue);
        // What `init` does with both inputs, spelled out: the queried step first, the
        // configured colour over it.
        let mut p = base();
        let (surface, bar_bg) = derive_backgrounds((0x0c, 0x0c, 0x0c));
        p.surface = surface;
        p.bar_bg = bar_bg;
        p.surface = Color::Blue;
        assert_eq!(p.surface, Color::Blue);
        assert_eq!(p.bar_bg, bar_bg, "the hint bar keeps its own derivation");
    }

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
