use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::text;
use crate::app::Pane;
use crate::buffer::{Buffer, Seq};

fn split(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area)
}

/// Where a pane's lines go, below its title.
pub fn body(area: Rect) -> Rect {
    split(area)[1]
}

pub fn render(frame: &mut Frame, area: Rect, index: usize, pane: &Pane, strip: bool) {
    let [title, body] = split(area);
    let hidden = pane.buffer.view_start() - pane.buffer.first_seq();
    let mut right = super::lines_count(pane.buffer.len());
    if hidden > 0 {
        right.push_str(&format!(" ({hidden} hidden)"));
    }
    super::render_title(frame, title, index, pane, right);
    let rows = tail_rows(&pane.buffer, pane.buffer.view_start(), body.width, body.height, strip);
    frame.render_widget(Paragraph::new(rows), body);
}

/// The last `height` rows of lines from `start` on, bottom-anchored like a
/// terminal: the newest line is always on the last row.
fn tail_rows(buffer: &Buffer, start: Seq, width: u16, height: u16, strip: bool) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut chunks = Vec::new();
    let mut count = 0;
    let mut seq = buffer.end_seq();
    while seq > start && count < height {
        seq -= 1;
        let rows = text::rows(buffer.get(seq).unwrap_or_default(), width as usize, strip);
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

    #[test]
    fn keeps_newest_rows_with_partial_top_line() {
        let mut b = Buffer::new(100);
        for l in ["one", "abcdef", "z"] {
            b.push(l.into());
        }
        let rows: Vec<String> = tail_rows(&b, 0, 3, 2, false).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, ["def", "z"]);
        let rows: Vec<String> = tail_rows(&b, 2, 3, 5, false).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, ["z"], "lines before start (cleared) are hidden");
    }
}
