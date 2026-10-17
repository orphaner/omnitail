use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::text::Render;
use crate::app::{Pane, Scrollback};
use crate::buffer::{Buffer, Seq};

pub fn render(frame: &mut Frame, area: Rect, sb: &mut Scrollback, pane: &Pane, strip: bool, wrap: bool) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let render = Render { width: body.width as usize, strip, scheme: pane.color.active(), wrap, hscroll: sb.hscroll };
    let view = layout(&pane.buffer, sb.cursor, sb.top, body.height as usize, &render);
    sb.top = view.top;
    sb.page = body.height as usize;

    let buffer = &pane.buffer;
    let position = match sb.cursor {
        None => "end".to_string(),
        Some(c) => format!("line {}", c + 1),
    };
    let lines = format!("lines {}-{}", buffer.first_seq() + 1, buffer.end_seq());
    let right = [format!("scrollback · {lines} · {position}"), position.clone()];
    super::render_title(frame, title, sb.pane, pane, &right);
    frame.render_widget(Paragraph::new(view.rows), body);
}

struct View {
    rows: Vec<Line<'static>>,
    top: Seq,
}

/// Rows at position `seq`: clear marks that sit just before line `seq`,
/// then the line itself (positions go up to `end_seq`, which has no line).
fn position_rows(buffer: &Buffer, seq: Seq, render: &Render, cursor: bool) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = buffer
        .clears()
        .filter(|m| m.seq == seq)
        .map(|m| Line::styled(format!("──── {} at {} ────", m.reason, m.at.format("%H:%M:%S")), super::dim()))
        .collect();
    if let Some(line) = buffer.get(seq) {
        let lines = render.rows(line);
        if cursor {
            rows.extend(lines.into_iter().map(|l| {
                let style = l.style.bg(super::CURSOR_BG);
                l.style(style)
            }));
        } else {
            rows.extend(lines);
        }
    }
    rows
}

/// Rows of positions `..=bottom`, bottom-anchored, and the top position.
fn bottom_anchored(buffer: &Buffer, bottom: Seq, cursor: Seq, height: usize, render: &Render) -> View {
    let mut chunks = Vec::new();
    let mut count = 0;
    let mut seq = bottom + 1;
    while seq > buffer.first_seq() && count < height {
        seq -= 1;
        let rows = position_rows(buffer, seq, render, seq == cursor);
        count += rows.len();
        chunks.push(rows);
    }
    let mut rows: Vec<_> = chunks.into_iter().rev().flatten().collect();
    rows.drain(..rows.len().saturating_sub(height));
    View { rows, top: seq }
}

/// Lays out the screen so that the cursor line is visible, moving as
/// little as possible from the previous `top`. Without a cursor, follows
/// the end of the history.
fn layout(buffer: &Buffer, cursor: Option<Seq>, top: Seq, height: usize, render: &Render) -> View {
    let (first, end) = (buffer.first_seq(), buffer.end_seq());
    let last = end.saturating_sub(1).max(first);
    let Some(cursor) = cursor.map(|c| c.clamp(first, last)) else {
        // Following: bottom-anchored on the very end, marks after the last line included.
        return bottom_anchored(buffer, end, last, height, render);
    };
    let top = top.clamp(first, last);
    if cursor < top {
        return top_anchored(buffer, cursor, cursor, height, render);
    }
    // Is the cursor line fully on screen when starting from `top`?
    let mut used = 0;
    for seq in top..=cursor {
        used += position_rows(buffer, seq, render, false).len();
        if used > height {
            return bottom_anchored(buffer, cursor, cursor, height, render);
        }
    }
    top_anchored(buffer, top, cursor, height, render)
}

fn top_anchored(buffer: &Buffer, top: Seq, cursor: Seq, height: usize, render: &Render) -> View {
    let mut rows = Vec::new();
    let mut seq = top;
    while seq <= buffer.end_seq() && rows.len() < height {
        rows.extend(position_rows(buffer, seq, render, seq == cursor));
        seq += 1;
    }
    rows.truncate(height);
    View { rows, top }
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

    fn render() -> Render<'static> {
        Render { width: 40, strip: false, scheme: None, wrap: true, hscroll: 0 }
    }

    fn plain(view: &View) -> Vec<String> {
        view.rows.iter().map(|l| l.to_string()).collect()
    }

    fn highlighted(view: &View) -> Vec<String> {
        view.rows.iter().filter(|l| l.style.bg == Some(super::super::CURSOR_BG)).map(|l| l.to_string()).collect()
    }

    #[test]
    fn follows_the_end_with_cursor_on_last_line() {
        let v = layout(&buffer(10), None, 0, 3, &render());
        assert_eq!(plain(&v), ["l7", "l8", "l9"]);
        assert_eq!(highlighted(&v), ["l9"]);
        assert_eq!(v.top, 7);
    }

    #[test]
    fn view_moves_only_to_keep_the_cursor_visible() {
        let b = buffer(10);
        // Cursor inside the previous screen: nothing moves.
        let v = layout(&b, Some(5), 4, 3, &render());
        assert_eq!((plain(&v), v.top), (vec!["l4".into(), "l5".into(), "l6".into()], 4));
        // Above it: the cursor becomes the top line.
        let v = layout(&b, Some(2), 4, 3, &render());
        assert_eq!((plain(&v)[0].as_str(), v.top), ("l2", 2));
        // Below it: the cursor becomes the bottom line.
        let v = layout(&b, Some(8), 2, 3, &render());
        assert_eq!(plain(&v), ["l6", "l7", "l8"]);
        assert_eq!(highlighted(&v), ["l8"]);
    }

    #[test]
    fn cursor_is_clamped_to_history() {
        let v = layout(&buffer(3), Some(99), 0, 5, &render());
        assert_eq!(highlighted(&v), ["l2"]);
    }

    #[test]
    fn shows_clear_marks_inline() {
        let mut b = buffer(2);
        b.clear_view(Local::now(), "cleared");
        b.push("after".into());
        let rows = plain(&layout(&b, None, 0, 10, &render()));
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], "l1");
        assert!(rows[2].contains("cleared at"));
        assert_eq!(rows[3], "after");
    }

    #[test]
    fn mark_after_last_line_is_visible() {
        let mut b = buffer(2);
        b.clear_view(Local::now(), "cleared");
        let rows = plain(&layout(&b, None, 0, 10, &render()));
        assert!(rows.last().unwrap().contains("cleared at"));
    }
}
