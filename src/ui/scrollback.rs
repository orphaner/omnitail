use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::text::Render;
use crate::app::{Pane, Scrollback};
use crate::view::{HistoryView, Pos};

pub struct ScrollbackOptions {
    pub strip: bool,
    pub wrap: bool,
    /// Arrival time before each line.
    pub times: bool,
}

pub fn render(frame: &mut Frame, area: Rect, sb: &mut Scrollback, panes: &[Pane], opts: &ScrollbackOptions) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    // The cursor left the end (or came back): count new lines from there.
    sb.left_end = match (sb.cursor, sb.left_end) {
        (None, _) => None,
        (Some(_), None) => Some(sb.view(panes).end()),
        (Some(_), kept) => kept,
    };
    sb.count_new_matches(panes);
    let view = sb.view(panes);
    let highlight = sb.search.as_ref().map(|s| &s.pattern);
    let layout = Painter { view, panes, opts, highlight, width: body.width as usize, hscroll: sb.hscroll };
    let shown = layout.layout(sb.cursor, sb.top, body.height as usize);

    let position = match sb.cursor {
        None => "end".to_string(),
        Some(c) if view.is_merged() => format!("{}/{}", c + 1, view.end()),
        Some(c) => format!("line {}", c + 1),
    };
    let mut details = Vec::new();
    if let Some(n) = sb.arrived_since_pause(panes) {
        details.push(format!("⏸ paused +{n}"));
    }
    if let Some(n) = sb.arrived_below(panes).filter(|&n| n > 0) {
        details.push(format!("↓ +{n} new"));
    }
    if let Some(s) = &sb.search {
        details.push(if s.total > 0 { format!("match {}/{}", s.rank, s.total) } else { format!("/{}", s.pattern.text) });
    }
    if let Some(l) = sb.level {
        details.push(format!("≥{l}"));
    }
    if let Some(f) = &sb.filter {
        details.push(format!("filter: {}", f.text));
    }
    let details = details.iter().map(|d| format!("{d} · ")).collect::<String>();
    let paused = sb.arrived_since_pause(panes).map(|n| format!("⏸ +{n} · ")).unwrap_or_default();
    let below = sb.arrived_below(panes).filter(|&n| n > 0).map(|n| format!("↓ +{n} · ")).unwrap_or_default();
    let right = [
        format!("scrollback · {details}{position}"),
        format!("{details}{position}"),
        format!("{paused}{below}{position}"),
    ];
    match &sb.target {
        crate::view::Target::Pane(p) => super::render_title(frame, title, *p, &panes[*p], &right),
        crate::view::Target::Merged(_) => {
            let left = Line::from(vec![
                Span::styled(" B ", Style::new().fg(super::WHITE).bg(super::BADGE_BG).bold()),
                Span::raw(format!(" all {} panes, in arrival order ", panes.len())).bold(),
            ]);
            super::render_bar(frame, title, left, &right);
        }
    }
    sb.top = shown.top;
    sb.page = body.height as usize;
    frame.render_widget(Paragraph::new(shown.rows), body);
}

struct Shown {
    rows: Vec<Line<'static>>,
    top: Pos,
}

struct Painter<'a> {
    view: HistoryView<'a>,
    panes: &'a [Pane],
    opts: &'a ScrollbackOptions,
    highlight: Option<&'a crate::pattern::Pattern>,
    width: usize,
    hscroll: usize,
}

