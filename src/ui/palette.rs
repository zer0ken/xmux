//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and a
//! colour is changed in exactly one place.
//!
//! **The invariant: xmux never emits a colour of its own.** Every colour here is an
//! ANSI-16 slot, so the TERMINAL THEME decides the actual hue and the whole UI recolours
//! with whatever scheme the user runs. Anything that cannot be said in sixteen slots is
//! said with an ATTRIBUTE instead - reverse video, bold - which the theme also resolves.
//! A `Color::Rgb` or a `Color::Indexed` above 15 is a colour xmux picked for somebody
//! else's terminal, and it is wrong on every theme it was not picked for; a test below
//! fails if one appears.
//!
//! That is why the selection is REVERSE VIDEO rather than a raised surface. "One step
//! off the background" is not a colour sixteen slots can name, and computing one from
//! the terminal's reported background only works on terminals that answer a colour
//! query (Windows Terminal answers none). Reverse video needs no answer and no choice:
//! the terminal swaps its own foreground and background, which is exactly what a theme
//! means by "selected".
//!
//! A user who wants a specific colour names one (`[ui] selection-style`,
//! `[ui] hint-bar-style`). Their terminal, their choice.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// The semantic colour set. One field per UI role - callers name the role, never
/// a hue, so the assignments below stay changeable in one place.
pub(crate) struct Palette {
    /// The single accent: the selection mark, key hints, and popup titles all
    /// share it, so "interactive / current" is one colour everywhere.
    pub accent: Color,
    /// Secondary text: panel prose.
    pub subtext: Color,
    /// Muted furniture: the digit gutter, separators, the └/├ connectors, popup
    /// borders, the scrollbar thumb, and settled status text.
    pub overlay: Color,
    /// The hint bar's background: ANSI black, the darkest slot every theme has, so the
    /// bar reads as chrome rather than content. `[ui] hint-bar-style` overrides it.
    pub bar_bg: Color,
    /// The hint bar's text, and the text of the refusal bar. Paired with `bar_bg`, so
    /// the two are legible together in any theme that keeps its own slots legible.
    pub bar_fg: Color,
    /// Level colour: host. Blue, the slot a code theme gives a keyword, so the machine
    /// reads as the outermost level.
    pub host: Color,
    /// Level colour: mux.
    pub mux: Color,
    /// Level colour: window (the `{index}:{name}` part of the detail line) - the
    /// quietest level, so the session name reads as the detail line's anchor.
    pub window: Color,
    /// Level colour: session. Red, so the level a user actually picks stands out from
    /// the machine and mux above it. Shares the slot with [`danger`](Self::danger), which
    /// only ever paints a STATUS line - never a name - so the two never sit side by side.
    pub session: Color,
    /// In-flight state: the scanning status and the loading spinner.
    pub pending: Color,
    /// Failure state: the unreachable status and error text.
    pub danger: Color,
    /// The background `[ui] selection-style` names, or `None` for the default: reverse
    /// video, the terminal's own "selected" look. Not a colour role - a user's override
    /// of one - so it is the only field that may hold a colour xmux did not choose.
    pub selection_bg: Option<Color>,
}

/// The role-to-ANSI assignments. `DarkGray` is ANSI bright black, every theme's own muted
/// tone. The hint bar is `Black` under `White` with `Blue` keys: the plain terminal
/// combination, legible on every theme, which is what lets the bar be a solid bar of
/// chrome without xmux naming a colour of its own.
const fn base() -> Palette {
    Palette {
        accent: Color::Blue,
        subtext: Color::DarkGray,
        overlay: Color::DarkGray,
        bar_bg: Color::Black,
        bar_fg: Color::White,
        host: Color::Blue,
        mux: Color::Green,
        window: Color::DarkGray,
        session: Color::Red,
        pending: Color::Yellow,
        danger: Color::Red,
        selection_bg: None,
    }
}

/// The set served until [`init`] runs (tests, the headless dump path).
static FALLBACK: Palette = base();

/// The active set, installed once at startup. Unset reads as [`FALLBACK`].
static ACTIVE: OnceLock<Palette> = OnceLock::new();

/// Installs this run's palette, once, before any render; later calls are ignored
/// (`OnceLock`). `selection_bg` is `[ui] selection-style` - the only colour xmux takes
/// from outside the sixteen slots, because the user naming it is the one person who
/// knows their own theme.
pub(crate) fn init(selection_bg: Option<Color>) {
    let _ = ACTIVE.set(Palette {
        selection_bg,
        ..base()
    });
}

