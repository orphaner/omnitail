use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use super::{ACCENT, BADGE_BG, BAR_FG, MUTED, OK, POPUP_BG, WHITE};
use crate::app::{Pane, SchemeEntry, SchemePicker, SelectPurpose};

pub fn render(frame: &mut Frame, area: Rect, purpose: SelectPurpose, selected: usize, panes: &[Pane]) {
    let title = match purpose {
        SelectPurpose::Scrollback => " Scrollback ",
        SelectPurpose::Find { .. } => " Search in ",
        SelectPurpose::Split => " Split into pods / containers ",
        SelectPurpose::Clear => " Clear ",
        SelectPurpose::Pause => " Pause/resume ",
        SelectPurpose::Filter => " Filter ",
        SelectPurpose::Scheme => " Color scheme ",
        SelectPurpose::Level => " Level filter ",
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
        .style(Style::new().fg(BAR_FG).bg(POPUP_BG))
        .highlight_style(Style::new().fg(WHITE).bg(BADGE_BG));
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut ListState::default().with_selected(Some(selected)));
}

/// `Esc` on the first screen: quit for real?
pub fn render_quit(frame: &mut Frame, area: Rect) {
    let text = Line::from(vec![
        Span::styled(" y Enter ", Style::new().fg(WHITE).bg(BADGE_BG)),
        Span::raw(" quit   "),
        Span::styled(" any key ", Style::new().fg(WHITE).bg(BADGE_BG)),
        Span::raw(" stay "),
    ]);
    let popup = centered(area, text.width() as u16 + 4, 3);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Quit omnitail? ")
        .border_style(Style::new().fg(ACCENT));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).centered().block(block).style(Style::new().fg(BAR_FG).bg(POPUP_BG)),
        popup,
    );
}

/// The scheme chooser: a filter line, then the matching schemes.
pub fn render_schemes(frame: &mut Frame, area: Rect, picker: &SchemePicker, entries: &[SchemeEntry], current: &str) {
    let name_w = entries.iter().map(|e| e.name.len()).max().unwrap_or(4).max(10);
    let popup = centered(area, 92.min(area.width.saturating_sub(4)), 26.min(area.height.saturating_sub(2)));
    let title = format!(" Color scheme · pane {} ", picker.pane + 1);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .title_bottom(Line::from(" type to filter · ↑↓ choose · Enter apply · Esc cancel ").right_aligned())
        .border_style(Style::new().fg(ACCENT))
        .style(Style::new().fg(BAR_FG).bg(POPUP_BG));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [query, _, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    let typed = if picker.query.is_empty() {
        Span::styled("name or description…", Style::new().fg(MUTED).italic())
    } else {
        Span::styled(picker.query.clone(), Style::new().fg(WHITE).bold())
    };
    let cursor = Span::styled(" ", Style::new().bg(WHITE));
    let count = Span::styled(format!("   {} schemes", entries.len()), Style::new().fg(MUTED));
    frame.render_widget(Paragraph::new(Line::from(vec![Span::raw(" › "), typed, cursor, count])), query);

    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            let mark = if e.name == current { Span::styled(" ● ", Style::new().fg(OK)) } else { Span::raw("   ") };
            ListItem::new(Line::from(vec![
                mark,
                Span::raw(format!("{:name_w$}  ", e.name)).bold(),
                Span::styled(format!("{:<9}  ", e.origin), Style::new().fg(MUTED)),
                Span::raw(e.description.clone()),
            ]))
        })
        .collect();
    let list_widget = List::new(items).highlight_style(Style::new().fg(WHITE).bg(BADGE_BG));
    let selected = (!entries.is_empty()).then_some(picker.selected);
    frame.render_stateful_widget(list_widget, list, &mut ListState::default().with_selected(selected));
    if entries.is_empty() {
        let none = Paragraph::new("   no scheme matches").style(Style::new().fg(MUTED).bg(POPUP_BG));
        frame.render_widget(none, list);
    }
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
