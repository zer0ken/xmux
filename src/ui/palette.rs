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

use std::sync::RwLock;

use ratatui::style::{Color, Modifier, Style};

/// The semantic colour set. One field per UI role - callers name the role, never
/// a hue, so the assignments below stay changeable in one place.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    /// The single accent: the selection mark, popup titles, and the view border's
    /// FOCUS half all share it, so "interactive / current" is one colour everywhere.
    /// Painted on the CARD / TERMINAL background, so it follows the theme.
    pub accent: Color,
    /// Content furniture, one role for the quiet supporting marks on the cards: the
    /// "no sessions" status word, the `/` separator, the card number,
    /// the popup borders, and the scrollbar thumb. All the marks a card
    /// needs to read apart without being part of any level - see `bar_accent` and
    /// `border_inactive` for the surfaces that live on a background of their own.
    pub overlay: Color,
    /// The view border's FOCUS half - the lit side of the nav|mux divider that marks
    /// which view holds focus. Its own role, apart from the card accent, so the divider
    /// is tuned independently of the selection mark / popup titles.
    pub border_active: Color,
    /// The view border's NON-focus half - the dim side of the nav|mux divider that
    /// marks which half does NOT hold focus.
    pub border_inactive: Color,
    /// The view border's DRAG-HOVER cue (the grab handle that appears while the mouse
    /// hovers the rule). Its own role, no longer a fixed yellow.
    pub border_hover: Color,
    /// The card's NUMBER in the left column (the digit gutter) - the address a user
    /// jumps to. Split from `overlay` so it can be tuned apart from the other marks.
    pub number: Color,
    /// The `/` separator between the host/mux/session parts of a card's lines.
    pub separator: Color,
    /// The scroll-overflow cue (`« n more` / `n more »`) when cards run off the band.
    pub more: Color,
    /// The hint bar's background: a single ANSI slot, so the bar reads as chrome
    /// rather than content. `[ui] hint-bar-style` overrides it.
    pub bar_bg: Color,
    /// The hint bar's text, and the text of the refusal bar. Paired with `bar_bg`, so
    /// the two are legible together in any theme that keeps its own slots legible.
    pub bar_fg: Color,
    /// The hint bar's KEY accent - the prefix and each key token in the cheatsheet.
    /// Split from [`accent`](Self::accent) because the bar sits on `bar_bg` (a
    /// different surface than the cards), so the slot that reads on one may not read
    /// on the other: a light theme's dark `accent` is invisible on a dark bar.
    pub bar_accent: Color,
    /// The one text colour a host-state card paints, separators and the accent target
    /// excepted: the host half of the card reads in it, so a card reads as one neutral
    /// block with a single highlighted element. The accent target (the session name, or
    /// the mux on a host-state card) is the only card text that leaves it; a section
    /// title reads in the quiet `overlay` header role instead.
    pub text: Color,
    /// In-flight state: the scanning status and the loading spinner.
    pub pending: Color,
    /// Failure state: the unreachable status and error text.
    pub danger: Color,
    /// The background `[ui] selection-style` names, or `None` for the default: reverse
    /// video, the terminal's own "selected" look. Not a colour role - a user's override
    /// of one - so it is the only field that may hold a colour xmux did not choose.
    pub selection_bg: Option<Color>,
}

/// The two built-in themes' names. `[ui] theme` names one; `auto` is not a mode, the
/// two names ARE the two ANSI-only themes - `auto-light` for a light terminal, `auto-
/// dark` for a dark one, each following the terminal's own palette by painting only
/// ANSI slots. See the module doc and `Colour ownership` in `CONTEXT.md`.
pub(crate) const AUTO_DARK: &str = "auto-dark";
pub(crate) const AUTO_LIGHT: &str = "auto-light";

/// `auto-dark`: for a dark terminal background. Painted with the dark-slot ends of the
/// ANSI set - the level colours read on black, the accent pops on it.
const fn auto_dark() -> Palette {
    Palette {
        accent: Color::LightGreen,
        overlay: Color::DarkGray,
        border_active: Color::White,
        border_inactive: Color::DarkGray,
        border_hover: Color::LightGreen,
        number: Color::DarkGray,
        separator: Color::DarkGray,
        more: Color::DarkGray,
        bar_bg: Color::DarkGray,
        bar_fg: Color::White,
        bar_accent: Color::White,
        text: Color::White,
        pending: Color::Yellow,
        danger: Color::LightYellow,
        selection_bg: None,
    }
}

/// `auto-light`: for a light terminal background. Painted with the dark-slot ends of
/// the ANSI set (a light background washes the bright slots out), so the level colours
/// and the accent read against white; the hint bar keeps the dark slots that read on a
/// bar of its own.
const fn auto_light() -> Palette {
    Palette {
        accent: Color::Green,
        overlay: Color::DarkGray,
        border_active: Color::Black,
        border_inactive: Color::White,
        border_hover: Color::Green,
        number: Color::DarkGray,
        separator: Color::DarkGray,
        more: Color::DarkGray,
        bar_bg: Color::DarkGray,
        bar_fg: Color::White,
        bar_accent: Color::White,
        text: Color::Black,
        pending: Color::LightYellow,
        danger: Color::Yellow,
        selection_bg: None,
    }
}

