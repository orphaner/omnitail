//! Full-screen key reference (`h`): sections flowed into as many columns
//! as the screen allows, the current view's section first.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Help, Mode};

pub struct Section {
    pub title: &'static str,
    pub keys: &'static [(&'static str, &'static str)],
}

pub const PANES: Section = Section {
    title: "Panes",
    keys: &[
        ("b", "history of a pane"),
        ("B", "all panes merged, in arrival order"),
        ("/", "highlight a pattern everywhere"),
        ("E e", "filter all panes / one pane"),
        ("N n", "clear all panes / one pane"),
        ("P p", "pause all panes / one pane"),
        ("C c", "colors on/off: all / one pane"),
        ("w", "wrap long lines, or cut them"),
        ("h F1", "this help"),
        ("q Esc", "quit"),
        ("Ctrl+C", "quit, from anywhere"),
    ],
};

pub const HISTORY: Section = Section {
    title: "History (b, B)",
    keys: &[
        ("↑ ↓  j k", "move the cursor"),
        ("PgUp PgDn", "move a page (also u d)"),
        ("g G", "first line / follow the end"),
        ("/ ?", "search down / up"),
        ("n N", "next match / previous one"),
        ("&", "show matching lines only"),
        ("Enter", "line detail, JSON pretty-printed"),
        ("y", "copy the line"),
        ("p", "pause the view"),
        ("t", "arrival times"),
        ("w", "wrap or cut lines"),
        ("← →", "scroll sideways (cut lines)"),
        ("q Esc", "back"),
    ],
};

pub const DETAIL: Section = Section {
    title: "Line detail",
    keys: &[
        ("↑ ↓  j k", "scroll"),
        ("PgUp PgDn", "move a page"),
        ("g G", "top / bottom"),
        ("y", "copy the line"),
        ("q Esc Enter", "back"),
    ],
};

pub const PICKER: Section = Section {
    title: "Pane picker",
    keys: &[("1-9", "pick a pane"), ("↑ ↓ Enter", "choose"), ("Esc", "cancel")],
};

pub const PROMPT: Section = Section {
    title: "Typing a pattern",
    keys: &[
        ("Enter", "apply (empty: remove)"),
        ("Esc", "cancel"),
        ("↑ ↓", "previous patterns"),
        ("Ctrl+A Ctrl+E", "start / end of line"),
        ("Ctrl+U", "erase"),
    ],
};

pub const PATTERNS: Section = Section {
    title: "Patterns",
    keys: &[
        ("error", "regex, any case"),
        ("Error", "an uppercase letter: exact case"),
        ("!info", "lines NOT matching (filters)"),
        ("500|503", "either"),
        ("^GET", "at line start"),
    ],
};

/// Sections in display order: the one for `mode` first.
pub fn sections(mode: &Mode) -> Vec<&'static Section> {
    let current = match mode {
        Mode::Panes => &PANES,
        Mode::Select { .. } => &PICKER,
        Mode::Scrollback(_) => &HISTORY,
        Mode::Detail(_) => &DETAIL,
    };
    let mut all: Vec<&Section> = vec![&PANES, &HISTORY, &DETAIL, &PICKER, &PROMPT, &PATTERNS];
    all.retain(|s| !std::ptr::eq(*s, current));
    all.insert(0, current);
    all
}

const GAP: usize = 4;

fn key_width(section: &Section) -> usize {
    section.keys.iter().map(|(k, _)| k.width() + 2).max().unwrap_or(0)
}

fn section_lines(section: &Section, current: bool, key_width: usize) -> Vec<Line<'static>> {
    let title_style = if current {
        Style::new().fg(super::ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::new().fg(super::WHITE).add_modifier(Modifier::BOLD)
    };
    let mut lines = vec![Line::styled(section.title, title_style)];
    for (key, desc) in section.keys {
        let cap = format!(" {key} ");
        let pad = " ".repeat(key_width.saturating_sub(cap.width()) + 1);
        lines.push(Line::from(vec![
            Span::styled(cap, Style::new().fg(super::WHITE).bg(super::KEY_BG)),
            Span::raw(pad),
            Span::styled(*desc, Style::new().fg(super::BAR_FG)),
        ]));
    }
    lines.push(Line::default());
    lines
}

/// Columns of lines, and their width: sections go, in order, to the
/// shortest column.
fn columns(mode: &Mode, width: usize) -> (Vec<Vec<Line<'static>>>, usize) {
    let sections = sections(mode);
    let key_w = sections.iter().map(|s| key_width(s)).max().unwrap_or(0);
    let col_w = sections
        .iter()
        .flat_map(|s| s.keys.iter().map(|(_, d)| key_w + 1 + d.width()))
        .max()
        .unwrap_or(30);
    let count = ((width + GAP) / (col_w + GAP)).clamp(1, sections.len());
    let mut cols: Vec<Vec<Line<'static>>> = vec![Vec::new(); count];
    for (i, section) in sections.iter().enumerate() {
        let shortest = (0..count).min_by_key(|&c| cols[c].len()).unwrap_or(0);
        // The current section always opens the first column.
        let col = if i == 0 { 0 } else { shortest };
        cols[col].extend(section_lines(section, i == 0, key_w));
    }
    (cols, col_w)
}

pub fn render(frame: &mut Frame, area: Rect, help: &mut Help, mode: &Mode) {
    frame.render_widget(Clear, area);
    let [title, _, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)])
            .areas(area);
    let left = Line::from(vec![
        Span::styled(" mtr ", Style::new().fg(super::WHITE).bg(super::BADGE_BG).bold()),
        Span::raw(" keys ").bold(),
    ]);
    super::render_bar(frame, title, left, &["Esc to close".into()]);

    let inner = Rect { x: body.x + 2, width: body.width.saturating_sub(4), ..body };
    let (cols, col_w) = columns(mode, inner.width as usize);
    let tallest = cols.iter().map(Vec::len).max().unwrap_or(0);
    help.max_scroll = tallest.saturating_sub(inner.height as usize);
    help.scroll = help.scroll.min(help.max_scroll);

    // Fixed-width columns, centered: no gulf between them on wide screens.
    let col_areas = Layout::horizontal(vec![Constraint::Length(col_w as u16); cols.len()])
        .spacing(GAP as u16)
        .flex(Flex::Center)
        .split(inner);
    for (lines, area) in cols.into_iter().zip(col_areas.iter()) {
        frame.render_widget(Paragraph::new(lines).scroll((help.scroll as u16, 0)), *area);
    }

    let more = if help.max_scroll > 0 { "↑ ↓ to scroll  ·  " } else { "" };
    let note = format!(" {more}Uppercase keys act on all panes, lowercase ones on a pane you pick.");
    frame.render_widget(Paragraph::new(note).style(super::dim()), footer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_section_first_and_columns_follow_width() {
        let history = Mode::Scrollback(crate::app::Scrollback::for_test());
        assert_eq!(sections(&history)[0].title, "History (b, B)");
        assert_eq!(sections(&Mode::Panes)[0].title, "Panes");
        assert_eq!(sections(&Mode::Panes).len(), 6);

        assert_eq!(columns(&Mode::Panes, 40).0.len(), 1);
        assert!(columns(&Mode::Panes, 200).0.len() >= 3);
        let total = |(cols, _): (Vec<Vec<Line>>, usize)| cols.iter().map(Vec::len).sum::<usize>();
        assert_eq!(total(columns(&Mode::Panes, 40)), total(columns(&Mode::Panes, 200)), "same content");
    }
}
