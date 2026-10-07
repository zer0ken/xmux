//! The switcher's semantic colour palette: one module naming every colour the nav
//! cards, chrome, and modals paint with, so the UI reads as one coherent theme and a
//! colour is changed in exactly one place. It is how xmux keeps Terminal-Owned Colour
//! (`docs/principles.md`).
//!
//! **The invariant: xmux never emits a colour of its own.** The palette is the sixteen
//! ANSI slots (one per UI role) plus ATTRIBUTES: reverse video, bold, dim, and underline.
//! Nothing else. The TERMINAL THEME decides the actual hue, so the whole UI recolours with
//! whatever scheme the user runs. A `Color::Rgb` or a `Color::Indexed` above 15 is a
//! colour xmux picked for somebody else's terminal, and it is wrong on every theme it was
//! not picked for; a test below fails if one appears. A new colour goes into the palette
//! as a slot, or it does not go in.
//!
//! Anything the sixteen slots cannot say is said with an attribute instead, which the
//! theme also resolves. "One step off the background" is the case that keeps coming up,
//! and it is not a slot: computing a raised surface needs the terminal's background, and
//! a terminal is free to answer no colour query at all (Windows Terminal answers none).
//! So the selection is not a raised surface but the ACCENT slot as a background, with
//! the theme's `on_accent` slot as the text on it: two slots, one look on every surface.
//!
//! A THEME is a named role→ANSI-slot assignment, and [`THEMES`] is the registry: the
//! built-ins are `auto-dark` (the default) and `auto-light`, each an ANSI-only theme for
//! a dark or a light terminal background. `[ui] theme` names one; an unknown name falls
//! back to `auto-dark`, and `xmux doctor` reports the resolution. Selecting a theme picks
//! no colours: the theme IS the slot mapping, and both ends (the `accent` on the cards,
//! the `bar_accent` on the hint bar) stay within the slots. Adding a theme is adding one
//! registry entry plus its tests, which is how the set grows without loosening the
//! invariant.
//!
//! The exceptions are colours the USER names: the per-role keys (`[ui] primary`,
//! `secondary`, `accent`, `decoration`, `warning`, `error`, `disabled`, and the hint
//! bar's `bar-bg`/`bar-fg`/`bar-accent`), plus `[ui] selection-style`,
//! `[ui] hint-bar-style`, and the view-border colours. Their terminal, their choice;
//! those user-named colours are the only ones that may leave the sixteen slots (see
//! [`Overrides`]). The chrome parses them, never this module, and the chrome's colour
//! mapping is the only place a `#rrggbb` may enter. A nonempty `NO_COLOR` resets this
//! palette and the configured chrome colours; with no accent left to paint, the
//! selection stays visible through reverse video.
//!
//! A colour a CHILD program emits passes through untouched: it is that program's own
//! choice against the same theme, and xmux is not in it.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The one bold shape every interaction screen paints a key token in, so a key reads as
/// a key wherever it is offered.
pub(crate) fn interaction_key_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// The semantic colour set. One field per UI role - callers name the role, never
/// a hue, so the assignments below stay changeable in one place. Every field is an
/// ANSI-16 slot (see the module doc) except the one the user names; a `[ui]` key can
/// override any role (see [`Overrides`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Palette {
    /// The whole view border while the nav holds focus. Its own role, apart from the
    /// card accent, so the divider is tuned independently of the screen links and
    /// session name. Also the list-failed glyph `✗` on a host-state card.
    pub primary: Color,
    /// The host/mux text of a host-state card and the state word beside it. A section
    /// title over a group of session cards uses `decoration`, so the
    /// group label stays below the sessions it names.
    pub secondary: Color,
    /// The single accent: the session name, the screen links, the popup titles, and
    /// the view border's drag-hover cue all share it, so "interactive / current" is
    /// one colour everywhere. Painted on the CARD / TERMINAL background, so it
    /// follows the theme. It is also the background of every hard selection.
    pub accent: Color,
    /// The text of a hard selection, painted on [`accent`](Self::accent). Its own role
    /// because no level colour reads on the accent: the level colours are picked to
    /// read on the terminal's background, which the accent is picked to stand out
    /// from.
    pub on_accent: Color,
    /// Content furniture: the card number, the `/` separator, the section title, the
    /// band/column rules, the popup borders, and a band's overflow counts (`‹ n` /
    /// `n ›`). All the quiet marks a card needs to read apart without being part of any
    /// level.
    pub decoration: Color,
    /// In-flight and actionable-state marks: the scanning spinner and login-needed
    /// glyph.
    pub warning: Color,
    /// Failure state: error text, the unreachable glyph `▲`, and the refusal bar's
    /// background.
    pub error: Color,
    /// The whole view border while the terminal holds focus.
    pub disabled: Color,
    /// The hint bar's background: a single ANSI slot, so the bar reads as chrome
    /// rather than content. `[ui] hint-bar-style` overrides it.
    pub bar_bg: Color,
    /// The hint bar's text, and the text of the refusal bar. Paired with `bar_bg`, so
    /// the two are legible together in any theme that keeps its own slots legible.
    pub bar_fg: Color,
    /// The hint bar's KEY accent - the prefix and each key token the bar names.
    /// Split from [`accent`](Self::accent) because the bar sits on `bar_bg` (a
    /// different surface than the cards), so the slot that reads on one may not read
    /// on the other: a light theme's dark `accent` is invisible on a dark bar.
    pub bar_accent: Color,
    /// The background `[ui] selection-style` names, or `None` for the default: the
    /// accent. Not a colour role - a user's override of one - so it is the only field
    /// that may hold a colour xmux did not choose.
    pub selection_bg: Option<Color>,
}