impl Painter<'_> {
    /// Rows at `pos`: clear marks just before it, then its line if visible,
    /// behind its prefix (arrival time, pane badge).
    fn rows(&self, pos: Pos, cursor: Option<Pos>) -> Vec<Line<'static>> {
        let mut rows: Vec<Line<'static>> = self
            .view
            .marks(pos)
            .into_iter()
            .map(|m| Line::styled(format!("──── {} at {} ────", m.reason, m.at.format("%H:%M:%S")), super::dim()))
            .collect();
        let Some(line) = self.view.visible(pos) else { return rows };

        let mut prefix: Vec<Span<'static>> = Vec::new();
        if self.opts.times {
            prefix.push(Span::styled(format!("{} ", line.entry.at.format("%H:%M:%S%.3f")), super::dim()));
        }
        if self.view.is_merged() {
            prefix.extend(super::badge(format!("{:>2} ", line.pane + 1), line.pane));
        }
        let pane = &self.panes[line.pane];
        prefix.extend(super::origin_badges(line.entry, pane.badges));
        let render = Render {
            width: self.width,
            strip: self.opts.strip,
            scheme: pane.scheme_for(line.entry),
            wrap: self.opts.wrap,
            hscroll: self.hscroll,
            highlight: self.highlight,
        };
        let is_cursor = Some(pos) == cursor;
        for row in super::prefixed_rows(&render, &line.entry.text, prefix) {
            rows.push(if is_cursor { row.patch_style(Style::new().bg(super::CURSOR_BG)) } else { row });
        }
        rows
    }

    /// Lays out the screen so the cursor line is visible, moving as little as
    /// possible from the previous `top`. Without a cursor, follows the end.
    fn layout(&self, cursor: Option<Pos>, top: Pos, height: usize) -> Shown {
        let selected = self.view.resolve(cursor);
        let (first, end) = (self.view.first(), self.view.end());
        let Some(c) = cursor.and(selected) else {
            // Following: bottom-anchored on the very end, marks after the last line included.
            return self.bottom_anchored(end, selected, height);
        };
        let top = top.clamp(first, end.saturating_sub(1).max(first));
        if c < top {
            return self.top_anchored(c, selected, height);
        }
        // Is the cursor line fully on screen when starting from `top`?
        let mut used = 0;
        for pos in top..=c {
            used += self.rows(pos, None).len();
            if used > height {
                return self.bottom_anchored(c, selected, height);
            }
        }
        self.top_anchored(top, selected, height)
    }

    fn bottom_anchored(&self, bottom: Pos, cursor: Option<Pos>, height: usize) -> Shown {
        let mut chunks = Vec::new();
        let mut count = 0;
        let mut pos = bottom + 1;
        while pos > self.view.first() && count < height {
            pos -= 1;
            let rows = self.rows(pos, cursor);
            count += rows.len();
            chunks.push(rows);
        }
        let mut rows: Vec<_> = chunks.into_iter().rev().flatten().collect();
        rows.drain(..rows.len().saturating_sub(height));
        Shown { rows, top: pos }
    }

    fn top_anchored(&self, top: Pos, cursor: Option<Pos>, height: usize) -> Shown {
        let mut rows = Vec::new();
        let mut pos = top;
        while pos <= self.view.end() && rows.len() < height {
            rows.extend(self.rows(pos, cursor));
            pos += 1;
        }
        rows.truncate(height);
        Shown { rows, top }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Local;

    use super::*;
    use crate::app::App;
    use crate::pattern::Pattern;
    use crate::source::SourceEvent;
    use crate::view::{Merged, Target};

    fn app(lines: &[&str]) -> App {
        let mut app = App::with_names(&["a"]);
        for l in lines {
            app.ingest(0, SourceEvent::Line(l.to_string()));
        }
        app
    }

    fn numbered(n: usize) -> App {
        app(&(0..n).map(|i| format!("l{i}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn opts() -> ScrollbackOptions {
        ScrollbackOptions { strip: false, wrap: true, times: false }
    }

    fn shown(app: &App, target: &Target, filter: Option<&Pattern>, cursor: Option<Pos>, top: Pos, height: usize) -> (Vec<String>, Vec<String>, Pos) {
        let opts = opts();
        let layout = Painter {
            view: HistoryView::new(&app.panes, target, filter),
            panes: &app.panes,
            opts: &opts,
            highlight: None,
            width: 40,
            hscroll: 0,
        };
        let s = layout.layout(cursor, top, height);
        let text = |l: &Line| l.to_string();
        let cursor_rows = s.rows.iter().filter(|l| l.style.bg == Some(super::super::CURSOR_BG)).map(text).collect();
        (s.rows.iter().map(text).collect(), cursor_rows, s.top)
    }

    #[test]
    fn follows_the_end_with_cursor_on_last_line() {
        let app = numbered(10);
        let (rows, cursor, top) = shown(&app, &Target::Pane(0), None, None, 0, 3);
        assert_eq!((rows, cursor, top), (vec!["l7".into(), "l8".into(), "l9".into()], vec!["l9".into()], 7));
    }

    #[test]
    fn view_moves_only_to_keep_the_cursor_visible() {
        let app = numbered(10);
        let t = Target::Pane(0);
        assert_eq!(shown(&app, &t, None, Some(5), 4, 3).0, ["l4", "l5", "l6"], "inside: nothing moves");
        assert_eq!(shown(&app, &t, None, Some(2), 4, 3).2, 2, "above: cursor becomes the top");
        let (rows, cursor, _) = shown(&app, &t, None, Some(8), 2, 3);
        assert_eq!((rows, cursor), (vec!["l6".into(), "l7".into(), "l8".into()], vec!["l8".into()]), "below: bottom");
    }

    #[test]
    fn filter_hides_lines_and_cursor_snaps() {
        let app = app(&["keep 1", "drop", "keep 2", "drop"]);
        let filter = Pattern::parse("keep", false).unwrap();
        let (rows, cursor, _) = shown(&app, &Target::Pane(0), Some(&filter), Some(3), 0, 5);
        assert_eq!(rows, ["keep 1", "keep 2"]);
        assert_eq!(cursor, ["keep 2"]);
    }

    #[test]
    fn clear_marks_inline_and_after_last_line() {
        let mut app = numbered(2);
        app.panes[0].buffer.clear_view(Local::now(), "cleared");
        let (rows, ..) = shown(&app, &Target::Pane(0), None, None, 0, 10);
        assert!(rows.last().unwrap().contains("cleared at"), "{rows:?}");
        app.ingest(0, SourceEvent::Line("after".into()));
        let (rows, ..) = shown(&app, &Target::Pane(0), None, None, 0, 10);
        assert_eq!(rows.len(), 4);
        assert!(rows[2].contains("cleared at"));
    }

    #[test]
    fn merged_rows_carry_a_pane_badge() {
        let mut app = App::with_names(&["a", "b"]);
        app.ingest(0, SourceEvent::Line("from a".into()));
        app.ingest(1, SourceEvent::Line("from b".into()));
        let target = Target::Merged(Merged::build(&app.panes));
        let (rows, ..) = shown(&app, &target, None, None, 0, 5);
        assert_eq!(rows, [" 1  from a", " 2  from b"]);
    }
}
