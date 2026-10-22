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
/// Badge colors of panes in the merged view and of pods (fixed palette, dark text).
const BADGE_COLORS: [u8; 8] = [75, 114, 221, 176, 80, 215, 203, 147];

/// A colored badge on dark text, then a space.
fn badge(text: String, color: usize) -> [Span<'static>; 2] {
    let bg = Color::Indexed(BADGE_COLORS[color % BADGE_COLORS.len()]);
    [Span::styled(text, Style::new().fg(BLACK).bg(bg)), Span::raw(" ")]
}

/// The badges of the pod and container a line comes from, as far as the
/// pane needs them.
fn origin_badges(entry: &crate::buffer::Entry, badges: crate::app::Badges) -> Vec<Span<'static>> {
    let Some(o) = &entry.origin else { return Vec::new() };
    let mut spans = Vec::new();
    if badges.pod {
        spans.extend(badge(format!(" {} ", o.badge), o.color));
    }
    if let Some(width) = badges.container {
        let color = Color::Indexed(BADGE_COLORS[o.container_color % BADGE_COLORS.len()]);
        spans.push(Span::styled(format!("{:<width$} ", o.container), Style::new().fg(color).bold()));
    }
    spans
}

/// The full pod and container names of a line, for the detail view.
fn origin_line(o: &crate::buffer::Origin) -> Line<'static> {
    let mut spans = Vec::from(badge(format!(" {} ", o.pod), o.color));
    let color = Color::Indexed(BADGE_COLORS[o.container_color % BADGE_COLORS.len()]);
    spans.push(Span::styled(o.container.to_string(), Style::new().fg(color).bold()));
    Line::from(spans)
}

/// Rows of `text` behind `prefix` (badges, arrival time): the text gets the
/// remaining width, and its continuation rows are indented under it.
fn prefixed_rows(render: &text::Render, text: &str, prefix: Vec<Span<'static>>) -> Vec<Line<'static>> {
    if prefix.is_empty() {
        return render.rows(text);
    }
    let width: usize = prefix.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
    let render = text::Render { width: render.width.saturating_sub(width).max(1), ..*render };
    render
        .rows(text)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = if i == 0 { prefix.clone() } else { vec![Span::raw(" ".repeat(width))] };
            spans.extend(row.spans);
            Line::from(spans).style(row.style)
        })
        .collect()
}

