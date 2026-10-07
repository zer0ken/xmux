//! Machine problems, grouped by cause with the reason each host last reported.
//! Lines are built from the entries the switcher derives.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::model::FailureKind;
use crate::ui::palette::{self, Palette};
use crate::ui::switcher::CheckEntry;

/// The state glyph, its colour, and the state word of one cause, as a host card states it.
fn cause(kind: FailureKind, palette: &Palette) -> (&'static str, Style, &'static str) {
    match kind {
        FailureKind::Blocked => (
            crate::ui::chrome::BLOCK_MARK,
            Style::default().fg(palette.warning),
            crate::ui::tree::host_state_word(false, true, false, false),
        ),
        FailureKind::Unreachable => (
            crate::ui::chrome::UNREACHABLE_MARK,
            Style::default().fg(palette.error),
            crate::ui::tree::host_state_word(false, false, false, true),
        ),
        FailureKind::ListFailed => (
            crate::ui::chrome::LIST_FAILED_MARK,
            Style::default().fg(palette.primary),
            crate::ui::tree::host_state_word(false, false, true, false),
        ),
    }
}

/// The table's top-border meta and lines at `width` inner cells. Each cause is a group
/// title with its glyph in the state's colour, and each host under it a row in the
/// key-column grammar: the host bold, then its reason muted, wrapped under the reason
/// column rather than cut. A host wider than its column takes rows of its own above its
/// reason. The selected host's rows are highlighted across the whole width, and the rows of
/// the `hover` host, the soft selection, take the soft selection's background. Each line comes with the host it
/// belongs to (none for a cause title), so a click is hit-tested against the rows the
/// paint shows.
pub(crate) fn check_lines(
    entries: &[CheckEntry],
    selected: usize,
    hover: Option<usize>,
    width: u16,
    visible_rows: usize,
    palette: &Palette,
) -> (String, crate::ui::modal::ItemLines) {
    let dim = Style::default().fg(palette.decoration);
    if entries.is_empty() {
        let room = (width as usize).saturating_sub(2).max(1) as u16;
        return (
            String::new(),
            crate::ui::modal::wrap_text("nothing to check: every machine answered", room)
                .into_iter()
                .map(|c| (None, Line::from(Span::styled(format!(" {c}"), dim))))
                .collect(),
        );
    }
    let lw = entries
        .iter()
        .map(|e| UnicodeWidthStr::width(e.label.as_str()))
        .max()
        .unwrap_or(0)
        .min((width as usize).saturating_sub(3 + 2 + 1) / 2)
        .max(1);
    let lead = 3 + lw + 2;
    let words = (width as usize).saturating_sub(lead + 1).max(1) as u16;
    let bold = palette::interaction_key_style();
    let mut lines = Vec::new();
    let mut last: Option<FailureKind> = None;
    let mut selected_line = 0;
    for (i, entry) in entries.iter().enumerate() {
        if last != Some(entry.kind) {
            let (glyph, style, word) = cause(entry.kind, palette);
            lines.push((
                None,
                Line::from(vec![
                    Span::styled(format!(" {glyph} "), style),
                    Span::styled(word.to_string(), dim.add_modifier(Modifier::BOLD)),
                ]),
            ));
            last = Some(entry.kind);
        }
        let chosen = i == selected;
        let mark = "   ".to_string();
        let label_w = UnicodeWidthStr::width(entry.label.as_str());
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let mut reason = crate::ui::modal::wrap_text(&entry.reason, words).into_iter();
        if label_w > lw {
            let room = (width as usize).saturating_sub(3 + 1).max(1) as u16;
            for (n, chunk) in crate::ui::modal::wrap_text(&entry.label, room)
                .into_iter()
                .enumerate()
            {
                let lead = if n == 0 { mark.clone() } else { "   ".into() };
                rows.push(vec![Span::raw(lead), Span::styled(chunk, bold)]);
            }
        } else {
            rows.push(vec![
                Span::raw(mark),
                Span::styled(entry.label.clone(), bold),
                Span::raw(" ".repeat(lw - label_w + 2)),
                Span::styled(reason.next().unwrap_or_default(), dim),
            ]);
        }
        rows.extend(reason.map(|c| vec![Span::raw(" ".repeat(lead)), Span::styled(c, dim)]));
        let hovered = hover == Some(i);
        for (n, mut spans) in rows.into_iter().enumerate() {
            let mut line = if chosen {
                if n == 0 {
                    selected_line = lines.len();
                }
                let used: usize = spans.iter().map(|s| s.width()).sum();
                spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
                palette::selected_line(Line::from(spans), palette)
            } else {
                Line::from(spans)
            };
            if hovered {
                line = palette::soft_selected_line(line, chosen, palette);
            }
            lines.push((Some(i), line));
        }
    }
    if lines.len() > visible_rows && visible_rows > 0 {
        let start = selected_line
            .saturating_sub(visible_rows.saturating_sub(2))
            .min(lines.len() - visible_rows);
        lines = lines.into_iter().skip(start).take(visible_rows).collect();
    }
    (entries.len().to_string(), lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, kind: FailureKind) -> CheckEntry {
        CheckEntry {
            host: host.into(),
            label: host.into(),
            kind,
            reason: format!("{host} said no"),
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn entries_read_under_their_cause_with_the_reason() {
        let entries = vec![
            entry("gpu-02", FailureKind::Blocked),
            entry("web-03", FailureKind::Unreachable),
            entry("web-04", FailureKind::Unreachable),
            entry("db-01", FailureKind::ListFailed),
        ];
        let p = Palette::default();
        let (meta, lines) = check_lines(&entries, 1, None, 40, usize::MAX, &p);
        let lines: Vec<Line> = lines.into_iter().map(|(_, l)| l).collect();
        assert_eq!(meta, "4");
        let texts: Vec<String> = lines
            .iter()
            .map(|l| text(l).trim_end().to_string())
            .collect();
        assert_eq!(
            texts,
            [
                " ? login needed",
                "   gpu-02  gpu-02 said no",
                " ▲ unreachable",
                "   web-03  web-03 said no",
                "   web-04  web-04 said no",
                " ✗ list failed",
                "   db-01   db-01 said no",
            ]
        );
        assert_eq!(
            lines[3].style,
            palette::selection_style(&p),
            "the selected row"
        );
        assert_ne!(lines[1].style, palette::selection_style(&p));
    }

    #[test]
    fn an_empty_table_says_every_host_answered() {
        let (meta, lines) = check_lines(&[], 0, None, 48, usize::MAX, &Palette::default());
        let lines: Vec<Line> = lines.into_iter().map(|(_, l)| l).collect();
        assert_eq!(meta, "");
        assert_eq!(lines.len(), 1);
        assert!(text(&lines[0]).contains("every machine answered"));
    }

    #[test]
    fn selected_host_remains_visible_when_the_table_exceeds_the_popup() {
        let entries: Vec<_> = (0..20)
            .map(|i| entry(&format!("host-{i:02}"), FailureKind::Unreachable))
            .collect();
        let (_, lines) = check_lines(&entries, 19, None, 50, 8, &Palette::default());
        let lines: Vec<Line> = lines.into_iter().map(|(_, l)| l).collect();
        assert_eq!(lines.len(), 8);
        assert!(lines.iter().any(|line| text(line).contains("host-19")));
        assert!(lines
            .iter()
            .any(|line| text(line).contains("host-19 said no")));
        assert!(lines
            .iter()
            .any(|line| line.style == palette::selection_style(&Palette::default())));
    }

    #[test]
    fn a_long_host_name_leaves_every_reason_its_column() {
        let mut long = entry("gpu-02", FailureKind::Blocked);
        long.label = "worker-01.production.example.com".into();
        let entries = vec![long, entry("db-01", FailureKind::Blocked)];
        let (_, lines) = check_lines(&entries, 0, None, 29, usize::MAX, &Palette::default());
        let lines: Vec<Line> = lines.into_iter().map(|(_, l)| l).collect();
        assert!(lines.iter().all(|l| l.width() <= 29));
        let all: Vec<String> = lines.iter().map(text).collect();
        for reason in ["gpu-02", "db-01"] {
            assert!(all.iter().any(|l| l.contains(reason)), "{all:?}");
        }
        assert!(
            all.iter().all(|l| !l.contains('…'))
                && all[1..3]
                    .iter()
                    .map(|l| l.trim())
                    .collect::<String>()
                    .contains("worker-01.production.example.com"),
            "the host is whole, wrapped on rows of its own: {all:?}"
        );
        assert!(
            all[3].starts_with(&" ".repeat(5)) && all[3].trim_start().starts_with("gpu-02"),
            "its reason under the reason column: {all:?}"
        );
    }

    #[test]
    fn the_hovered_host_is_underlined_and_every_row_names_its_host() {
        let entries = vec![
            entry("gpu-02", FailureKind::Blocked),
            entry("web-03", FailureKind::Unreachable),
        ];
        let p = Palette::default();
        let (_, lines) = check_lines(&entries, 0, Some(1), 40, usize::MAX, &p);
        let items: Vec<Option<usize>> = lines.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            items,
            [None, Some(0), None, Some(1)],
            "a cause title names no host"
        );
        assert_eq!(lines[1].1.style, palette::selection_style(&p));
        assert_eq!(lines[3].1.style, palette::soft_selection_style(&p));
    }
}
