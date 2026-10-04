//! Rendering and placement of the toasts and the history popup.
//! [`State`](crate::state::State) owns the [`Notifications`] this module paints.

use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::state::notify::{Level, Note, Notifications, Toast};
use crate::ui::modal::wrap_text;
use crate::ui::palette::Palette;
use crate::ui::switcher::NavPosition;

/// The widest a toast may be, as a share of the window: wide enough for a host name and a
/// reason, narrow enough that the terminal it floats over stays readable.
const TOAST_MAX_PERCENT: u16 = 40;

/// The narrowest toast worth drawing: a glyph, a space, and a few cells of words inside
/// the border. A terminal view too small for it gets no toast; the history still has it.
const TOAST_MIN_WIDTH: u16 = 12;

/// The cells a note line spends before its words: a margin, the glyph, a space.
const NOTE_LEAD: usize = 3;

/// The colour of a level's glyph.
fn level_style(level: Level, palette: &Palette) -> Style {
    let color = match level {
        Level::Success => palette.accent,
        Level::Info => palette.decoration,
        Level::Warning => palette.warning,
        Level::Error => palette.error,
    };
    Style::default().fg(color)
}

/// One note as the lines it takes at `inner` cells: the glyph leads the first, and the
/// words wrap under themselves rather than under the glyph.
fn note_lines(note: &Note, inner: u16, palette: &Palette) -> Vec<Line<'static>> {
    let words = (inner as usize).saturating_sub(NOTE_LEAD + 1).max(1) as u16;
    note.text
        .lines()
        .flat_map(|line| wrap_text(line, words))
        .enumerate()
        .map(|(i, chunk)| {
            if i == 0 {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(note.level.glyph(), level_style(note.level, palette)),
                    Span::raw(format!(" {chunk}")),
                ])
            } else {
                Line::from(format!("{:NOTE_LEAD$}{chunk}", ""))
            }
        })
        .collect()
}

/// The size of `toast` when it may be at most `max_w` cells wide: as wide as its longest
/// note or its title needs, and as tall as its wrapped notes plus the border.
fn toast_size(toast: &Toast, max_w: u16) -> (u16, u16) {
    let words = toast
        .notes
        .iter()
        .flat_map(|n| n.text.lines())
        .map(|line| UnicodeWidthStr::width(line) + NOTE_LEAD + 1)
        .max()
        .unwrap_or(0);
    let title = UnicodeWidthStr::width(toast.title.as_str()) + 4;
    let w = ((words.max(title) + 2) as u16).min(max_w);
    let inner = w.saturating_sub(2);
    let body: usize = toast
        .notes
        .iter()
        .map(|n| note_lines(n, inner, &Palette::default()).len())
        .sum();
    (w, body as u16 + 2)
}

/// Where each toast floats: the terminal view's top corner farthest from the nav, or its
/// bottom right corner when the nav rides on top. The newest toast takes the corner and
/// older ones stack away from it while they fit; a toast that does not fit is left for
/// the history. No toast is wider than [`TOAST_MAX_PERCENT`] of the window.
pub(crate) fn place_toasts(
    notify: &Notifications,
    terminal: Rect,
    window: Rect,
    position: NavPosition,
) -> Vec<(u64, Rect)> {
    let max_w = (window.width * TOAST_MAX_PERCENT / 100).min(terminal.width);
    if max_w < TOAST_MIN_WIDTH || terminal.height < 3 {
        return Vec::new();
    }
    let from_bottom = position == NavPosition::Top;
    let at_left = position == NavPosition::Right;
    let mut placed = Vec::new();
    let mut used = 0u16;
    for toast in notify.toasts.iter().rev() {
        let (w, h) = toast_size(toast, max_w);
        if used + h > terminal.height {
            break;
        }
        let x = if at_left {
            terminal.x
        } else {
            terminal.right() - w
        };
        let y = if from_bottom {
            terminal.bottom() - used - h
        } else {
            terminal.y + used
        };
        placed.push((toast.id, Rect::new(x, y, w, h)));
        used += h;
    }
    placed
}