/// The light built-in theme's name; the dark one is the config default
/// (`auto-dark`). `[ui] theme` names one; `auto` is not a mode, the two names ARE the
/// two ANSI-only themes - `auto-light` for a light terminal, `auto-dark` for a dark one, each following the terminal's own palette by painting only
/// ANSI slots. See the module doc.
pub(crate) const AUTO_LIGHT: &str = "auto-light";

/// `auto-dark`: for a dark terminal background. Painted with the dark-slot ends of the
/// ANSI set - the level colours read on black, the accent pops on it. A selection is
/// Black on the bright accent: a dark theme keeps its Black slot near its background,
/// which is what the bright accent is picked to stand out from.
const fn auto_dark() -> Palette {
    Palette {
        primary: Color::White,
        secondary: Color::Gray,
        accent: Color::LightGreen,
        on_accent: Color::Black,
        decoration: Color::DarkGray,
        warning: Color::Yellow,
        error: Color::LightRed,
        disabled: Color::DarkGray,
        bar_bg: Color::DarkGray,
        bar_fg: Color::White,
        bar_accent: Color::White,
        selection_bg: None,
    }
}

/// `auto-light`: for a light terminal background. Painted with the dark-slot ends of
/// the ANSI set (a light background washes the bright slots out), so the level colours
/// and the accent read against white; the hint bar keeps the dark slots that read on a
/// bar of its own. A selection is Black on the Green accent: a light theme's Green is a
/// mid green, and Black, the theme's own text colour, reads on it where White, close to
/// the theme's background, washes out.
const fn auto_light() -> Palette {
    Palette {
        primary: Color::Black,
        secondary: Color::DarkGray,
        accent: Color::Green,
        on_accent: Color::Black,
        decoration: Color::Gray,
        warning: Color::Yellow,
        error: Color::Red,
        disabled: Color::Gray,
        bar_bg: Color::DarkGray,
        bar_fg: Color::White,
        bar_accent: Color::White,
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
    (crate::provision::config::DEFAULT_THEME, &AUTO_DARK_THEME),
    (AUTO_LIGHT, &AUTO_LIGHT_THEME),
];

/// Resolves a theme name to its canonical name and [`Palette`]. Unknown names resolve
/// to `None`, leaving the caller's fallback (the default `auto-dark`) to apply.
pub(crate) fn resolve_theme(name: &str) -> Option<(&'static str, &'static Palette)> {
    THEMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, p)| (*n, *p))
}

