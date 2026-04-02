use crate::app::App;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::{event};
use itertools::Itertools;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::{StatefulWidget, Stylize, Widget};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::{DefaultTerminal, Frame};
use std::rc::Rc;
use tracing::info;

#[derive(Default)]
pub struct Ui {
    // app: App,
    vertical_scroll_state: ScrollbarState,
    vertical_scroll: usize,
}

impl Ui {
    pub fn new(app: App) -> Self {
        info!("hello from ui");
        Self::default()
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        info!("hello from run");
        let terminal = ratatui::init();
        self.start_loop(terminal)?;
        ratatui::restore();
        Ok(())
    }

    fn start_loop(&mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        let tick_rate = std::time::Duration::from_millis(250);
        let mut last_tick = std::time::Instant::now();
        loop {
            terminal.draw(|frame|self.render(frame))?;
            if event::poll(tick_rate)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                        break Ok(());
                    }
                }
            }
            if (last_tick.elapsed() >= tick_rate) {
                last_tick = std::time::Instant::now();
            }
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }


    // Later this will be a struct enriched with some display heuristics
    fn layout(&mut self, area: Rect) -> Rc<[Rect]> {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        layout
    }
}

impl Widget for &mut Ui { // TODO: remove &mut and use a state struct Note: https://ratatui.rs/how-to/state
    fn render(self, area: Rect, buf: &mut Buffer) {

        let chunks = self.layout(area);
        let top = chunks[0];
        let main = chunks[1];
        let bottom = chunks[2];

        Paragraph::new("Top")
            .dark_gray()
            .alignment(Alignment::Center)
            .scroll((self.vertical_scroll as u16, 0))
            .render(top, buf);

        let paragraph = Paragraph::new("Main")
            .dark_gray()
            .alignment(Alignment::Center)
            .render(main, buf);
        self.vertical_scroll_state = self.vertical_scroll_state.content_length(10);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        scrollbar.render(main, buf, &mut self.vertical_scroll_state);


        self.render_status_bar(bottom, buf);
    }
}

impl Ui {
    fn render_status_bar(&mut self, area: Rect, buf: &mut Buffer) {
        let keys = [
            ("H/←", "Left"),
            ("L/→", "Right"),
            ("K/↑", "Up"),
            ("J/↓", "Down"),
            ("D/Del", "Destroy"),
            ("Q/Esc", "Quit"),
        ];
        let spans = keys
            .iter()
            .flat_map(|(key, desc)| {
                let key = Span::styled(format!(" {key} "), Style::new().fg(BLACK).bg(DARK_GRAY));
                let desc = Span::styled(format!(" {desc} "), Style::new().fg(DARK_GRAY).bg(BLACK));
                [key, desc]
            })
            .collect_vec();
        Line::from(spans)
            .centered()
            .style((Color::Indexed(236), Color::Indexed(232)))
            .render(area, buf);
    }
}


const DARK_BLUE: Color = Color::Rgb(16, 24, 48);
const LIGHT_BLUE: Color = Color::Rgb(64, 96, 192);
const LIGHT_YELLOW: Color = Color::Rgb(192, 192, 96);
const LIGHT_GREEN: Color = Color::Rgb(64, 192, 96);
const LIGHT_RED: Color = Color::Rgb(192, 96, 96);
const RED: Color = Color::Rgb(215, 0, 0);
const BLACK: Color = Color::Rgb(8, 8, 8); // not really black, often #080808
const DARK_GRAY: Color = Color::Rgb(68, 68, 68);
const MID_GRAY: Color = Color::Rgb(128, 128, 128);
const LIGHT_GRAY: Color = Color::Rgb(188, 188, 188);
const WHITE: Color = Color::Rgb(238, 238, 238); // not really white, often #eeeeee
