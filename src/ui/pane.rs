use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::text::Render;
use crate::app::Pane;
use crate::buffer::{Buffer, Seq};
use crate::pattern::Pattern;

fn split(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area)
}

/// Where a pane's lines go, below its title.
pub fn body(area: Rect) -> Rect {
    split(area)[1]
}

pub struct PaneOptions<'a> {
    pub strip: bool,
    pub wrap: bool,
    pub highlight: Option<&'a Pattern>,
}

pub fn render(frame: &mut Frame, area: Rect, index: usize, pane: &Pane, opts: &PaneOptions) {
    let [title, body] = split(area);
    let hidden = pane.buffer.view_start() - pane.buffer.first_seq();
    let mut count = super::lines_count(pane.buffer.len());
    if hidden > 0 {
        count.push_str(&format!(" ({hidden} hidden)"));
    }
    let filter = pane.filter.as_ref().map(|f| format!("filter: {} · ", f.text)).unwrap_or_default();
    let right = [
        format!("{filter}{}{count}", super::color_label(pane)),
        format!("{filter}{count}"),
        count,
    ];
    super::render_title(frame, title, index, pane, &right);
    let render = Render {
        width: body.width as usize,
        strip: opts.strip,
        scheme: pane.color.active(),
        wrap: opts.wrap,
        hscroll: 0,
        highlight: opts.highlight,
    };
    let start = pane.buffer.view_start();
    let end = pane.view_end().max(start);
    let rows = tail_rows(&pane.buffer, start, end, body.height as usize, &render, pane.filter.as_ref());
    frame.render_widget(Paragraph::new(rows), body);
}

/// The last `height` rows of lines `start..end` that pass `filter`,
/// bottom-anchored like a terminal: the newest line is on the last row.
fn tail_rows(
    buffer: &Buffer,
    start: Seq,
    end: Seq,
    height: usize,
    render: &Render,
    filter: Option<&Pattern>,
) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    let mut count = 0;
    let mut seq = end;
    while seq > start && count < height {
        seq -= 1;
        let line = buffer.get(seq).unwrap_or_default();
        if filter.is_some_and(|f| !f.accepts(line)) {
            continue;
        }
        let rows = render.rows(line);
        count += rows.len();
        chunks.push(rows);
    }
    let mut rows: Vec<_> = chunks.into_iter().rev().flatten().collect();
    rows.drain(..rows.len().saturating_sub(height));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(rows: Vec<Line>) -> Vec<String> {
        rows.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn keeps_newest_rows_with_partial_top_line() {
        let mut b = Buffer::new(100);
        for l in ["one", "abcdef", "z"] {
            b.push(l.into());
        }
        let render = Render { width: 3, strip: false, scheme: None, wrap: true, hscroll: 0, highlight: None };
        assert_eq!(plain(tail_rows(&b, 0, 3, 2, &render, None)), ["def", "z"]);
        assert_eq!(plain(tail_rows(&b, 2, 3, 5, &render, None)), ["z"], "lines before start (cleared) are hidden");
        assert_eq!(plain(tail_rows(&b, 0, 2, 5, &render, None)), ["one", "abc", "def"], "lines from end on (paused) too");
        let not_z = Pattern::parse("!z", true).unwrap();
        assert_eq!(plain(tail_rows(&b, 0, 3, 5, &render, Some(&not_z))), ["one", "abc", "def"], "filtered out");
    }
}