/// Resolves a theme name, or the default `auto-dark` when unknown.
fn resolve_or_default(name: &str) -> (&'static str, &'static Palette) {
    resolve_theme(name).unwrap_or((crate::provision::config::DEFAULT_THEME, &AUTO_DARK_THEME))
}

/// The per-role overrides a user can name in `[ui]`; `None` leaves that role at the
/// theme's own slot. The only colours xmux takes from outside the sixteen slots are
/// ones the USER names, because the user naming one is the one person who knows their
/// own theme (see the module doc).
#[derive(Default, Clone, Copy)]
pub(crate) struct Overrides {
    pub primary: Option<Color>,
    pub secondary: Option<Color>,
    pub accent: Option<Color>,
    pub decoration: Option<Color>,
    pub warning: Option<Color>,
    pub error: Option<Color>,
    pub disabled: Option<Color>,
    pub bar_bg: Option<Color>,
    pub bar_fg: Option<Color>,
    pub bar_accent: Option<Color>,
    /// `[ui] selection-style`, parsed to `None` when unset (the accent).
    pub selection_bg: Option<Color>,
}

/// Resolves a theme and layers the user's overrides over it. An unknown theme name
/// falls back to `auto-dark`; each `Some` in `ov` replaces that role's slot, and each
/// `None` keeps the theme's own.
pub(crate) fn resolve(theme: &str, ov: Overrides) -> Palette {
    let (_name, base) = resolve_or_default(theme);
    apply_overrides(*base, ov)
}

pub(crate) fn resolve_output(theme: &str, ov: Overrides) -> Palette {
    let palette = resolve(theme, ov);
    if no_color() {
        without_color(palette)
    } else {
        palette
    }
}

fn without_color(mut palette: Palette) -> Palette {
    palette.primary = Color::Reset;
    palette.secondary = Color::Reset;
    palette.accent = Color::Reset;
    palette.on_accent = Color::Reset;
    palette.decoration = Color::Reset;
    palette.warning = Color::Reset;
    palette.error = Color::Reset;
    palette.disabled = Color::Reset;
    palette.bar_bg = Color::Reset;
    palette.bar_fg = Color::Reset;
    palette.bar_accent = Color::Reset;
    palette.selection_bg = None;
    palette
}

pub(crate) fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

fn apply_overrides(base: Palette, ov: Overrides) -> Palette {
    let mut p = base;
    p.primary = ov.primary.unwrap_or(p.primary);
    p.secondary = ov.secondary.unwrap_or(p.secondary);
    p.accent = ov.accent.unwrap_or(p.accent);
    p.decoration = ov.decoration.unwrap_or(p.decoration);
    p.warning = ov.warning.unwrap_or(p.warning);
    p.error = ov.error.unwrap_or(p.error);
    p.disabled = ov.disabled.unwrap_or(p.disabled);
    p.bar_bg = ov.bar_bg.unwrap_or(p.bar_bg);
    p.bar_fg = ov.bar_fg.unwrap_or(p.bar_fg);
    p.bar_accent = ov.bar_accent.unwrap_or(p.bar_accent);
    p.selection_bg = ov.selection_bg;
    p
}

impl Default for Palette {
    fn default() -> Self {
        resolve(
            crate::provision::config::DEFAULT_THEME,
            Overrides::default(),
        )
    }
}

/// The style a popup item or a help tab under the pointer is painted with, the soft
/// selection: an underline, which reads apart from the hard selection's accent background
/// and sits on top of it when both mark one item. An attribute, so the theme resolves it.
pub(crate) fn soft_selection_style() -> Style {
    Style::default().add_modifier(Modifier::UNDERLINED)
}

