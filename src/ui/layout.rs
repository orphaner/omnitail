use ratatui::layout::{Constraint, Layout, Rect};

/// How panes share the screen. Only stacking exists for now; grids or
/// user-sized layouts will be other implementations.
pub trait PaneLayout {
    fn areas(&self, area: Rect, panes: usize) -> Vec<Rect>;
}

/// Panes on top of each other, sharing the height equally (multitail's default).
pub struct Stacked;

impl PaneLayout for Stacked {
    fn areas(&self, area: Rect, panes: usize) -> Vec<Rect> {
        Layout::vertical(vec![Constraint::Fill(1); panes]).split(area).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacked_splits_height() {
        let areas = Stacked.areas(Rect::new(0, 0, 80, 30), 3);
        assert_eq!(areas.iter().map(|a| a.height).collect::<Vec<_>>(), [10, 10, 10]);
        assert!(areas.iter().all(|a| a.width == 80));
        assert_eq!(areas[2].y, 20);
    }
}