/// Paints one toast: a rounded box titled with what it reports on, its notes inside, and
/// the key that opens the history on its bottom border. A toast that leaves by itself
/// counts down on its first line: an underline across the share of its life still ahead.
pub(crate) fn render_toast(
    frame: &mut Frame,
    rect: Rect,
    toast: &Toast,
    now: Option<Instant>,
    prefix: &str,
    palette: &Palette,
) {
    let inner = rect.width.saturating_sub(2);
    let lines: Vec<Line> = toast
        .notes
        .iter()
        .flat_map(|n| note_lines(n, inner, palette))
        .collect();
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.decoration))
        .style(Style::reset());
    if !toast.title.is_empty() {
        block = block.title(Span::styled(
            format!(" {} ", toast.title),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let footer = format!(" {prefix} m history ");
    if UnicodeWidthStr::width(footer.as_str()) as u16 + 2 <= rect.width {
        block = block.title_bottom(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{prefix} m"),
                crate::ui::palette::interaction_key_style(),
            ),
            Span::styled(" history ", Style::default().fg(palette.decoration)),
        ]));
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    let left = now.and_then(|now| toast.remaining(now));
    if let Some(left) = left {
        let cells = (left * inner as f32).ceil() as u16;
        let buf = frame.buffer_mut();
        for x in rect.x + 1..rect.x + 1 + cells.min(inner) {
            let cell = &mut buf[(x, rect.y + 1)];
            cell.set_style(cell.style().add_modifier(Modifier::UNDERLINED));
        }
    }
}

