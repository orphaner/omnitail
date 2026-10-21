//! Turns raw log lines into terminal rows.

use ansi_to_tui::IntoText;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::color::Scheme;
use crate::pattern::Pattern;

/// How lines are turned into rows.
#[derive(Clone, Copy)]
pub struct Render<'a> {
    pub width: usize,
    /// Drop the source's ANSI colors.
    pub strip: bool,
    /// omnitail's coloring, for lines the source did not color.
    pub scheme: Option<&'a Scheme>,
    /// Wrap long lines; otherwise cut them, marking the cut with `›`.
    pub wrap: bool,
    /// Columns skipped on the left when not wrapping.
    pub hscroll: usize,
    /// Matches stand out, over any other styling.
    pub highlight: Option<&'a Pattern>,
    /// What the filter matched, marked less loudly (under the search).
    pub filter: Option<&'a Pattern>,
}

impl Render<'_> {
    /// Styles a raw line and lays it out in rows. Lines colored by their
    /// source keep those colors (unless `strip`); the others get omnitail's
    /// scheme, if any. Always at least one row, so empty lines keep their place.
    pub fn rows(&self, raw: &str) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for line in self.styled(raw) {
            let line = match self.filter {
                Some(pattern) => highlight(line, pattern, super::FILTER_MATCH),
                None => line,
            };
            let line = match self.highlight {
                Some(pattern) => highlight(line, pattern, super::HIGHLIGHT),
                None => line,
            };
            if self.wrap {
                out.extend(wrap(line, self.width));
            } else {
                out.push(cut(line, self.width, self.hscroll));
            }
        }
        if out.is_empty() {
            out.push(Line::default());
        }
        out
    }

    fn styled(&self, raw: &str) -> Vec<Line<'static>> {
        if !raw.contains('\x1b') {
            return vec![match self.scheme {
                Some(scheme) => scheme.line(raw),
                None => Line::raw(raw.to_owned()),
            }];
        }
        let lines = match raw.as_bytes().into_text() {
            Ok(text) => text.lines,
            Err(_) => vec![Line::raw(raw.to_owned())],
        };
        match (self.strip, self.scheme) {
            (false, _) => lines,
            (true, None) => lines.into_iter().map(unstyled).collect(),
            (true, Some(scheme)) => lines.into_iter().map(|l| scheme.line(&unstyled(l).to_string())).collect(),
        }
    }
}

