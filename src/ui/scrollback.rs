use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::text;
use crate::app::{Pane, Scrollback};
use crate::buffer::{Buffer, Seq};

pub fn render(frame: &mut Frame, area: Rect, sb: &mut Scrollback, pane: &Pane, strip: bool) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let view = layout(&pane.buffer, sb.top, body.width, body.height, strip);
    sb.top = view.top;
    sb.shown_top = view.shown_top;
    sb.page = body.height as usize;

    let buffer = &pane.buffer;
    let position = match sb.top {
        None => "end".to_string(),
        Some(t) => format!("line {}", t + 1),
    };
    let right = format!(
        "scrollback · lines {}-{} · {position}",
        buffer.first_seq() + 1,
        buffer.end_seq()
    );
    super::render_title(frame, title, sb.pane, pane, right);
    frame.render_widget(Paragraph::new(view.rows), body);
}

struct View {
    rows: Vec<Line<'static>>,
    /// Normalized: clamped to history, `None` once the end is in view.
    top: Option<Seq>,
    shown_top: Seq,
}

/// Rows at position `seq`: clear marks that sit just before line `seq`,
/// then the line itself (positions go up to `end_seq`, which has no line).
fn position_rows(buffer: &Buffer, seq: Seq, width: usize, strip: bool) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = buffer
        .clears()
        .filter(|m| m.seq == seq)
        .map(|m| Line::styled(format!("──── cleared at {} ────", m.at.format("%H:%M:%S")), super::dim()))
        .collect();
    if let Some(line) = buffer.get(seq) {
        rows.extend(text::rows(line, width, strip));
    }
    rows
}

fn layout(buffer: &Buffer, top: Option<Seq>, width: u16, height: u16, strip: bool) -> View {
    let (width, height) = (width as usize, height as usize);
    let (first, end) = (buffer.first_seq(), buffer.end_seq());

    if let Some(top) = top {
        let top = top.clamp(first, end);
        let mut rows = Vec::new();
        let mut seq = top;
        while seq <= end && rows.len() <= height {
            rows.extend(position_rows(buffer, seq, width, strip));
            seq += 1;
        }
        // Content from `top` overflows the screen: stay anchored there.
        if rows.len() > height {
            rows.truncate(height);
            return View { rows, top: Some(top), shown_top: top };
        }
    }

    // Following the end: bottom-anchored.
    let mut chunks = Vec::new();
    let mut count = 0;
    let mut seq = end + 1;
    while seq > first && count < height {
        seq -= 1;
        let rows = position_rows(buffer, seq, width, strip);
        count += rows.len();
        chunks.push(rows);
    }
    let mut rows: Vec<_> = chunks.into_iter().rev().flatten().collect();
    rows.drain(..rows.len().saturating_sub(height));
    View { rows, top: None, shown_top: seq }
}

#[cfg(test)]
mod tests {
    use chrono::Local;

    use super::*;

    fn buffer(n: usize) -> Buffer {
        let mut b = Buffer::new(100);
        for i in 0..n {
            b.push(format!("l{i}"));
        }
        b
    }

    fn plain(view: &View) -> Vec<String> {
        view.rows.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn follows_the_end() {
        let v = layout(&buffer(10), None, 20, 3, false);
        assert_eq!(plain(&v), ["l7", "l8", "l9"]);
        assert_eq!((v.top, v.shown_top), (None, 7));
    }

    #[test]
    fn anchored_view_and_back_to_follow_at_the_end() {
        let b = buffer(10);
        let v = layout(&b, Some(2), 20, 3, false);
        assert_eq!(plain(&v), ["l2", "l3", "l4"]);
        assert_eq!(v.top, Some(2));

        let v = layout(&b, Some(8), 20, 3, false);
        assert_eq!(v.top, None, "end in view: following again");
        assert_eq!(plain(&v), ["l7", "l8", "l9"]);
    }

    #[test]
    fn shows_clear_marks_inline() {
        let mut b = buffer(2);
        b.clear_view(Local::now());
        b.push("after".into());
        let rows = plain(&layout(&b, None, 40, 10, false));
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], "l1");
        assert!(rows[2].contains("cleared at"));
        assert_eq!(rows[3], "after");
    }

    #[test]
    fn mark_after_last_line_is_visible() {
        let mut b = buffer(2);
        b.clear_view(Local::now());
        let rows = plain(&layout(&b, None, 40, 10, false));
        assert!(rows.last().unwrap().contains("cleared at"));
    }
}
