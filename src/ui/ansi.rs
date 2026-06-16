//! Converts a pane's ANSI SGR sequences into a ratatui [`Text`] of styled
//! [`Span`]s. Each span carries its FULL resolved style, so an attribute
//! (underline, dim, …) can never bleed past its reset — every span is rendered
//! independently. Non-SGR CSI sequences and OSC strings are dropped (never leaked
//! as raw escape bytes). Unlike the tview port this does not interpret or escape
//! markup, because ratatui spans hold literal text.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

/// The accumulating SGR state: a current fg/bg and modifier set.
#[derive(Clone, Default)]
struct SgrState {
    fg: Option<Color>,
    bg: Option<Color>,
    mods: Modifier,
}

impl SgrState {
    fn style(&self) -> Style {
        let mut s = Style::default();
        if let Some(c) = self.fg {
            s = s.fg(c);
        }
        if let Some(c) = self.bg {
            s = s.bg(c);
        }
        s.add_modifier(self.mods)
    }
}

/// Converts an ANSI string into a styled [`Text`]. `\n` splits lines; a trailing
/// newline does not produce a spurious empty line.
pub fn ansi_to_text(s: &str) -> Text<'static> {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut state = SgrState::default();

    let mut i = 0;
    while i < len {
        let c = chars[i];
        match c {
            '\n' => {
                flush_span(&mut spans, &mut cur, &state);
                lines.push(Line::from(std::mem::take(&mut spans)));
                i += 1;
            }
            '\r' => {
                i += 1; // drop a bare CR
            }
            '\u{1b}' => {
                if i + 1 >= len {
                    break;
                }
                match chars[i + 1] {
                    '[' => {
                        // CSI: params until a final byte 0x40–0x7e.
                        let mut j = i + 2;
                        while j < len && !('\u{40}'..='\u{7e}').contains(&chars[j]) {
                            j += 1;
                        }
                        if j >= len {
                            break; // unterminated CSI — drop the rest
                        }
                        if chars[j] == 'm' {
                            // Flush text accumulated under the PRIOR style first.
                            flush_span(&mut spans, &mut cur, &state);
                            let params: String = chars[i + 2..j].iter().collect();
                            apply_sgr(&params, &mut state);
                        }
                        i = j + 1;
                    }
                    ']' => {
                        // OSC: until BEL or ST (ESC \).
                        let mut j = i + 2;
                        while j < len {
                            if chars[j] == '\u{07}' {
                                break;
                            }
                            if chars[j] == '\u{1b}' && j + 1 < len && chars[j + 1] == '\\' {
                                j += 1;
                                break;
                            }
                            j += 1;
                        }
                        i = j + 1;
                    }
                    _ => {
                        i += 2; // consume ESC + the next byte
                    }
                }
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }

    flush_span(&mut spans, &mut cur, &state);
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    Text::from(lines)
}

fn flush_span(spans: &mut Vec<Span<'static>>, cur: &mut String, state: &SgrState) {
    if !cur.is_empty() {
        spans.push(Span::styled(std::mem::take(cur), state.style()));
    }
}

fn basic_color(n: i32) -> Option<Color> {
    if (0..=15).contains(&n) {
        Some(Color::Indexed(n as u8))
    } else {
        None
    }
}

fn apply_sgr(params: &str, state: &mut SgrState) {
    let params = if params.is_empty() { "0" } else { params };
    let fields: Vec<&str> = params.split(';').collect();
    let mut i = 0;
    while i < fields.len() {
        match fields[i] {
            "0" | "" => {
                state.fg = None;
                state.bg = None;
                state.mods = Modifier::empty();
            }
            "1" | "01" => state.mods.insert(Modifier::BOLD),
            "2" | "02" => state.mods.insert(Modifier::DIM),
            "3" | "03" => state.mods.insert(Modifier::ITALIC),
            "4" | "04" => state.mods.insert(Modifier::UNDERLINED),
            "5" | "05" => state.mods.insert(Modifier::SLOW_BLINK),
            "7" | "07" => state.mods.insert(Modifier::REVERSED),
            "9" | "09" => state.mods.insert(Modifier::CROSSED_OUT),
            "22" => state.mods.remove(Modifier::BOLD | Modifier::DIM),
            "23" => state.mods.remove(Modifier::ITALIC),
            "24" => state.mods.remove(Modifier::UNDERLINED),
            "25" => state.mods.remove(Modifier::SLOW_BLINK),
            "27" => state.mods.remove(Modifier::REVERSED),
            "29" => state.mods.remove(Modifier::CROSSED_OUT),
            "39" => state.fg = None,
            "49" => state.bg = None,
            "38" | "48" => {
                let (color, consumed) = extended_color(&fields, i);
                if let Some(c) = color {
                    if fields[i] == "38" {
                        state.fg = Some(c);
                    } else {
                        state.bg = Some(c);
                    }
                }
                i += consumed;
            }
            "58" => {
                // underline colour — ignore, but consume its sub-params.
                let (_c, consumed) = extended_color(&fields, i);
                i += consumed;
            }
            other => {
                if let Ok(n) = other.parse::<i32>() {
                    match n {
                        30..=37 => state.fg = basic_color(n - 30),
                        40..=47 => state.bg = basic_color(n - 40),
                        90..=97 => state.fg = basic_color(n - 82),
                        100..=107 => state.bg = basic_color(n - 92),
                        _ => {}
                    }
                }
                // anything else (59, colon-forms like "4:3", …) is ignored.
            }
        }
        i += 1;
    }
}

/// Parses a `38`/`48`/`58` colour starting at `fields[i]` and returns the colour
/// plus how many EXTRA fields it consumed.
fn extended_color(fields: &[&str], i: usize) -> (Option<Color>, usize) {
    if i + 1 >= fields.len() {
        return (None, 0);
    }
    match fields[i + 1] {
        "5" => {
            // 8-bit: any palette index 0–255 maps to Indexed (the terminal's own
            // theme decides the actual colour).
            if i + 2 >= fields.len() {
                return (None, 1);
            }
            match fields[i + 2].parse::<u16>() {
                Ok(n) if n <= 255 => (Some(Color::Indexed(n as u8)), 2),
                _ => (None, 2),
            }
        }
        "2" => {
            // 24-bit truecolor.
            if i + 4 >= fields.len() {
                return (None, fields.len() - i - 1);
            }
            let r = fields[i + 2].parse::<u8>().unwrap_or(0);
            let g = fields[i + 3].parse::<u8>().unwrap_or(0);
            let b = fields[i + 4].parse::<u8>().unwrap_or(0);
            (Some(Color::Rgb(r, g, b)), 4)
        }
        _ => (None, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flattens a Text into (text, style) span tuples.
    fn spans(t: &Text<'static>) -> Vec<(String, Style)> {
        let mut out = Vec::new();
        for line in &t.lines {
            for span in &line.spans {
                out.push((span.content.to_string(), span.style));
            }
        }
        out
    }

    fn find(spans: &[(String, Style)], text: &str) -> Style {
        spans
            .iter()
            .find(|(c, _)| c == text)
            .unwrap_or_else(|| panic!("no span with text {text:?} in {spans:?}"))
            .1
    }

    fn flat(t: &Text<'static>) -> String {
        spans(t).into_iter().map(|(c, _)| c).collect()
    }

    #[test]
    fn no_underline_bleed() {
        let t = ansi_to_text("\x1b[4mUNDER\x1b[24mNORMAL");
        let sp = spans(&t);
        assert!(find(&sp, "UNDER")
            .add_modifier
            .contains(Modifier::UNDERLINED));
        assert!(!find(&sp, "NORMAL")
            .add_modifier
            .contains(Modifier::UNDERLINED));
    }

    #[test]
    fn reset_clears_attributes() {
        let t = ansi_to_text("\x1b[4mU\x1b[0m end");
        let sp = spans(&t);
        assert_eq!(find(&sp, " end"), Style::default());
    }

    #[test]
    fn truecolor_then_underline() {
        let t = ansi_to_text("\x1b[38;2;255;0;0;4mX");
        let style = find(&spans(&t), "X");
        assert_eq!(style.fg, Some(Color::Rgb(255, 0, 0)));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn underline_color_ignored_not_misparsed() {
        // 58;2;r;g;b (underline colour) must be consumed, not parsed as 2=dim.
        let t = ansi_to_text("\x1b[4;58;2;0;255;0mX");
        let style = find(&spans(&t), "X");
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn basic_and_256_colors() {
        let t = ansi_to_text("\x1b[31mR");
        let sp = spans(&t);
        assert_eq!(find(&sp, "R").fg, Some(Color::Indexed(1)));
        let t = ansi_to_text("\x1b[38;5;9mR");
        let sp = spans(&t);
        assert_eq!(find(&sp, "R").fg, Some(Color::Indexed(9)));
    }

    #[test]
    fn bright_foreground_maps_to_high_palette() {
        // 91 = bright red = palette index 9.
        let t = ansi_to_text("\x1b[91mR");
        let sp = spans(&t);
        assert_eq!(find(&sp, "R").fg, Some(Color::Indexed(9)));
    }

    #[test]
    fn drops_unknown_sequences() {
        // An OSC hyperlink and a non-SGR CSI must be dropped, not leaked.
        let t = ansi_to_text("a\x1b]8;;http://x\x07b\x1b[2Kc");
        let s = flat(&t);
        assert!(!s.contains('\u{1b}'), "no raw escape may leak: {s:?}");
        for w in ["a", "b", "c"] {
            assert!(s.contains(w), "text {w:?} lost: {s:?}");
        }
    }

    #[test]
    fn newlines_split_lines_no_trailing_empty() {
        let t = ansi_to_text("one\ntwo");
        assert_eq!(t.lines.len(), 2);
        let t2 = ansi_to_text("one\n");
        assert_eq!(t2.lines.len(), 1);
    }
}
