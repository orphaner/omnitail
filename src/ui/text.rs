//! Turns raw log lines into terminal rows.

use ansi_to_tui::IntoText;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Parses ANSI styling (or drops it with `strip`) and wraps to `width`
/// columns. Always returns at least one row, so empty lines keep their place.
pub fn rows(raw: &str, width: usize, strip: bool) -> Vec<Line<'static>> {
    let lines = match raw.as_bytes().into_text() {
        Ok(text) => text.lines,
        Err(_) => vec![Line::raw(raw.to_owned())],
    };
    let mut out = Vec::new();
    for line in lines {
        let line = if strip { unstyled(line) } else { line };
        out.extend(wrap(line, width));
    }
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

fn unstyled(line: Line<'static>) -> Line<'static> {
    Line::raw(line.spans.into_iter().map(|s| s.content).collect::<String>())
}

/// Character-level wrap (log lines are rarely prose, and exact row counts
/// matter for anchoring the view). Control characters left over after ANSI
/// parsing are dropped: they would corrupt the screen.
pub fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let line_style = line.style;
    let mut rows = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0;
    for span in line.spans {
        let mut chunk = String::new();
        for c in span.content.chars() {
            if c.is_control() {
                continue;
            }
            let w = c.width().unwrap_or(0);
            if row_width + w > width && row_width > 0 {
                if !chunk.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut row)).style(line_style));
                row_width = 0;
            }
            chunk.push(c);
            row_width += w;
        }
        if !chunk.is_empty() {
            row.push(Span::styled(chunk, span.style));
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(Line::from(row).style(line_style));
    }
    rows
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    fn plain(rows: &[Line]) -> Vec<String> {
        rows.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn wraps_by_width() {
        assert_eq!(plain(&rows("abcdefg", 3, false)), ["abc", "def", "g"]);
        assert_eq!(plain(&rows("abc", 3, false)), ["abc"]);
        assert_eq!(plain(&rows("", 3, false)), [""]);
    }

    #[test]
    fn wide_chars_do_not_split() {
        assert_eq!(plain(&rows("a日本", 4, false)), ["a日", "本"]);
    }

    #[test]
    fn keeps_or_strips_ansi() {
        let raw = "\x1b[31mred\x1b[0m ok";
        let styled = rows(raw, 80, false);
        assert_eq!(styled[0].to_string(), "red ok");
        assert_eq!(styled[0].spans[0].style, Style::default().fg(Color::Red));
        let stripped = rows(raw, 80, true);
        assert_eq!(stripped[0].to_string(), "red ok");
        assert!(stripped[0].spans.iter().all(|s| s.style == Style::default()));
    }

    #[test]
    fn style_survives_wrapping() {
        let r = rows("\x1b[32mabcdef\x1b[0m", 4, false);
        assert_eq!(plain(&r), ["abcd", "ef"]);
        assert_eq!(r[1].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn drops_control_chars() {
        assert_eq!(plain(&rows("a\x07b\x08c", 80, false)), ["abc"]);
    }
}
