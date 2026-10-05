//! Hosts to check, grouped by cause with the reason each host last reported.
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
/// column rather than cut. The selected row is reversed across the whole width with `❯`.
pub(crate) fn check_lines(
    entries: &[CheckEntry],
    selected: usize,
    width: u16,
    visible_rows: usize,
    palette: &Palette,
) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(palette.decoration);
    if entries.is_empty() {
        return (
            String::new(),
            vec![Line::from(Span::styled(
                " nothing to check: every host answered",
                dim,
            ))],
        );
    }
    let lw = entries
        .iter()
        .map(|e| UnicodeWidthStr::width(e.label.as_str()))
        .max()
        .unwrap_or(0);
    let lead = 3 + lw + 2;
    let words = (width as usize).saturating_sub(lead + 1).max(1) as u16;
    let bold = palette::interaction_key_style();
    let mut lines = Vec::new();
    let mut last: Option<FailureKind> = None;
    let mut selected_line = 0;
    for (i, entry) in entries.iter().enumerate() {
        if last != Some(entry.kind) {
            let (glyph, style, word) = cause(entry.kind, palette);
            lines.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), style),
                Span::styled(word.to_string(), dim.add_modifier(Modifier::BOLD)),
            ]));
            last = Some(entry.kind);
        }
        let chosen = i == selected;
        let pad = lw.saturating_sub(UnicodeWidthStr::width(entry.label.as_str()));
        let reason = crate::ui::modal::wrap_text(&entry.reason, words);
        // The selected row is reversed as one surface, so its spans keep no colour that
        // the reversal would turn into a second background.
        let dim = if chosen { Style::default() } else { dim };
        for (n, chunk) in reason.into_iter().enumerate() {
            let mut spans = if n == 0 {
                vec![
                    Span::raw(if chosen {
                        format!(" {} ", crate::ui::switcher::SELECTED_MARK)
                    } else {
                        "   ".to_string()
                    }),
                    Span::styled(entry.label.clone(), bold),
                    Span::raw(" ".repeat(pad + 2)),
                    Span::styled(chunk, dim),
                ]
            } else {
                vec![Span::raw(" ".repeat(lead)), Span::styled(chunk, dim)]
            };
            if n == 0 && chosen {
                selected_line = lines.len();
                let used: usize = spans.iter().map(|s| s.width()).sum();
                spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
                lines.push(Line::from(spans).style(palette::selection_style(palette)));
            } else {
                lines.push(Line::from(spans));
            }
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

    fn entry(source: &str, kind: FailureKind) -> CheckEntry {
        CheckEntry {
            source: source.into(),
            label: source.into(),
            kind,
            reason: format!("{source} said no"),
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
        let (meta, lines) = check_lines(&entries, 1, 40, usize::MAX, &p);
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
                " ❯ web-03  web-03 said no",
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
        let (meta, lines) = check_lines(&[], 0, 40, usize::MAX, &Palette::default());
        assert_eq!(meta, "");
        assert_eq!(lines.len(), 1);
        assert!(text(&lines[0]).contains("every host answered"));
    }

    #[test]
    fn selected_host_remains_visible_when_the_table_exceeds_the_popup() {
        let entries: Vec<_> = (0..20)
            .map(|i| entry(&format!("host-{i:02}"), FailureKind::Unreachable))
            .collect();
        let (_, lines) = check_lines(&entries, 19, 50, 8, &Palette::default());
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