/// The style every hard selection is painted with: a nav card or the half of a section
/// title, a screen link, a help tab, a list row, and a focused login stop.
///
/// By default the theme's `on_accent` text on its `accent` background over every cell of
/// the item: the foreground is pinned along with the background, and dim and reverse
/// video are cleared, so no level colour, dim number, or swapped cell of the item survives
/// inside the highlight and the selection reads as one look on every surface.
///
/// With no accent to paint (`NO_COLOR`, or an accent named `default`) the selection is
/// reverse video over the terminal's own pair instead, which needs no colour at all. The
/// `fg`/`bg` are pinned to `Reset` there because the swap happens per cell: left alone, a
/// cyan session name would invert into a cyan background.
///
/// `[ui] selection-style` replaces the accent with that background, keeping the level
/// colours on top, for a user who names a surface of their own.
pub(crate) fn selection_style(palette: &Palette) -> Style {
    match palette.selection_bg {
        Some(bg) => Style::default().bg(bg),
        None if palette.accent == Color::Reset => Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED),
        None => Style::default()
            .fg(palette.on_accent)
            .bg(palette.accent)
            .remove_modifier(Modifier::DIM | Modifier::REVERSED),
    }
}

/// `style` as the hard selection paints it: the selection style patched over it, so the
/// surface's own colour never stays under the highlight as a second background.
pub(crate) fn selected(style: Style, palette: &Palette) -> Style {
    style.patch(selection_style(palette))
}

