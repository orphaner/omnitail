//! Drawing only: everything here reads the [`App`] state, the single
//! exception being the scrollback writing back what it actually showed.

mod layout;
mod pane;
mod popup;
mod scrollback;
mod text;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Mode, Pane};
use crate::source::SourceStatus;
use layout::{PaneLayout, Stacked};

const BAR_BG: Color = Color::Indexed(236);
const BAR_FG: Color = Color::Indexed(250);
const KEY_BG: Color = Color::Indexed(240);
// Fixed 256-palette indices rather than named colors: terminal themes
// remap the 16 named ones, which broke the badge contrast.
const BADGE_FG: Color = Color::Indexed(231); // #ffffff
const BADGE_BG: Color = Color::Indexed(24); // #005f87, ~7:1 against white
const ACCENT: Color = Color::Indexed(39); // #00afff, popup border

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status_bar] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    match &mut app.mode {
        Mode::Scrollback(sb) => scrollback::render(frame, main, sb, &app.panes[sb.pane], app.strip_ansi),
        Mode::Panes | Mode::Select { .. } => {
            let areas = Stacked.areas(main, app.panes.len());
            for (i, (pane, area)) in app.panes.iter().zip(areas).enumerate() {
                pane::render(frame, area, i, pane, app.strip_ansi);
            }
        }
    }
    if let Mode::Select { purpose, selected } = app.mode {
        popup::render(frame, main, purpose, selected, &app.panes);
    }
    render_keys(frame, status_bar, &app.mode);
}

/// One-line header: index, name and status on the left, `right` on the right.
fn render_title(frame: &mut Frame, area: Rect, index: usize, pane: &Pane, right: String) {
    let (status, color) = match &pane.status {
        None => ("starting".to_string(), Color::DarkGray),
        Some(s @ SourceStatus::Running) => (s.to_string(), Color::Green),
        Some(s @ SourceStatus::Waiting(_)) => (s.to_string(), Color::Yellow),
        Some(s @ SourceStatus::Exited(Some(0))) => (s.to_string(), Color::DarkGray),
        Some(s) => (s.to_string(), Color::Red),
    };
    let badge = format!(" {} ", index + 1);
    let status = format!("● {status}");
    // The name gives way first, so the status always stays visible.
    let name_room = (area.width as usize).saturating_sub(badge.len() + status.chars().count() + 2);
    let left = Line::from(vec![
        Span::styled(badge, Style::new().fg(BADGE_FG).bg(BADGE_BG).bold()),
        Span::raw(format!(" {} ", ellipsize(&pane.name, name_room))).bold(),
        Span::styled(status, Style::new().fg(color)),
    ]);
    let style = Style::new().fg(BAR_FG).bg(BAR_BG);
    let left_width = left.width() as u16;
    frame.render_widget(Paragraph::new(left).style(style), area);
    // Right part only in the space left over; dropped rather than overlapping.
    let right = format!("{right} ");
    let free = area.width.saturating_sub(left_width + 1);
    if right.chars().count() as u16 <= free {
        let right_area = Rect { x: area.right() - free, width: free, ..area };
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), right_area);
    }
}

/// Shortens `s` to at most `max` columns by cutting out its middle, which
/// keeps both the start of a path and the file name.
fn ellipsize(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    if max < 5 {
        return chars[..max].iter().collect();
    }
    let tail = (max - 1) / 2;
    let head = max - 1 - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

fn lines_count(n: usize) -> String {
    if n == 1 { "1 line".into() } else { format!("{n} lines") }
}

fn render_keys(frame: &mut Frame, area: Rect, mode: &Mode) {
    let keys: &[(&str, &str)] = match mode {
        Mode::Panes => &[("q", "Quit"), ("b", "Scrollback"), ("N", "Clear all"), ("n", "Clear one")],
        Mode::Select { .. } => &[("↑↓", "Choose"), ("Enter", "OK"), ("1-9", "Pick"), ("Esc", "Cancel")],
        Mode::Scrollback(_) => &[
            ("↑↓ jk", "Line"),
            ("PgUp PgDn", "Page"),
            ("g G", "Top/End"),
            ("q", "Back"),
        ],
    };
    let spans: Vec<Span> = keys
        .iter()
        .flat_map(|(key, desc)| {
            [
                Span::styled(format!(" {key} "), Style::new().fg(Color::White).bg(KEY_BG)),
                Span::styled(format!(" {desc}  "), Style::new().fg(BAR_FG)),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(BAR_BG)), area);
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::source::SourceEvent;

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn demo() -> App {
        let mut app = App::new(vec!["file:a.log".into(), "cmd:ping".into()], 100, false);
        app.ingest(0, SourceEvent::Status(SourceStatus::Running));
        for i in 0..20 {
            app.ingest(0, SourceEvent::Line(format!("a{i}")));
        }
        app.ingest(1, SourceEvent::Line("\x1b[31mpong\x1b[0m".into()));
        app.ingest(1, SourceEvent::Status(SourceStatus::Exited(Some(1))));
        app
    }

    #[test]
    fn ellipsize_cuts_the_middle() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("/very/long/path/x.log", 11), "/very…x.log");
        assert_eq!(ellipsize("abcdef", 3), "abc");
    }

    #[test]
    fn long_name_keeps_status_visible() {
        let mut app = App::new(vec![format!("file:/{}/x.log", "deep/".repeat(20))], 100, false);
        app.ingest(0, SourceEvent::Status(SourceStatus::Running));
        let s = screen(&mut app);
        let title = s.lines().next().unwrap();
        assert!(title.contains("…") && title.contains("x.log") && title.contains("● running"), "{title}");
    }

    #[test]
    fn panes_screen() {
        let s = screen(&mut demo());
        println!("{s}");
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with(" 1  file:a.log ● running"), "{s}");
        assert!(lines[0].ends_with("20 lines"));
        // Pane 1 has 5 body rows: the 5 newest lines, newest last.
        assert_eq!(&lines[1..6], ["a15", "a16", "a17", "a18", "a19"]);
        assert!(lines[6].contains("cmd:ping ● exited: 1"));
        assert_eq!(lines[7], "pong");
        assert!(lines[11].contains("Quit"));
    }

    #[test]
    fn clear_then_scrollback_shows_everything() {
        let mut app = demo();
        press(&mut app, KeyCode::Char('N'));
        app.ingest(0, SourceEvent::Line("fresh".into()));
        let s = screen(&mut app);
        assert!(s.contains("(20 hidden)") && s.contains("fresh") && !s.contains("a19"), "{s}");

        press(&mut app, KeyCode::Char('b'));
        let s = screen(&mut app);
        assert!(s.contains(" Scrollback ") && s.contains("2 cmd:ping"), "popup:\n{s}");

        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.lines().next().unwrap().contains("file:a.log ● running"), "title kept readable: {s}");
        assert!(s.contains("a19") && s.contains("cleared at") && s.contains("fresh"), "{s}");

        press(&mut app, KeyCode::Char('g'));
        let s = screen(&mut app);
        assert_eq!(s.lines().nth(1), Some("a0"), "{s}");
        assert!(matches!(app.mode, Mode::Scrollback(crate::app::Scrollback { top: Some(0), .. })));
    }
}