/// The two built-in themes as statics, so [`THEMES`] and the fallback can hold
/// `&'static` references to them.
static AUTO_DARK_THEME: Palette = auto_dark();
static AUTO_LIGHT_THEME: Palette = auto_light();

/// The theme registry. A theme is a role→ANSI-slot assignment, and adding a theme is
/// adding one entry here (plus its tests). The two built-ins are ANSI-only by the
/// invariant; a future theme that names a colour of its own would carry that exception
/// on itself rather than loosening the guard.
pub(crate) static THEMES: &[(&str, &Palette)] = &[
    (AUTO_DARK, &AUTO_DARK_THEME),
    (AUTO_LIGHT, &AUTO_LIGHT_THEME),
];

/// The active set, installable again on every config change so a `[ui]` edit applies
/// live. A read guard on the render path is a cheap atomic; the write happens once per
/// config reload, off the render's read path.
static ACTIVE: RwLock<Palette> = RwLock::new(auto_dark());

/// Resolves a theme name to its canonical name and [`Palette`]. Unknown names resolve
/// to `None`, leaving the caller's fallback (the default `auto-dark`) to apply.
pub(crate) fn resolve_theme(name: &str) -> Option<(&'static str, &'static Palette)> {
    THEMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, p)| (*n, *p))
}

/// Resolves a theme name, or the default `auto-dark` when unknown. Split from
/// [`apply`] so the fallback is testable without touching the process-wide lock.
fn resolve_or_default(name: &str) -> (&'static str, &'static Palette) {
    resolve_theme(name).unwrap_or((AUTO_DARK, &AUTO_DARK_THEME))
}

/// Installs a theme + selection override, REPLACING the active palette. Called at
/// startup and again on every config change, so a `[ui] theme` / `selection-style`
/// edit applies live. `theme` names a built-in theme (an unknown name falls back to
/// `auto-dark`); `selection_bg` is `[ui] selection-style` - the only colour xmux takes
/// from outside the sixteen slots, because the user naming it is the one person who
/// knows their own theme.
pub(crate) fn apply(theme: &str, selection_bg: Option<Color>) {
    let (_name, base) = resolve_or_default(theme);
    *ACTIVE.write().unwrap() = Palette {
        selection_bg,
        ..*base
    };
}

/// The active palette.
pub(crate) fn get() -> std::sync::RwLockReadGuard<'static, Palette> {
    ACTIVE.read().unwrap()
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
    fn every_colour_every_theme_chooses_is_an_ansi_slot() {
        // THE invariant of this module. A `Color::Rgb`, or an `Indexed` above 15, is a
        // hue xmux picked for somebody else's terminal: it cannot follow a theme, and it
        // is wrong on every theme it was not picked for. Sixteen slots and attributes are
        // the whole slot set. Held for EVERY built-in theme, so a theme added later
        // carries the guard with it.
        for p in [auto_dark(), auto_light()] {
            for (role, c) in [
                ("accent", p.accent),
                ("overlay", p.overlay),
                ("border_active", p.border_active),
                ("border_inactive", p.border_inactive),
                ("border_hover", p.border_hover),
                ("number", p.number),
                ("separator", p.separator),
                ("more", p.more),
                ("bar_bg", p.bar_bg),
                ("bar_fg", p.bar_fg),
                ("bar_accent", p.bar_accent),
                ("text", p.text),
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
    }

    #[test]
    fn the_registry_names_the_two_builtin_themes() {
        // The two ANSI-only themes are the whole current set. A new theme is a new
        // entry; this test names what is on offer so a rename is a test change.
        assert_eq!(theme_names_impl(), vec!["auto-dark", "auto-light"]);
    }

    #[test]
    fn resolve_theme_resolves_both_and_rejects_unknown() {
        let (name, p) = resolve_theme("auto-dark").unwrap();
        assert_eq!(name, "auto-dark");
        assert_eq!(p.accent, Color::LightGreen);
        assert_eq!(p.text, Color::White);
        assert_eq!(p.border_hover, Color::LightGreen);
        assert_eq!(p.overlay, Color::DarkGray);
        assert_eq!(p.danger, Color::LightYellow);
        assert_eq!(p.pending, Color::Yellow);
        let (name, p) = resolve_theme("auto-light").unwrap();
        assert_eq!(name, "auto-light");
        assert_eq!(p.accent, Color::Green);
        assert_eq!(p.overlay, Color::DarkGray);
        assert_eq!(p.border_inactive, Color::White);
        assert_eq!(p.text, Color::Black);
        assert_eq!(p.danger, Color::Yellow);
        assert_eq!(p.pending, Color::LightYellow);
        assert!(resolve_theme("nope").is_none());
        assert!(resolve_theme("").is_none());
    }

    #[test]
    fn unknown_theme_name_falls_back_to_auto_dark() {
        // `[ui] theme` naming something xmux does not ship must not paint a broken UI:
        // it falls back to the safe dark theme, and the doctor reports the resolution.
        let (name, p) = resolve_or_default("nope");
        assert_eq!(name, "auto-dark");
        assert_eq!(p.accent, auto_dark().accent);
        let (name, p) = resolve_or_default("auto-light");
        assert_eq!(name, "auto-light");
        assert_eq!(p.accent, auto_light().accent);
    }

    fn theme_names_impl() -> Vec<&'static str> {
        THEMES.iter().map(|(n, _)| *n).collect()
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
