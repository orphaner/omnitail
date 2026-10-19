//! What a full-screen history view looks at: one pane's buffer, or all of
//! them merged in arrival order, seen through an optional filter. Keys
//! (moving, searching) and rendering both go through [`HistoryView`].

use std::collections::VecDeque;

use crate::app::Pane;
use crate::buffer::{ClearMark, Entry, Seq};
use crate::level::Level;
use crate::pattern::Pattern;

/// A position in a view: a sequence number for one pane, an index in the
/// merged order otherwise. Positions stay valid as lines arrive or get evicted.
pub type Pos = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Pane(usize),
    Merged(Merged),
}

/// All panes' lines in arrival order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Merged {
    /// Position of `entries[0]`.
    base: Pos,
    entries: VecDeque<(usize, Seq)>,
}

impl Merged {
    pub fn build(panes: &[Pane]) -> Self {
        let mut all: Vec<(u64, usize, Seq)> = panes
            .iter()
            .enumerate()
            .flat_map(|(i, p)| p.buffer.entries().map(move |(seq, e)| (e.arrival, i, seq)))
            .collect();
        all.sort_unstable();
        Self { base: 0, entries: all.into_iter().map(|(_, pane, seq)| (pane, seq)).collect() }
    }

    /// A line just arrived (the app calls this while the view is open).
    pub fn push(&mut self, pane: usize, seq: Seq, panes: &[Pane]) {
        self.entries.push_back((pane, seq));
        // Forget lines evicted from their pane, keeping positions stable.
        while let Some(&(p, s)) = self.entries.front() {
            if panes[p].buffer.entry(s).is_some() {
                break;
            }
            self.entries.pop_front();
            self.base += 1;
        }
    }
}

pub struct ViewLine<'a> {
    pub pane: usize,
    pub seq: Seq,
    pub entry: &'a Entry,
}

#[derive(Clone, Copy)]
pub struct HistoryView<'a> {
    panes: &'a [Pane],
    target: &'a Target,
    filter: Option<&'a Pattern>,
    /// Paused views end here, whatever arrived since.
    until: Option<Pos>,
    /// Lowest level shown; lines with no known level always are.
    level: Option<Level>,
}

impl<'a> HistoryView<'a> {
    pub fn new(panes: &'a [Pane], target: &'a Target, filter: Option<&'a Pattern>) -> Self {
        Self { panes, target, filter, until: None, level: None }
    }

    /// The same view, ending at `until` when set (a paused view).
    pub fn until(self, until: Option<Pos>) -> Self {
        Self { until, ..self }
    }

    /// The same view, showing only lines of `level` and up when set.
    pub fn level(self, level: Option<Level>) -> Self {
        Self { level, ..self }
    }

    pub fn is_merged(&self) -> bool {
        matches!(self.target, Target::Merged(_))
    }

    /// Oldest retained position.
    pub fn first(&self) -> Pos {
        match self.target {
            Target::Pane(p) => self.panes[*p].buffer.first_seq(),
            Target::Merged(m) => m.base,
        }
    }

    /// One past the newest position.
    pub fn end(&self) -> Pos {
        let end = match self.target {
            Target::Pane(p) => self.panes[*p].buffer.end_seq(),
            Target::Merged(m) => m.base + m.entries.len() as Pos,
        };
        self.until.map_or(end, |u| u.clamp(self.first(), end))
    }