/// How long ago `at` was, in the largest whole unit: `12s`, `4m`, `2h`, `3d`.
fn age(now: Option<Instant>, at: Instant) -> String {
    let secs = now.map_or(0, |now| now.saturating_duration_since(at).as_secs());
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// The history popup's `(title, lines)` at `width` cells inside the border: the newest
/// record first, `scroll` records down. Each record is its age, its glyph, what it was
/// about, and its words, wrapped under the words.
pub(crate) fn history_lines(
    notify: &Notifications,
    scroll: usize,
    width: u16,
    palette: &Palette,
) -> (String, Vec<Line<'static>>) {
    if notify.history.is_empty() {
        return (
            "history".to_string(),
            vec![Line::from(Span::styled(
                " nothing yet: results and background events land here",
                Style::default().fg(palette.decoration),
            ))],
        );
    }
    let aw = 4;
    let lead = aw + 4;
    let words = (width as usize).saturating_sub(lead + 1).max(1) as u16;
    let lines = notify
        .history
        .iter()
        .rev()
        .skip(scroll.min(notify.history.len() - 1))
        .flat_map(|entry| {
            let text = if entry.title.is_empty() {
                entry.note.text.clone()
            } else {
                format!("{} · {}", entry.title, entry.note.text)
            };
            text.lines()
                .flat_map(|line| wrap_text(line, words))
                .enumerate()
                .map(|(i, chunk)| {
                    if i == 0 {
                        Line::from(vec![
                            Span::styled(
                                format!(" {:>aw$} ", age(notify.now, entry.at)),
                                Style::default().fg(palette.decoration),
                            ),
                            Span::styled(
                                entry.note.level.glyph(),
                                level_style(entry.note.level, palette),
                            ),
                            Span::raw(format!(" {chunk}")),
                        ])
                    } else {
                        Line::from(format!("{:lead$}{chunk}", ""))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    (format!("history · {}", notify.history.len()), lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::notify::Note;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn notify_with(titles: &[&str]) -> Notifications {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        for t in titles {
            n.toast_at(
                t0,
                *t,
                vec![Note::new(Level::Error, "login failed: denied")],
            );
        }
        n
    }

    /// The four positions, each with its terminal view, in a 100x30 window.
    fn terminal_for(position: NavPosition) -> Rect {
        match position {
            NavPosition::Left => Rect::new(31, 0, 69, 30),
            NavPosition::Right => Rect::new(0, 0, 69, 30),
            NavPosition::Top => Rect::new(0, 11, 100, 19),
            NavPosition::Bottom => Rect::new(0, 0, 100, 19),
        }
    }

    #[test]
    fn a_toast_floats_in_the_terminal_corner_farthest_from_the_nav() {
        let window = Rect::new(0, 0, 100, 30);
        let n = notify_with(&["gpu-02"]);
        for (position, corner) in [
            (NavPosition::Left, "top right"),
            (NavPosition::Right, "top left"),
            (NavPosition::Bottom, "top right"),
            (NavPosition::Top, "bottom right"),
        ] {
            let terminal = terminal_for(position);
            let placed = place_toasts(&n, terminal, window, position);
            assert_eq!(placed.len(), 1, "{position:?}");
            let r = placed[0].1;
            let (left, top) = match corner {
                "top right" => (r.right() == terminal.right(), r.y == terminal.y),
                "top left" => (r.x == terminal.x, r.y == terminal.y),
                _ => (
                    r.right() == terminal.right(),
                    r.bottom() == terminal.bottom(),
                ),
            };
            assert!(
                left && top,
                "{position:?} puts it {corner}: {r:?} in {terminal:?}"
            );
            assert!(
                r.width <= 40,
                "{position:?}: at most 40% of the window: {r:?}"
            );
        }
    }

    #[test]
    fn newer_toasts_take_the_corner_and_older_ones_stack_away_from_it() {
        let window = Rect::new(0, 0, 100, 30);
        let n = notify_with(&["old", "new"]);
        let terminal = terminal_for(NavPosition::Left);
        let placed = place_toasts(&n, terminal, window, NavPosition::Left);
        let new_id = n.toasts[1].id;
        assert_eq!(placed[0].0, new_id, "the newest is placed first");
        assert_eq!(placed[0].1.y, terminal.y);
        assert_eq!(
            placed[1].1.y,
            placed[0].1.bottom(),
            "the older one stacks below"
        );

        let terminal = terminal_for(NavPosition::Top);
        let placed = place_toasts(&n, terminal, window, NavPosition::Top);
        assert_eq!(placed[0].1.bottom(), terminal.bottom());
        assert_eq!(
            placed[1].1.bottom(),
            placed[0].1.y,
            "from the bottom it stacks up"
        );
    }

    #[test]
    fn a_long_reason_wraps_inside_the_width_cap() {
        let window = Rect::new(0, 0, 100, 30);
        let mut n = Notifications::default();
        n.toast("gpu-02", vec![Note::new(Level::Error, "x ".repeat(60))]);
        let placed = place_toasts(
            &n,
            terminal_for(NavPosition::Left),
            window,
            NavPosition::Left,
        );
        let r = placed[0].1;
        assert_eq!(r.width, 40);
        assert!(r.height > 3, "the reason wraps rather than clipping: {r:?}");
    }

    #[test]
    fn a_terminal_too_narrow_for_a_toast_gets_none() {
        let n = notify_with(&["gpu-02"]);
        let placed = place_toasts(
            &n,
            Rect::new(0, 0, 10, 10),
            Rect::new(0, 0, 100, 10),
            NavPosition::Right,
        );
        assert!(placed.is_empty());
    }

    #[test]
    fn a_timed_toast_underlines_its_remaining_life_on_its_message_line() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(t0, "re-scan", vec![Note::new(Level::Success, "no changes")]);
        let toast = n.toasts[0].clone();
        let rect = Rect::new(0, 0, 22, 3);
        let underlined = |now: Instant| {
            let mut term = Terminal::new(TestBackend::new(22, 3)).unwrap();
            term.draw(|f| render_toast(f, rect, &toast, Some(now), "C-g", &Palette::default()))
                .unwrap();
            let buf = term.backend().buffer().clone();
            (1..21)
                .filter(|&x| buf[(x, 1)].modifier.contains(Modifier::UNDERLINED))
                .count()
        };
        assert_eq!(
            underlined(t0),
            20,
            "a fresh toast underlines its whole line"
        );
        let half = underlined(t0 + crate::state::notify::TOAST_TTL / 2);
        assert_eq!(half, 10, "half its life underlines half its line");
        assert_eq!(underlined(t0 + crate::state::notify::TOAST_TTL), 0);
    }

    #[test]
    fn a_sticky_toast_draws_no_countdown() {
        let n = notify_with(&["gpu-02"]);
        let rect = Rect::new(0, 0, 30, 3);
        let mut term = Terminal::new(TestBackend::new(30, 3)).unwrap();
        term.draw(|f| render_toast(f, rect, &n.toasts[0], n.now, "C-g", &Palette::default()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        assert!(!(1..29).any(|x| buf[(x, 1)].modifier.contains(Modifier::UNDERLINED)));
        let row: String = (0..30).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row.contains("✗ login failed"), "{row:?}");
        let bottom: String = (0..30).map(|x| buf[(x, 2)].symbol().to_string()).collect();
        assert!(bottom.contains("C-g m history"), "{bottom:?}");
    }

    #[test]
    fn the_history_lists_the_newest_record_first_and_scrolls() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(t0, "gpu-02", vec![Note::new(Level::Success, "logged in")]);
        n.toast_at(
            t0,
            "re-scan",
            vec![Note::new(Level::Warning, "web-03 unreachable")],
        );
        n.tick(t0 + std::time::Duration::from_secs(90));
        let text = |scroll| {
            history_lines(&n, scroll, 60, &Palette::default())
                .1
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
        };
        let all = text(0);
        assert_eq!(all.len(), 2);
        assert!(
            all[0].contains("1m ▲ re-scan · web-03 unreachable"),
            "{all:?}"
        );
        assert!(all[1].contains("1m ✓ gpu-02 · logged in"), "{all:?}");
        let scrolled = text(1);
        assert_eq!(scrolled.len(), 1);
        assert!(scrolled[0].contains("gpu-02"));
        assert_eq!(text(99).len(), 1, "scrolling past the end keeps the oldest");
    }
}