/// `line` as the hard selection paints it: [`selected`] over the line and over each of its
/// spans, so every cell the line covers reads as the one selected look.
pub(crate) fn selected_line(line: Line<'static>, palette: &Palette) -> Line<'static> {
    let style = selected(line.style, palette);
    let spans = line
        .spans
        .into_iter()
        .map(|span| {
            let style = selected(span.style, palette);
            span.style(style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(style)
}

/// Whether `style` is the hard selection's paint: it carries the selection's background.
fn is_selected(style: Style, palette: &Palette) -> bool {
    let bg = selection_style(palette).bg;
    bg.is_some() && style.bg == bg
}

/// `line` with each run of hard-selected cells padded by one blank cell on each side, so
/// the highlight never sits tight on the item's text. A side whose cell in the run is
/// already blank is padded; otherwise the neighbouring cell takes the run's paint when
/// it is blank, and a run that ends the line gains one blank cell after it. A neighbour
/// that is text stays as it is, so the padding never moves a character.
pub(crate) fn pad_selected(line: Line<'static>, palette: &Palette) -> Line<'static> {
    // One entry per character: the span it came from, the character, and its style.
    let mut cells: Vec<(usize, char, Style)> = line
        .spans
        .iter()
        .enumerate()
        .flat_map(|(i, span)| span.content.chars().map(move |c| (i, c, span.style)))
        .collect();
    let selected = |cells: &[(usize, char, Style)], at: usize| is_selected(cells[at].2, palette);
    let mut at = 0;
    while at < cells.len() {
        if !selected(&cells, at) {
            at += 1;
            continue;
        }
        let start = at;
        while at < cells.len() && selected(&cells, at) {
            at += 1;
        }
        let end = at;
        if cells[start].1 != ' ' && start > 0 && cells[start - 1].1 == ' ' {
            cells[start - 1].2 = cells[start].2;
            cells[start - 1].0 = usize::MAX - 1;
        }
        if cells[end - 1].1 != ' ' {
            if end == cells.len() {
                cells.push((usize::MAX, ' ', cells[end - 1].2));
            } else if cells[end].1 == ' ' {
                cells[end].2 = cells[end - 1].2;
                cells[end].0 = usize::MAX;
                at += 1;
            }
        }
    }
    // Rebuilt span by span: a padding cell is a span of its own, so every span the
    // surface built keeps its own text.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut last: Option<(usize, Style)> = None;
    for (origin, c, style) in cells {
        match spans.last_mut() {
            Some(span) if last == Some((origin, style)) => span.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
        last = Some((origin, style));
    }
    Line::from(spans).style(line.style)
}

/// The cell offset of the last hard-selected cell in `line`.
pub(crate) fn last_selected_cell(line: &Line, palette: &Palette) -> Option<u16> {
    let mut x = 0usize;
    let mut found = None;
    for span in &line.spans {
        let w = span.width();
        if w > 0 && is_selected(span.style, palette) {
            found = Some(x + w - 1);
        }
        x += w;
    }
    found.and_then(|x| u16::try_from(x).ok())
}

/// What `xmux doctor` says about the selected card's paint. The selection is the one
/// place the palette takes an outside colour, and which background is in effect cannot be
/// told from a screenshot, so the doctor states it.
pub(crate) fn selection_report(palette: &Palette) -> String {
    match palette.selection_bg {
        Some(c) => format!(
            "selected card: {} (set by [ui] selection-style)",
            describe(c)
        ),
        None if palette.accent == Color::Reset => {
            "selected card: reverse video (no accent colour to paint)".to_string()
        }
        None => format!(
            "selected card: {} on the accent {}",
            describe(palette.on_accent),
            describe(palette.accent)
        ),
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
    fn monochrome_palette_resets_every_colour_and_keeps_selection_visible() {
        let p = without_color(auto_dark());
        for color in [
            p.primary,
            p.secondary,
            p.accent,
            p.decoration,
            p.warning,
            p.error,
            p.disabled,
            p.bar_bg,
            p.bar_fg,
            p.bar_accent,
        ] {
            assert_eq!(color, Color::Reset);
        }
        assert_eq!(p.on_accent, Color::Reset);
        assert!(selection_style(&p)
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn every_colour_every_theme_chooses_is_an_ansi_slot() {
        // THE invariant of this module. A `Color::Rgb`, or an `Indexed` above 15, is a
        // hue xmux picked for somebody else's terminal: it cannot follow a theme, and it
        // is wrong on every theme it was not picked for. Sixteen slots and attributes are
        // the whole slot set. Held for EVERY built-in theme, so a theme added later
        // carries the guard with it.
        for p in [auto_dark(), auto_light()] {
            for (role, c) in [
                ("primary", p.primary),
                ("secondary", p.secondary),
                ("accent", p.accent),
                ("on_accent", p.on_accent),
                ("decoration", p.decoration),
                ("warning", p.warning),
                ("error", p.error),
                ("disabled", p.disabled),
                ("bar_bg", p.bar_bg),
                ("bar_fg", p.bar_fg),
                ("bar_accent", p.bar_accent),
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
        assert_eq!(p.primary, Color::White);
        assert_eq!(p.secondary, Color::Gray);
        assert_eq!(p.accent, Color::LightGreen);
        assert_eq!(p.on_accent, Color::Black);
        assert_eq!(p.decoration, Color::DarkGray);
        assert_eq!(p.warning, Color::Yellow);
        assert_eq!(p.error, Color::LightRed);
        assert_eq!(p.disabled, Color::DarkGray);
        assert_eq!(p.bar_bg, Color::DarkGray);
        assert_eq!(p.bar_fg, Color::White);
        assert_eq!(p.bar_accent, Color::White);
        let (name, p) = resolve_theme("auto-light").unwrap();
        assert_eq!(name, "auto-light");
        assert_eq!(p.primary, Color::Black);
        assert_eq!(p.secondary, Color::DarkGray);
        assert_eq!(p.accent, Color::Green);
        assert_eq!(p.on_accent, Color::Black);
        assert_eq!(p.decoration, Color::Gray);
        assert_eq!(p.warning, Color::Yellow);
        assert_eq!(p.error, Color::Red);
        assert_eq!(p.disabled, Color::Gray);
        assert_eq!(p.bar_bg, Color::DarkGray);
        assert_eq!(p.bar_fg, Color::White);
        assert_eq!(p.bar_accent, Color::White);
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
    fn the_default_selection_is_the_on_accent_text_on_the_accent() {
        // Each theme's own pair, pinned on both sides and clearing dim and reverse video,
        // so nothing the item painted on its cells survives inside the highlight.
        for (theme, fg, bg) in [
            ("auto-dark", Color::Black, Color::LightGreen),
            ("auto-light", Color::Black, Color::Green),
        ] {
            let s = selection_style(&resolve(theme, Overrides::default()));
            assert_eq!((s.fg, s.bg), (Some(fg), Some(bg)), "{theme}");
            assert!(s.sub_modifier.contains(Modifier::DIM), "{theme}");
            assert!(s.sub_modifier.contains(Modifier::REVERSED), "{theme}");
            assert!(!s.add_modifier.contains(Modifier::REVERSED), "{theme}");
        }
    }

    #[test]
    fn the_selection_follows_a_user_named_accent() {
        // The selection IS the accent: naming another accent recolours the highlight with
        // it, and naming none (`default`) leaves reverse video, which needs no colour.
        let s = selection_style(&resolve(
            "auto-dark",
            Overrides {
                accent: Some(Color::Cyan),
                ..Default::default()
            },
        ));
        assert_eq!(s.bg, Some(Color::Cyan));
        let s = selection_style(&resolve(
            "auto-dark",
            Overrides {
                accent: Some(Color::Reset),
                ..Default::default()
            },
        ));
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert_eq!((s.fg, s.bg), (Some(Color::Reset), Some(Color::Reset)));
    }

    #[test]
    fn an_override_replaces_only_that_role_and_leaves_the_rest() {
        // `[ui] primary` names one role: it replaces that slot on the chosen theme and
        // every other role keeps the theme's own. `None` in an override means "the
        // theme's slot", not a reset.
        let p = resolve(
            "auto-dark",
            Overrides {
                primary: Some(Color::Red),
                ..Default::default()
            },
        );
        assert_eq!(p.primary, Color::Red);
        assert_eq!(p.secondary, auto_dark().secondary);
        assert_eq!(p.accent, auto_dark().accent);
        // selection_bg is replaced as given, even by None: that is the accent default
        // rather than "keep what was there".
        let p = resolve("auto-dark", Overrides::default());
        assert_eq!(p, auto_dark());
    }

    #[test]
    fn the_report_says_which_of_the_two_paints_the_selection() {
        // Invisible on a screenshot: an accent row and a named-background row both just
        // look "selected". So the doctor names the source, and spells a colour the way
        // config does rather than as a Debug triple.
        let d = selection_report(&auto_dark());
        assert!(d.contains("black on the accent lightgreen"), "{d}");
        let named = selection_report(&Palette {
            selection_bg: Some(Color::Rgb(0x2d, 0x4f, 0x6b)),
            ..auto_dark()
        });
        assert!(named.contains("[ui] selection-style"), "{named}");
        assert!(named.contains("#2d4f6b"), "{named}");
        assert!(!named.contains("accent"), "{named}");
        let bare = selection_report(&without_color(auto_dark()));
        assert!(bare.contains("reverse video"), "{bare}");
    }

    #[test]
    fn padding_takes_a_blank_neighbour_and_never_moves_text() {
        let p = auto_dark();
        let lit = selected(Style::default(), &p);
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let lit_text = |line: &Line| {
            line.spans
                .iter()
                .filter(|s| is_selected(s.style, &p))
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        // Blank cells on both sides take the paint.
        let line = pad_selected(
            Line::from(vec![
                Span::raw("a  "),
                Span::styled("tab", lit),
                Span::raw("  b"),
            ]),
            &p,
        );
        assert_eq!(text(&line), "a  tab  b");
        assert_eq!(lit_text(&line), " tab ");
        // Text beside the run stays as it is, and a run that ends the line gains a cell.
        let line = pad_selected(
            Line::from(vec![Span::raw("x/"), Span::styled("host", lit)]),
            &p,
        );
        assert_eq!(text(&line), "x/host ");
        assert_eq!(lit_text(&line), "host ");
        // A run that already opens and closes on a blank cell is left alone.
        let line = pad_selected(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(" (*) a ", lit),
                Span::raw(" "),
            ]),
            &p,
        );
        assert_eq!(lit_text(&line), " (*) a ");
        assert_eq!(last_selected_cell(&line, &p), Some(8));
    }

    #[test]
    fn a_named_selection_style_is_a_plain_background() {
        // A user who names a colour knows their own theme, so it is used as given - and
        // without REVERSED, which would invert the very colour they asked for.
        let s = selection_style(&Palette {
            selection_bg: Some(Color::Blue),
            ..auto_dark()
        });
        assert_eq!(s.bg, Some(Color::Blue));
        assert!(!s.add_modifier.contains(Modifier::REVERSED));
    }
}
