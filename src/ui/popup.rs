use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Clear, List, ListState};

use super::ACCENT;
use crate::app::{Pane, SelectPurpose};

pub fn render(frame: &mut Frame, area: Rect, purpose: SelectPurpose, selected: usize, panes: &[Pane]) {
    let title = match purpose {
        SelectPurpose::Scrollback => " Scrollback ",
        SelectPurpose::Clear => " Clear ",
    };
    let max_name = (area.width as usize).saturating_sub(8);
    let items: Vec<String> = panes
        .iter()
        .enumerate()
        .map(|(i, p)| format!(" {} {} ", i + 1, super::ellipsize(&p.name, max_name)))
        .collect();
    let inner_width = items.iter().map(|s| s.chars().count()).max().unwrap_or(0).max(title.len());
    let popup = centered(area, inner_width as u16 + 2, items.len() as u16 + 2);

    let list = List::new(items)
        .block(Block::bordered().border_type(BorderType::Rounded).title(title).border_style(Style::new().fg(ACCENT)))
        .highlight_style(Style::new().reversed());
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut ListState::default().with_selected(Some(selected)));
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
