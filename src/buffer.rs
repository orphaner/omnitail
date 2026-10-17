//! Bounded line history of one source.
//!
//! Every line gets a sequence number that never changes, even when older
//! lines are evicted, so views can point at a line without being shifted by
//! new arrivals. "Clearing" a pane only records a mark: the pane shows what
//! comes after it, while the scrollback still has everything.

use std::collections::VecDeque;

use chrono::{DateTime, Local};

pub type Seq = u64;

pub struct Buffer {
    lines: VecDeque<String>,
    capacity: usize,
    /// Sequence number the next pushed line will get.
    next_seq: Seq,
    /// In increasing `seq` order.
    clears: VecDeque<ClearMark>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearMark {
    /// First line that came after the clear.
    pub seq: Seq,
    pub at: DateTime<Local>,
    /// Why: "cleared" by the user, or a source reset such as "truncated".
    pub reason: &'static str,
}

impl Buffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            lines: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            next_seq: 0,
            clears: VecDeque::new(),
        }
    }

    pub fn push(&mut self, line: String) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            // Already the oldest seq once the new line is in.
            let first = self.first_seq();
            // A mark at `first` is still meaningful: it sits before the oldest line.
            while self.clears.front().is_some_and(|m| m.seq < first) {
                self.clears.pop_front();
            }
        }
        self.lines.push_back(line);
        self.next_seq += 1;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Sequence number of the oldest retained line (`end_seq` when empty).
    pub fn first_seq(&self) -> Seq {
        self.next_seq - self.lines.len() as Seq
    }

    /// One past the newest line.
    pub fn end_seq(&self) -> Seq {
        self.next_seq
    }

    pub fn get(&self, seq: Seq) -> Option<&str> {
        let idx = seq.checked_sub(self.first_seq())?;
        self.lines.get(usize::try_from(idx).ok()?).map(String::as_str)
    }

    /// Hides everything received so far from the pane view.
    pub fn clear_view(&mut self, at: DateTime<Local>, reason: &'static str) {
        let seq = self.next_seq;
        match self.clears.back_mut() {
            // Nothing arrived since the last clear: just refresh it.
            Some(last) if last.seq == seq => *last = ClearMark { seq, at, reason },
            _ => self.clears.push_back(ClearMark { seq, at, reason }),
        }
    }

    /// First line the pane view shows.
    pub fn view_start(&self) -> Seq {
        self.clears.back().map_or(0, |m| m.seq).max(self.first_seq())
    }

    pub fn clears(&self) -> impl Iterator<Item = &ClearMark> {
        self.clears.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(cap: usize, n: usize) -> Buffer {
        let mut b = Buffer::new(cap);
        for i in 0..n {
            b.push(format!("l{i}"));
        }
        b
    }

    #[test]
    fn evicts_oldest_and_keeps_sequence_numbers() {
        let b = buf(3, 5);
        assert_eq!(b.len(), 3);
        assert_eq!((b.first_seq(), b.end_seq()), (2, 5));
        assert_eq!(b.get(1), None);
        assert_eq!(b.get(2), Some("l2"));
        assert_eq!(b.get(4), Some("l4"));
        assert_eq!(b.get(5), None);
    }

    #[test]
    fn clear_hides_from_view_but_keeps_history() {
        let mut b = buf(10, 3);
        assert_eq!(b.view_start(), 0);
        b.clear_view(Local::now(), "cleared");
        assert_eq!(b.view_start(), 3);
        b.push("after".into());
        assert_eq!(b.get(b.view_start()), Some("after"));
        assert_eq!(b.get(0), Some("l0"));
    }

    #[test]
    fn repeated_clear_without_new_lines_is_one_mark() {
        let mut b = buf(10, 2);
        b.clear_view(Local::now(), "cleared");
        b.clear_view(Local::now(), "cleared");
        assert_eq!(b.clears().count(), 1);
        b.push("x".into());
        b.clear_view(Local::now(), "cleared");
        assert_eq!(b.clears().count(), 2);
    }

    #[test]
    fn marks_older_than_history_are_dropped() {
        let mut b = buf(2, 1);
        b.clear_view(Local::now(), "cleared"); // mark at seq 1
        b.push("a".into()); // seq 1, buffer [0, 1]
        b.push("b".into()); // seq 2, buffer [1, 2]: mark still before oldest line
        assert_eq!(b.clears().count(), 1);
        b.push("c".into()); // buffer [2, 3]
        assert_eq!(b.clears().count(), 0);
        assert_eq!(b.view_start(), 2);
    }
}
