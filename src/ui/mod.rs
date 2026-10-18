//! Drawing only: everything here reads the [`App`] state, the exception
//! being full-screen views writing back what they actually showed (top
//! line, page size, scroll bounds) for the next key press.

mod detail;
mod help;
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

// omnitail's own chrome uses fixed 256-palette indices only, never the 16 named
// colors: terminal themes remap those (white on light blue badges, …).
// Log content keeps the terminal's default colors and its own ANSI styling.
const BAR_BG: Color = Color::Indexed(236); // #303030
const BAR_FG: Color = Color::Indexed(252); // #d0d0d0
const KEY_BG: Color = Color::Indexed(240); // #585858
const WHITE: Color = Color::Indexed(231); // #ffffff
const BADGE_BG: Color = Color::Indexed(24); // #005f87, ~7:1 against white
const ACCENT: Color = Color::Indexed(39); // #00afff, popup border
const POPUP_BG: Color = Color::Indexed(235); // #262626
const OK: Color = Color::Indexed(114); // #87d787
const WARN: Color = Color::Indexed(221); // #ffd75f
const BAD: Color = Color::Indexed(203); // #ff5f5f
const MUTED: Color = Color::Indexed(247); // #9e9e9e
const CURSOR_BG: Color = Color::Indexed(238); // #444444, scrollback cursor line
const BLACK: Color = Color::Indexed(16); // #000000
/// Search and highlight matches.
const HIGHLIGHT: Style = Style::new().fg(BLACK).bg(Color::Indexed(221));

fn split_screen(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area)
}

/// Rows of content each pane gets on a screen of `size`, computed with the
/// same layout as [`draw`], so sources can load just enough history.
pub fn pane_body_heights(size: Rect, panes: usize) -> Vec<usize> {
    let [main, _] = split_screen(size);
    Stacked.areas(main, panes).iter().map(|a| pane::body(*a).height as usize).collect()
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status_bar] = split_screen(frame.area());

    let json = app.scheme("json");
    match &mut app.mode {
        Mode::Scrollback(sb) => {
            let opts = scrollback::ScrollbackOptions { strip: app.strip_ansi, wrap: app.wrap, times: app.times };
            scrollback::render(frame, main, sb, &app.panes, &opts)
        }
        Mode::Detail(d) => detail::render(frame, main, d, &app.panes[d.pane], app.strip_ansi, json.as_deref()),
        Mode::Panes | Mode::Select { .. } | Mode::Schemes(_) => {
            let opts = pane::PaneOptions { strip: app.strip_ansi, wrap: app.wrap, highlight: app.highlight.as_ref() };
            let areas = Stacked.areas(main, app.panes.len());
            for (i, (pane, area)) in app.panes.iter().zip(areas).enumerate() {
                pane::render(frame, area, i, pane, &opts);
            }
        }
    }
    if let Mode::Select { purpose, selected } = app.mode {
        popup::render(frame, main, purpose, selected, &app.panes);
    }
    if let Mode::Schemes(picker) = &app.mode {
        let entries = app.scheme_entries(&picker.query);
        popup::render_schemes(frame, main, picker, &entries, &app.panes[picker.pane].color.choice);
    }
    if let Some(help) = &mut app.help {
        help::render(frame, frame.area(), help, &app.mode);
        return;
    }
    match &app.prompt {
        Some(prompt) => render_prompt(frame, status_bar, prompt),
        None => render_keys(frame, status_bar, app),
    }
}