/// The active palette.
pub(crate) fn get() -> &'static Palette {
    ACTIVE.get().unwrap_or(&FALLBACK)
}

/// The style the SELECTED card is painted with.
///
/// By default reverse video, and nothing else: the terminal swaps its own foreground and
/// background, so the selection is as legible as that theme's own text and xmux picks no
/// colour. The `fg`/`bg` are pinned to `Reset` first because the swap happens per CELL -
/// left alone, a cyan session name would inverse into a cyan BACKGROUND and the card
/// would come out striped in its level colours.
///
/// `[ui] selection-style` replaces the whole thing with that background, keeping the
/// level colours on top, for a user who would rather have a surface.
pub(crate) fn selection_style() -> Style {
    selection_style_for(get().selection_bg)
}

/// [`selection_style`] as a function of the override alone, so a test can exercise both
/// branches without installing a palette: `ACTIVE` is a process-wide `OnceLock`, and one
/// test setting it would change what every other test in the binary renders.
fn selection_style_for(selection_bg: Option<Color>) -> Style {
    match selection_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED),
    }
}

/// What `xmux doctor` says about the selected card's paint. The selection is the one
/// place the palette takes an outside colour, and which of the two is in effect is
/// invisible on a screenshot, so the doctor states it.
pub(crate) fn selection_report(selection_bg: Option<Color>) -> String {
    match selection_bg {
        Some(c) => format!(
            "selected card: {} (set by [ui] selection-style)",
            describe(c)
        ),
        None => "selected card: reverse video (the terminal theme's own selected look)".to_string(),
    }
}

/// A colour as a user would write it in config: `#rrggbb` for a true colour, the slot's
/// own name otherwise. `Color`'s `Debug` spells an RGB triple as `Rgb(45, 79, 107)`,
/// which is not a value anyone can paste back into `config.toml`.
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
    fn every_colour_xmux_chooses_is_an_ansi_slot() {
        // THE invariant of this module. A `Color::Rgb`, or an `Indexed` above 15, is a
        // hue xmux picked for somebody else's terminal: it cannot follow a theme, and it
        // is wrong on every theme it was not picked for. Sixteen slots and attributes are
        // the whole vocabulary.
        let p = base();
        for (role, c) in [
            ("accent", p.accent),
            ("subtext", p.subtext),
            ("overlay", p.overlay),
            ("bar_bg", p.bar_bg),
            ("bar_fg", p.bar_fg),
            ("host", p.host),
            ("mux", p.mux),
            ("window", p.window),
            ("session", p.session),
            ("pending", p.pending),
            ("danger", p.danger),
        ] {
            let ansi = match c {
                Color::Rgb(..) => false,
                Color::Indexed(n) => n < 16,
                _ => true,
            };
            assert!(ansi, "{role} = {c:?} cannot follow the terminal theme");
        }
        assert!(
            p.selection_bg.is_none(),
            "the only colour from outside the slots is one the USER named"
        );
    }

    #[test]
    fn the_default_selection_is_the_terminals_own_reverse_video() {
        // No colour at all: the terminal swaps its own pair, so the selection is exactly
        // as legible as that theme's text. `Reset` on both sides is what keeps the swap
        // from striping the card in its level colours, cell by cell.
        let s = selection_style_for(None);
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(s.fg, Some(Color::Reset));
        assert_eq!(s.bg, Some(Color::Reset));
    }

    #[test]
    fn the_report_says_which_of_the_two_paints_the_selection() {
        // Invisible on a screenshot: a reverse-video row and a named-background row both
        // just look "selected". So the doctor names the source, and spells a colour the
        // way config does rather than as a Debug triple.
        let d = selection_report(None);
        assert!(d.contains("reverse video"), "{d}");
        let named = selection_report(Some(Color::Rgb(0x2d, 0x4f, 0x6b)));
        assert!(named.contains("[ui] selection-style"), "{named}");
        assert!(named.contains("#2d4f6b"), "{named}");
        assert!(!named.contains("reverse video"), "{named}");
    }

    #[test]
    fn a_named_selection_style_is_a_plain_background() {
        // A user who names a colour knows their own theme, so it is used as given - and
        // without REVERSED, which would invert the very colour they asked for.
        let s = selection_style_for(Some(Color::Blue));
        assert_eq!(s.bg, Some(Color::Blue));
        assert!(!s.add_modifier.contains(Modifier::REVERSED));
    }
}