/// Search and highlight matches.
const HIGHLIGHT: Style = Style::new().fg(BLACK).bg(Color::Indexed(221));
/// What a filter (`e`, `&`) matched: quieter than the search.
const FILTER_MATCH: Style = Style::new().fg(BLACK).bg(Color::Indexed(116));

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
        Mode::Scrollback(sb) | Mode::Schemes(crate::app::SchemePicker { back: Some(sb), .. }) => {
            let opts = scrollback::ScrollbackOptions { strip: app.strip_ansi, wrap: app.wrap, times: app.times };
            scrollback::render(frame, main, sb, &app.panes, &opts)
        }
        Mode::Detail(d) => {
            detail::render(frame, main, d, &app.panes[d.pane], app.strip_ansi, json.as_deref(), app.highlight.as_ref())
        }
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
    if app.confirm_quit {
        popup::render_quit(frame, main);
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
        _ if pane.gone => ("pod deleted".to_string(), MUTED),
        None => ("starting".to_string(), MUTED),
        Some(s @ SourceStatus::Running) => (s.to_string(), OK),
        Some(s @ SourceStatus::Waiting(_)) => (s.to_string(), WARN),
        Some(s @ (SourceStatus::Exited(Some(0)) | SourceStatus::Ended)) => (s.to_string(), MUTED),
        Some(s) => (s.to_string(), BAD),
    };
    let badge = format!(" {} ", index + 1);
    // A source that failed gets a cross; one running while the pane is
    // paused gives way to the pause, which is what the pane shows.
    let arrived = pane.paused.map(|at| pane.buffer.end_seq().saturating_sub(at));
    let restarts = match pane.restarts {
        0 => String::new(),
        n => format!(" ↻{n}"),
    };
    let (status, paused) = match (&pane.status, arrived) {
        (Some(SourceStatus::Running), Some(n)) => (String::new(), format!("⏸ paused +{n}{restarts}")),
        (_, arrived) => {
            let mark = if color == BAD { "✗" } else { "●" };
            (format!("{mark} {status}{restarts}"), arrived.map(|n| format!(" ⏸ +{n}")).unwrap_or_default())
        }
    };
    // The name gives way first, so the status and the shortest right part
    // (counts, position) always stay visible.
    let right_room = right.last().map_or(0, |r| r.chars().count() + 2);
    let name_room = (area.width as usize)
        .saturating_sub(badge.len() + status.chars().count() + paused.chars().count() + 2 + right_room);
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
    // Containers mixed in the pane: the formats guessed for each.
    let mut label = pane.color.label.clone();
    if pane.color.choice == "auto" && !pane.by_container.is_empty() {
        let mut labels: Vec<&str> =
            pane.by_container.values().map(|c| c.label.as_str()).filter(|&l| l != "auto").collect();
        labels.sort_unstable();
        labels.dedup();
        label = labels.join("/");
    }
    match (pane.color.available(), pane.color.enabled) {
        (false, _) => String::new(),
        (true, true) => format!("{label} · "),
        (true, false) => format!("{label} off · "),
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
            ("/", "Search"),
            ("E e", "Filter"),
            ("C c", "Clear"),
            ("P p", "Pause"),
            ("s S", "Scheme/Colors"),
            ("w", "Wrap"),
            ("⏎ Esc", "Open/Back"),
        ],
        Mode::Schemes(_) => &[("Esc", "Cancel"), ("↑↓", "Choose"), ("Enter", "Apply"), ("abc", "Filter")],
        Mode::Select { .. } => &[("↑↓", "Choose"), ("Enter", "OK"), ("1-9", "Pick"), ("Esc", "Cancel")],
        Mode::Scrollback(_) if !app.wrap => &[
            ("q", "Back"),
            ("h", "Help"),
            ("↑↓", "Line"),
            ("←→", "Scroll"),
            ("/", "Search"),
            ("n N", "Next"),
            ("&", "Filter"),
            ("Enter", "Detail"),
            ("C c", "Clear"),
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
            ("/", "Search"),
            ("n N", "Next"),
            ("&", "Filter"),
            ("Enter", "Detail"),
            ("C c", "Clear"),
            ("y", "Copy"),
            ("p", "Pause"),
            ("t", "Time"),
            ("w", "Wrap"),
        ],
        // Scrolling only matters for a line longer than the screen.
        Mode::Detail(d) if d.max_scroll > 0 => &[
            ("q", "Back"),
            ("h", "Help"),
            ("←→", "Prev/next line"),
            ("↑↓ jk", "Scroll"),
            ("PgUp PgDn", "Page"),
            ("/", "Search"),
            ("y", "Copy"),
        ],
        Mode::Detail(_) => &[("q", "Back"), ("h", "Help"), ("←→", "Prev/next line"), ("/", "Search"), ("y", "Copy")],
    };
    // With a single source, its history is the top view: q leaves omnitail.
    let quits = app.solo && matches!(app.mode, Mode::Scrollback(_));
    let mut spans: Vec<Span> = keys
        .iter()
        .map(|&(key, desc)| (key, if quits && key == "q" { "Quit" } else { desc }))
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
    fn history_counts_new_lines_below_a_moved_cursor() {
        let mut app = demo();
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('1'));
        let title = |app: &mut App| screen(app).lines().next().unwrap().to_owned();
        assert!(!title(&mut app).contains("new"), "following the end");
        press(&mut app, KeyCode::Up);
        screen(&mut app);
        for i in 0..3 {
            app.ingest(0, SourceEvent::Line(format!("new{i}")));
        }
        app.ingest(1, SourceEvent::Line("other pane".into()));
        assert!(title(&mut app).contains("↓ +3"), "{}", title(&mut app));
        press(&mut app, KeyCode::Char('G'));
        assert!(!title(&mut app).contains("↓"), "back to following");
    }

    #[test]
    fn search_total_counts_matches_that_arrive_later() {
        let mut app = demo();
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Char('/'));
        for c in "a1".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        let title = |app: &mut App| screen(app).lines().next().unwrap().to_owned();
        assert!(title(&mut app).contains("match 1/11"), "wrapped to the first of a1, a10…a19: {}", title(&mut app));
        app.ingest(0, SourceEvent::Line("a1 again".into()));
        app.ingest(0, SourceEvent::Line("nothing".into()));
        app.ingest(0, SourceEvent::Line("a1 once more".into()));
        let total = |app: &mut App| {
            screen(app);
            match &app.mode {
                Mode::Scrollback(sb) => sb.search.as_ref().map(|s| (s.rank, s.total)),
                _ => None,
            }
        };
        assert_eq!(total(&mut app), Some((1, 13)), "the two new matches counted");
        assert_eq!(total(&mut app), Some((1, 13)), "and only once");
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
        assert!(lines[6].contains("cmd:ping ✗ exited: 1"), "a cross for a failed source");
        assert_eq!(lines[7], "pong");
        assert!(lines[11].contains("Quit"));
    }

    #[test]
    fn clear_then_scrollback_shows_everything() {
        let mut app = demo();
        press(&mut app, KeyCode::Char('C'));
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
        let title = s.lines().next().unwrap();
        assert!(title.contains("file:a.log ⏸ paused +1") && !title.contains("running"), "the pause instead: {s}");
        assert!(!s.contains("late"), "{s}");

        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('2'));
        app.ingest(1, SourceEvent::Line("after".into()));
        let s = screen(&mut app);
        assert!(s.contains("cmd:ping ✗ exited: 1 ⏸ +1"), "a failed source stays visible: {s}");
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

    #[test]
    fn detail_names_the_pod_and_container() {
        let mut app = App::with_names(&["k8s: shop > deploy/shop"]);
        let pods = ["shop-5d8-aaaaa", "shop-5d8-bbbbb"].map(|n| crate::spec::K8sPod { name: n.into(), containers: vec!["app".into()] });
        app.set_pods(0, pods.to_vec());
        app.ingest(0, SourceEvent::Line(r#"[pod/shop-5d8-bbbbb/app] {"msg":"hi"}"#.into()));
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        let body: Vec<&str> = s.lines().skip(1).take(4).collect();
        assert_eq!(body, [" shop-5d8-bbbbb  app", "", "{", r#"  "msg": "hi""#], "{s}");
    }
}