/// Patches the style of the text `pattern` matches.
fn highlight(line: Line<'static>, pattern: &Pattern, style: Style) -> Line<'static> {
    let text = line.to_string();
    let ranges = pattern.find_ranges(&text);
    if ranges.is_empty() {
        return line;
    }
    let mut cells = Vec::with_capacity(text.len());
    let mut offset = 0;
    for span in &line.spans {
        for c in span.content.chars() {
            let hit = ranges.iter().any(|r| r.contains(&offset));
            cells.push((c, if hit { span.style.patch(style) } else { span.style }));
            offset += c.len_utf8();
        }
    }
    line_of(&cells, line.style)
}

fn unstyled(line: Line<'static>) -> Line<'static> {
    Line::raw(line.spans.into_iter().map(|s| s.content).collect::<String>())
}

/// The visible characters of a line with their style. Control characters
/// left over after ANSI parsing are dropped: they would corrupt the screen.
fn cells(line: &Line) -> Vec<(char, Style)> {
    line.spans
        .iter()
        .flat_map(|span| span.content.chars().filter(|c| !c.is_control()).map(move |c| (c, span.style)))
        .collect()
}

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Rebuilds styled spans from cells, merging neighbours with the same style.
fn line_of(cells: &[(char, Style)], line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &(c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    Line::from(spans).style(line_style)
}

/// Word wrap: breaks after a space when possible, inside a word only when
/// it is wider than the screen. A space at a break is dropped.
pub fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let cells = cells(&line);
    let mut rows = Vec::new();
    let (mut start, mut row_width, mut i) = (0, 0, 0);
    // Where the next row would start if we broke after the last space.
    let mut after_space: Option<usize> = None;
    while i < cells.len() {
        let (c, _) = cells[i];
        let w = width_of(c);
        if row_width + w > width && i > start {
            if c == ' ' {
                rows.push(start..i);
                start = i + 1;
                i += 1;
            } else if let Some(next) = after_space.filter(|&b| b > start) {
                rows.push(start..next - 1); // without the space
                start = next;
            } else {
                rows.push(start..i);
                start = i;
            }
            row_width = cells[start..i].iter().map(|&(c, _)| width_of(c)).sum();
            after_space = None;
            continue;
        }
        row_width += w;
        if c == ' ' {
            after_space = Some(i + 1);
        }
        i += 1;
    }
    if start < cells.len() || rows.is_empty() {
        rows.push(start..cells.len());
    }
    rows.into_iter().map(|r| line_of(&cells[r], line.style)).collect()
}

/// One row: the line from column `offset`, cut to `width`. `‹` and `›`
/// replace the first and last columns when text is hidden on that side.
pub fn cut(line: Line<'static>, width: usize, offset: usize) -> Line<'static> {
    let marker = super::dim();
    let mut cells = cells(&line);
    let mut skipped = 0;
    let first = cells.iter().position(|&(c, _)| {
        skipped += width_of(c);
        skipped > offset
    });
    cells.drain(..first.unwrap_or(cells.len()));
    if offset > 0 && width > 0 {
        // Left marker, over the first column.
        while cells.first().is_some_and(|&(c, _)| width_of(c) == 0) {
            cells.remove(0);
        }
        if !cells.is_empty() {
            cells[0] = ('‹', marker);
        }
    }
    let mut used = 0;
    let fits = cells
        .iter()
        .position(|&(c, _)| {
            used += width_of(c);
            used > width
        })
        .unwrap_or(cells.len());
    if fits < cells.len() {
        cells.truncate(fits);
        // Make room for the right marker.
        while cells.iter().map(|&(c, _)| width_of(c)).sum::<usize>() + 1 > width && !cells.is_empty() {
            cells.pop();
        }
        cells.push(('›', marker));
    }
    line_of(&cells, line.style)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    fn plain(rows: &[Line]) -> Vec<String> {
        rows.iter().map(|l| l.to_string()).collect()
    }

    fn render(width: usize) -> Render<'static> {
        Render { width, strip: false, scheme: None, wrap: true, hscroll: 0, highlight: None, filter: None }
    }

    #[test]
    fn word_wrap() {
        assert_eq!(plain(&render(10).rows("the quick brown fox")), ["the quick", "brown fox"]);
        assert_eq!(plain(&render(5).rows("abcdefg hi")), ["abcde", "fg hi"], "long words break inside");
        assert_eq!(plain(&render(3).rows("abc")), ["abc"]);
        assert_eq!(plain(&render(3).rows("")), [""]);
        assert_eq!(plain(&render(6).rows("ab cd ef gh")), ["ab cd", "ef gh"]);
    }

    #[test]
    fn wide_chars_do_not_split() {
        assert_eq!(plain(&render(4).rows("a日本")), ["a日", "本"]);
    }

    #[test]
    fn cut_without_wrap() {
        let r = Render { wrap: false, ..render(6) };
        assert_eq!(plain(&r.rows("abcdefghij")), ["abcde›"]);
        assert_eq!(plain(&r.rows("abc")), ["abc"]);
        let scrolled = Render { hscroll: 3, ..r };
        assert_eq!(plain(&scrolled.rows("abcdefghij")), ["‹efgh›"]);
        assert_eq!(plain(&scrolled.rows("abcdefg")), ["‹efg"]);
        assert_eq!(plain(&scrolled.rows("ab")), [""]);
    }

    #[test]
    fn keeps_or_strips_ansi() {
        let raw = "\x1b[31mred\x1b[0m ok";
        let styled = render(80).rows(raw);
        assert_eq!(styled[0].to_string(), "red ok");
        assert_eq!(styled[0].spans[0].style, Style::default().fg(Color::Red));
        let stripped = Render { strip: true, ..render(80) }.rows(raw);
        assert_eq!(stripped[0].to_string(), "red ok");
        assert!(stripped[0].spans.iter().all(|s| s.style == Style::default()));
    }

    #[test]
    fn style_survives_wrapping() {
        let r = render(4).rows("\x1b[32mabcdef\x1b[0m");
        assert_eq!(plain(&r), ["abcd", "ef"]);
        assert_eq!(r[1].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn scheme_applies_to_lines_without_source_colors() {
        let lib = crate::color::Library::new(&Default::default()).unwrap();
        let generic = lib.resolve(&["generic".into()]).unwrap();
        let colored = Render { scheme: Some(&generic), ..render(80) };
        let error_fg = |rows: &[Line]| rows[0].spans.iter().find(|s| s.content == "ERROR").and_then(|s| s.style.fg);

        assert_eq!(error_fg(&colored.rows("ERROR boom")), Some(Color::Indexed(203)));

        let source_colored = "\x1b[34mERROR\x1b[0m boom";
        assert_eq!(error_fg(&colored.rows(source_colored)), Some(Color::Blue), "source colors win");
        let stripped = Render { strip: true, ..colored };
        assert_eq!(error_fg(&stripped.rows(source_colored)), Some(Color::Indexed(203)), "stripped, then omnitail colors");
    }

    #[test]
    fn highlights_matches_over_colors() {
        let pattern = Pattern::parse("err", false).unwrap();
        let r = Render { highlight: Some(&pattern), ..render(80) };
        let rows = r.rows("\x1b[34mERROR\x1b[0m and err");
        let hits: Vec<_> = rows[0].spans.iter().filter(|s| s.style.bg == super::super::HIGHLIGHT.bg).map(|s| s.content.to_string()).collect();
        assert_eq!(hits, ["ERR", "err"]);
    }

    #[test]
    fn filter_matches_are_marked_under_the_search() {
        let filter = Pattern::parse("error", false).unwrap();
        let search = Pattern::parse("or", false).unwrap();
        let r = Render { filter: Some(&filter), highlight: Some(&search), ..render(80) };
        let styles: Vec<_> = r.rows("an error")[0].spans.iter().map(|s| (s.content.to_string(), s.style.bg)).collect();
        let (filtered, searched) = (super::super::FILTER_MATCH.bg, super::super::HIGHLIGHT.bg);
        assert_eq!(styles, [("an ".into(), None), ("err".into(), filtered), ("or".into(), searched)]);
        let negated = Pattern::parse("!error", true).unwrap();
        let r = Render { filter: Some(&negated), ..render(80) };
        assert_eq!(r.rows("an error")[0].spans.len(), 1, "nothing to mark for !pattern");
    }

    #[test]
    fn drops_control_chars() {
        assert_eq!(plain(&render(80).rows("a\x07b\x08c")), ["abc"]);
    }
}