/// The text input, in place of the key bar.
fn render_prompt(frame: &mut Frame, area: Rect, prompt: &crate::app::Prompt) {
    let mut spans = vec![Span::styled(prompt.label(), Style::new().fg(ACCENT).bold())];
    let before: String = prompt.input.chars().take(prompt.cursor).collect();
    let mut after = prompt.input.chars().skip(prompt.cursor);
    let at = after.next().map_or(" ".to_string(), |c| c.to_string());
    spans.push(Span::styled(before, Style::new().fg(WHITE)));
    spans.push(Span::styled(at, Style::new().fg(BLACK).bg(WHITE)));
    spans.push(Span::styled(after.collect::<String>(), Style::new().fg(WHITE)));
    if let Some(error) = &prompt.error {
        spans.push(Span::styled(format!("   {error}"), Style::new().fg(BAD).bold()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(BAR_BG)), area);
}

/// A raw line without its ANSI styling, for copying or parsing.
pub fn plain_text(raw: &str) -> String {
    if !raw.contains('\x1b') {
        return raw.to_owned();
    }
    match ansi_to_tui::IntoText::into_text(&raw.as_bytes()) {
        Ok(text) => text.lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n"),
        Err(_) => raw.to_owned(),
    }
}

/// One-line header: index, name and status on the left, then on the right
/// the first of `right` (longest to shortest) that fits.
fn render_title(frame: &mut Frame, area: Rect, index: usize, pane: &Pane, right: &[String]) {
    let (status, color) = match &pane.status {
        None => ("starting".to_string(), MUTED),
        Some(s @ SourceStatus::Running) => (s.to_string(), OK),
        Some(s @ SourceStatus::Waiting(_)) => (s.to_string(), WARN),
        Some(s @ (SourceStatus::Exited(Some(0)) | SourceStatus::Ended)) => (s.to_string(), MUTED),
        Some(s) => (s.to_string(), BAD),
    };
    let badge = format!(" {} ", index + 1);
    let status = match pane.restarts {
        0 => format!("● {status}"),
        n => format!("● {status} ↻{n}"),
    };
    let paused = match pane.paused {
        Some(at) => format!(" ⏸ paused +{}", pane.buffer.end_seq().saturating_sub(at)),
        None => String::new(),
    };
    // The name gives way first, so the status always stays visible.
    let name_room =
        (area.width as usize).saturating_sub(badge.len() + status.chars().count() + paused.chars().count() + 2);
    let left = Line::from(vec![
        Span::styled(badge, Style::new().fg(WHITE).bg(BADGE_BG).bold()),
        Span::raw(format!(" {} ", ellipsize(&pane.name, name_room))).bold(),
        Span::styled(status, Style::new().fg(color)),
        Span::styled(paused, Style::new().fg(WARN).bold()),
    ]);
    render_bar(frame, area, left, right);
}

/// A title bar: `left`, then the first of `right` that fits on the right.
fn render_bar(frame: &mut Frame, area: Rect, left: Line, right: &[String]) {
    let style = Style::new().fg(BAR_FG).bg(BAR_BG);
    let left_width = left.width() as u16;
    frame.render_widget(Paragraph::new(left).style(style), area);
    // Right part only in the space left over; dropped rather than overlapping.
    let free = area.width.saturating_sub(left_width + 1);
    let fits = right.iter().map(|r| format!("{r} ")).find(|r| r.chars().count() as u16 <= free);
    if let Some(right) = fits {
        let right_area = Rect { x: area.right() - free, width: free, ..area };
        frame.render_widget(Paragraph::new(right).style(style).alignment(Alignment::Right), right_area);
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

/// "http · " for a colored pane, "http off · " once toggled off, nothing
/// for a pane without coloring.
fn color_label(pane: &Pane) -> String {
    match (pane.color.available(), pane.color.enabled) {
        (false, _) => String::new(),
        (true, true) => format!("{} · ", pane.color.label),
        (true, false) => format!("{} off · ", pane.color.label),
    }
}

fn lines_count(n: usize) -> String {
    if n == 1 { "1 line".into() } else { format!("{n} lines") }
}

/// Key hints for the current mode; uppercase keys act on all panes,
/// lowercase ones on a pane picked in a popup.
fn render_keys(frame: &mut Frame, area: Rect, app: &App) {
    let keys: &[(&str, &str)] = match &app.mode {
        Mode::Panes => &[
            ("q", "Quit"),
            ("h", "Help"),
            ("b B", "History"),
            ("/", "Highlight"),
            ("E e", "Filter"),
            ("N n", "Clear"),
            ("P p", "Pause"),
            ("C c", "Colors"),
            ("s", "Scheme"),
            ("w", "Wrap"),
        ],
        Mode::Schemes(_) => &[("Esc", "Cancel"), ("↑↓", "Choose"), ("Enter", "Apply"), ("abc", "Filter")],
        Mode::Select { .. } => &[("↑↓", "Choose"), ("Enter", "OK"), ("1-9", "Pick"), ("Esc", "Cancel")],
        Mode::Scrollback(_) if !app.wrap => &[
            ("q", "Back"),
            ("h", "Help"),
            ("↑↓", "Line"),
            ("←→", "Scroll"),
            ("/ ?", "Search"),
            ("n N", "Next"),
            ("&", "Filter"),
            ("Enter", "Detail"),
            ("y", "Copy"),
            ("p", "Pause"),
            ("t", "Time"),
            ("w", "Wrap"),
        ],
        Mode::Scrollback(_) => &[
            ("q", "Back"),
            ("h", "Help"),
            ("↑↓", "Line"),
            ("g G", "Top/End"),
            ("/ ?", "Search"),
            ("n N", "Next"),
            ("&", "Filter"),
            ("Enter", "Detail"),
            ("y", "Copy"),
            ("p", "Pause"),
            ("t", "Time"),
            ("w", "Wrap"),
        ],
        Mode::Detail(_) => &[
            ("q", "Back"),
            ("h", "Help"),
            ("←→", "Prev/next line"),
            ("↑↓ jk", "Scroll"),
            ("PgUp PgDn", "Page"),
            ("y", "Copy"),
        ],
    };
    let mut spans: Vec<Span> = keys
        .iter()
        .flat_map(|(key, desc)| {
            [
                Span::styled(format!(" {key} "), Style::new().fg(WHITE).bg(KEY_BG)),
                Span::styled(format!(" {desc}  "), Style::new().fg(BAR_FG)),
            ]
        })
        .collect();
    if let Some(notice) = &app.notice {
        spans.insert(0, Span::styled(format!(" {notice}  "), Style::new().fg(OK).bold()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(BAR_BG)), area);
}

fn dim() -> Style {
    Style::new().fg(MUTED).add_modifier(Modifier::ITALIC)
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
        let mut app = App::with_names(&["file:a.log", "cmd:ping"]);
        app.ingest(0, SourceEvent::Status(SourceStatus::Running));
        for i in 0..20 {
            app.ingest(0, SourceEvent::Line(format!("a{i}")));
        }
        app.ingest(1, SourceEvent::Line("\x1b[31mpong\x1b[0m".into()));
        app.ingest(1, SourceEvent::Status(SourceStatus::Exited(Some(1))));
        app
    }

    #[test]
    fn body_heights_match_rendered_panes() {
        // 12 rows: 11 for panes, so 6 + 5, minus one title row each.
        assert_eq!(pane_body_heights(Rect::new(0, 0, 50, 12), 2), [5, 4]);
    }

    #[test]
    fn ellipsize_cuts_the_middle() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("/very/long/path/x.log", 11), "/very…x.log");
        assert_eq!(ellipsize("abcdef", 3), "abc");
    }

    #[test]
    fn long_name_keeps_status_visible() {
        let mut app = App::with_names(&[&format!("file:/{}/x.log", "deep/".repeat(20))]);
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
        assert!(matches!(app.mode, Mode::Scrollback(crate::app::Scrollback { cursor: Some(0), .. })));
    }

    #[test]
    fn paused_pane_title_and_frozen_view() {
        let mut app = demo();
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('1'));
        app.ingest(0, SourceEvent::Line("late".into()));
        let s = screen(&mut app);
        assert!(s.lines().next().unwrap().contains("⏸ paused +1"), "{s}");
        assert!(!s.contains("late"), "{s}");
    }

    #[test]
    fn detail_pretty_prints_json_after_a_prefix() {
        let mut app = App::with_names(&["api"]);
        app.ingest(0, SourceEvent::Line(r#"[pod/api] {"level":"error","user":{"id":7}}"#.into()));
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        let body: Vec<&str> = s.lines().skip(1).take(8).collect();
        assert_eq!(
            body,
            ["[pod/api]", "{", r#"  "level": "error","#, r#"  "user": {"#, r#"    "id": 7"#, "  }", "}", ""],
            "{s}"
        );
        assert!(s.lines().next().unwrap().contains("line 1 · json"), "{s}");
    }
}
