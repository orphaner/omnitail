use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::text::Render;
use crate::app::Pane;
use crate::buffer::{Buffer, Seq};

fn split(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area)
}

/// Where a pane's lines go, below its title.
pub fn body(area: Rect) -> Rect {
    split(area)[1]
}

pub fn render(frame: &mut Frame, area: Rect, index: usize, pane: &Pane, strip: bool, wrap: bool) {
    let [title, body] = split(area);
    let hidden = pane.buffer.view_start() - pane.buffer.first_seq();
    let mut count = super::lines_count(pane.buffer.len());
    if hidden > 0 {
        count.push_str(&format!(" ({hidden} hidden)"));
    }
    let full = format!("{}{count}", super::color_label(pane));
    super::render_title(frame, title, index, pane, &[full, count]);
    let render = Render { width: body.width as usize, strip, scheme: pane.color.active(), wrap, hscroll: 0 };
    let start = pane.buffer.view_start();
    let rows = tail_rows(&pane.buffer, start, pane.view_end().max(start), body.height as usize, &render);
    frame.render_widget(Paragraph::new(rows), body);
}

/// The last `height` rows of lines `start..end`, bottom-anchored like a
/// terminal: the newest line is always on the last row.
fn tail_rows(buffer: &Buffer, start: Seq, end: Seq, height: usize, render: &Render) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    let mut count = 0;
    let mut seq = end;
    while seq > start && count < height {
        seq -= 1;
        let rows = render.rows(buffer.get(seq).unwrap_or_default());
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
        let render = Render { width: 3, strip: false, scheme: None, wrap: true, hscroll: 0 };
        assert_eq!(plain(tail_rows(&b, 0, 3, 2, &render)), ["def", "z"]);
        assert_eq!(plain(tail_rows(&b, 2, 3, 5, &render)), ["z"], "lines before start (cleared) are hidden");
        assert_eq!(plain(tail_rows(&b, 0, 2, 5, &render)), ["one", "abc", "def"], "lines from end on (paused) too");
    }
}