    /// The line at `pos`, filter ignored.
    pub fn line(&self, pos: Pos) -> Option<ViewLine<'a>> {
        if pos >= self.end() {
            return None;
        }
        let (pane, seq) = match self.target {
            Target::Pane(p) => (*p, pos),
            Target::Merged(m) => *m.entries.get(usize::try_from(pos.checked_sub(m.base)?).ok()?)?,
        };
        let entry = self.panes[pane].buffer.entry(seq)?;
        Some(ViewLine { pane, seq, entry })
    }

    /// The line at `pos` if the filter and the level let it through.
    pub fn visible(&self, pos: Pos) -> Option<ViewLine<'a>> {
        self.line(pos).filter(|l| {
            Level::passes(l.entry.level, self.level) && self.filter.is_none_or(|f| f.accepts_entry(l.entry))
        })
    }

    /// Clear marks just before `pos` (single-pane views only).
    pub fn marks(&self, pos: Pos) -> Vec<&'a ClearMark> {
        match self.target {
            Target::Pane(p) => self.panes[*p].buffer.clears().filter(|m| m.seq == pos).collect(),
            Target::Merged(_) => Vec::new(),
        }
    }

    /// Nearest visible position strictly after (or before) `from`.
    pub fn next(&self, from: Pos, forward: bool) -> Option<Pos> {
        if forward {
            (from.saturating_add(1).max(self.first())..self.end()).find(|&p| self.visible(p).is_some())
        } else {
            (self.first()..from.min(self.end())).rev().find(|&p| self.visible(p).is_some())
        }
    }

    pub fn last_visible(&self) -> Option<Pos> {
        self.next(self.end(), false)
    }

    pub fn first_visible(&self) -> Option<Pos> {
        let first = self.first();
        if self.visible(first).is_some() { Some(first) } else { self.next(first, true) }
    }

    /// The position to treat as selected: `cursor` if visible, else the
    /// nearest visible line before it, else after it. `None` follows the end.
    pub fn resolve(&self, cursor: Option<Pos>) -> Option<Pos> {
        match cursor {
            None => self.last_visible(),
            Some(c) if self.visible(c).is_some() => Some(c),
            Some(c) => self.next(c, false).or_else(|| self.next(c, true)),
        }
    }

    /// Next visible line matching `pattern` from `from`, in `forward`
    /// direction, wrapping around once. Also says whether it wrapped.
    pub fn search(&self, from: Pos, forward: bool, pattern: &Pattern) -> Option<(Pos, bool)> {
        let hit = |p: &Pos| self.visible(*p).is_some_and(|l| pattern.accepts_entry(l.entry));
        let (first, end) = (self.first(), self.end());
        let found = if forward {
            (from.saturating_add(1).max(first)..end).find(hit).map(|p| (p, false))
        } else {
            (first..from.min(end)).rev().find(hit).map(|p| (p, false))
        };
        found.or_else(|| {
            let wrapped = if forward {
                (first..=from.min(end.saturating_sub(1))).find(hit)
            } else {
                (from.max(first)..end).rev().find(hit)
            };
            wrapped.map(|p| (p, true))
        })
    }

    /// (rank of the match at `at`, starting at 1; total matches).
    pub fn count(&self, pattern: &Pattern, at: Pos) -> (usize, usize) {
        let matches: Vec<Pos> = (self.first()..self.end())
            .filter(|&p| self.visible(p).is_some_and(|l| pattern.accepts_entry(l.entry)))
            .collect();
        let rank = matches.iter().filter(|&&p| p <= at).count();
        (rank, matches.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::source::SourceEvent;

    fn app() -> App {
        let mut app = App::with_names(&["a", "b"]);
        // Interleaved arrivals: a0 b0 a1 b1 a2
        for (pane, text) in [(0, "a0 info"), (1, "b0 error"), (0, "a1 error"), (1, "b1 info"), (0, "a2 info")] {
            app.ingest(pane, SourceEvent::Line(text.into()));
        }
        app
    }

    fn texts(view: &HistoryView) -> Vec<String> {
        (view.first()..view.end()).filter_map(|p| view.visible(p)).map(|l| l.entry.text.clone()).collect()
    }

    #[test]
    fn merged_follows_arrival_order() {
        let app = app();
        let target = Target::Merged(Merged::build(&app.panes));
        let view = HistoryView::new(&app.panes, &target, None);
        assert_eq!(texts(&view), ["a0 info", "b0 error", "a1 error", "b1 info", "a2 info"]);
        assert_eq!(view.line(1).unwrap().pane, 1);
    }

    #[test]
    fn filter_navigation_and_resolve() {
        let app = app();
        let target = Target::Pane(0);
        let filter = Pattern::parse("!error", true).unwrap();
        let view = HistoryView::new(&app.panes, &target, Some(&filter));
        assert_eq!(texts(&view), ["a0 info", "a2 info"]);
        assert_eq!(view.next(0, true), Some(2), "skips the filtered line");
        assert_eq!(view.next(2, false), Some(0));
        assert_eq!(view.resolve(Some(1)), Some(0), "hidden cursor snaps to a visible line");
        assert_eq!(view.resolve(None), Some(2));
    }

    #[test]
    fn paused_view_ends_where_it_was_paused() {
        let mut app = app();
        let target = Target::Pane(0);
        let view = HistoryView::new(&app.panes, &target, None).until(Some(2));
        assert_eq!(texts(&view), ["a0 info", "a1 error"]);
        assert_eq!(view.last_visible(), Some(1));
        app.ingest(0, SourceEvent::Line("a3".into()));
        let view = HistoryView::new(&app.panes, &target, None).until(Some(2));
        assert_eq!(view.end(), 2, "new lines stay out");
    }

    #[test]
    fn search_wraps_and_counts() {
        let app = app();
        let target = Target::Merged(Merged::build(&app.panes));
        let view = HistoryView::new(&app.panes, &target, None);
        let error = Pattern::parse("error", false).unwrap();
        assert_eq!(view.search(4, false, &error), Some((2, false)));
        assert_eq!(view.search(2, false, &error), Some((1, false)));
        assert_eq!(view.search(1, false, &error), Some((2, true)), "wraps to the end");
        assert_eq!(view.search(2, true, &error), Some((1, true)), "wraps to the start");
        assert_eq!(view.count(&error, 2), (2, 2));
        assert_eq!(view.search(0, true, &Pattern::parse("nope", false).unwrap()), None);
    }
}
