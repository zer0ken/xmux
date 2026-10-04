//! The table of the hosts to check: every host in a problem state, grouped by cause under
//! the cause's state glyph, each with the reason its last answer gave and whether the
//! hiding leaves it without a card. The lines are built here from the entries the
//! switcher derives, so the paint and the tests read one answer.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

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

/// The table's title and lines at `width` inner cells. The selected entry is painted in
/// the selection style; its reason and every other reason wrap under it rather than being
/// cut. The last line names the table's keys, read from the key table.
pub(crate) fn check_lines(
    entries: &[CheckEntry],
    selected: usize,
    width: u16,
    visible_rows: usize,
    keys: &str,
    palette: &Palette,
) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(palette.decoration);
    let hidden = entries.iter().filter(|e| e.hidden).count();
    let title = if hidden == 0 {
        "hosts to check".to_string()
    } else {
        format!("hosts to check · {hidden} hidden")
    };
    if entries.is_empty() {
        return (
            title,
            vec![Line::from(Span::styled(
                " nothing to check: every host answered",
                dim,
            ))],
        );
    }
    let indent = 5usize;
    let words = (width as usize).saturating_sub(indent + 1).max(1) as u16;
    let mut lines = Vec::new();
    let mut last: Option<FailureKind> = None;
    let mut selected_line = 0;
    for (i, entry) in entries.iter().enumerate() {
        if last != Some(entry.kind) {
            let (glyph, style, word) = cause(entry.kind, palette);
            let count = entries.iter().filter(|e| e.kind == entry.kind).count();
            lines.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), style),
                Span::styled(format!("{word} · {count}"), dim),
            ]));
            last = Some(entry.kind);
        }
        let mut spans = vec![Span::raw(format!("   {}", entry.label))];
        if entry.hidden {
            spans.push(Span::styled("  hidden", dim));
        }
        let mut line = Line::from(spans);
        if i == selected {
            selected_line = lines.len();
            line = line.style(palette::selection_style(palette));
        }
        lines.push(line);
        for chunk in crate::ui::modal::wrap_text(&entry.reason, words) {
            lines.push(Line::from(Span::styled(
                format!("{:indent$}{chunk}", ""),
                dim,
            )));
        }
    }
    lines.push(Line::from(""));
    for chunk in crate::ui::modal::wrap_text(keys, (width as usize).saturating_sub(2).max(1) as u16)
    {
        lines.push(Line::from(Span::styled(format!(" {chunk}"), dim)));
    }
    if lines.len() > visible_rows && visible_rows > 0 {
        let start = selected_line
            .saturating_sub(visible_rows.saturating_sub(2))
            .min(lines.len() - visible_rows);
        lines = lines.into_iter().skip(start).take(visible_rows).collect();
    }
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: &str, kind: FailureKind, hidden: bool) -> CheckEntry {
        CheckEntry {
            source: source.into(),
            label: source.into(),
            kind,
            reason: format!("{source} said no"),
            hidden,
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn entries_read_under_their_cause_with_the_reason_and_the_hidden_mark() {
        let entries = vec![
            entry("gpu-02", FailureKind::Blocked, false),
            entry("web-03", FailureKind::Unreachable, true),
            entry("web-04", FailureKind::Unreachable, false),
            entry("db-01", FailureKind::ListFailed, false),
        ];
        let p = Palette::default();
        let (title, lines) = check_lines(&entries, 1, 60, usize::MAX, "keys", &p);
        assert_eq!(title, "hosts to check · 1 hidden");
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                " ? login needed · 1",
                "   gpu-02",
                "     gpu-02 said no",
                " ▲ unreachable · 2",
                "   web-03  hidden",
                "     web-03 said no",
                "   web-04",
                "     web-04 said no",
                " ✗ list failed · 1",
                "   db-01",
                "     db-01 said no",
                "",
                " keys",
            ]
        );
        assert_eq!(
            lines[4].style,
            palette::selection_style(&p),
            "the selected row"
        );
        assert_ne!(lines[1].style, palette::selection_style(&p));
    }

    #[test]
    fn an_empty_table_says_every_host_answered() {
        let (title, lines) = check_lines(&[], 0, 40, usize::MAX, "keys", &Palette::default());
        assert_eq!(title, "hosts to check");
        assert_eq!(lines.len(), 1);
        assert!(text(&lines[0]).contains("every host answered"));
    }

    #[test]
    fn selected_host_remains_visible_when_the_table_exceeds_the_popup() {
        let entries: Vec<_> = (0..20)
            .map(|i| entry(&format!("host-{i:02}"), FailureKind::Unreachable, true))
            .collect();
        let (_, lines) = check_lines(&entries, 19, 50, 8, "keys", &Palette::default());
        assert_eq!(lines.len(), 8);
        assert!(lines.iter().any(|line| text(line).contains("host-19")));
        assert!(lines
            .iter()
            .any(|line| text(line).contains("host-19 said no")));
        assert!(lines
            .iter()
            .any(|line| line.style == palette::selection_style(&Palette::default())));
    }
}
