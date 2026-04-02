use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

pub struct TailApp {
    file_path: String,
    lines: Vec<String>,
    should_quit: bool,
}

impl TailApp {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            lines: Vec::new(),
            should_quit: false,
        }
    }

    pub fn run(&mut self) -> Result<()> {
        // Setup terminal
        if let Err(e) = enable_raw_mode() {
            eprintln!("Failed to enable raw mode: {}", e);
            return Ok(());
        }

        let mut stdout = std::io::stdout();
        if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            eprintln!("Failed to enter alternate screen: {}", e);
            let _ = disable_raw_mode();
            return Ok(());
        }

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(term) => term,
            Err(e) => {
                eprintln!("Failed to create terminal: {}", e);
                let _ = disable_raw_mode();
                return Ok(());
            }
        };

        // Setup file watcher
        let (tx, rx) = mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(move |res| {
            let _ = tx.send(res);
        }, notify::Config::default()) {
            Ok(watcher) => watcher,
            Err(e) => {
                eprintln!("Failed to create file watcher: {}", e);
                // We'll continue without file watching
                let dummy_rx = mpsc::channel().1;
                // Run without file watching
                let result = self.run_ui_loop(&mut terminal, dummy_rx);
                let _ = disable_raw_mode();
                let _ = execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture
                );
                let _ = terminal.show_cursor();
                return result;
            }
        };

        // Try to watch the file, but don't fail if it doesn't work
        let _ = watcher.watch(Path::new(&self.file_path), RecursiveMode::NonRecursive);

        // Read initial content
        let _ = self.read_file_content();

        // Run the app
        let result = self.run_ui_loop(&mut terminal, rx);

        // Restore terminal
        let _ = disable_raw_mode();
        let _ = execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = terminal.show_cursor();

        result
    }

    fn run_ui_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, rx: mpsc::Receiver<Result<notify::Event, notify::Error>>) -> Result<()> {
        let tick_rate = Duration::from_millis(250);
        let mut last_tick = Instant::now();

        while !self.should_quit {
            if let Err(e) = terminal.draw(|f| self.ui(f)) {
                eprintln!("Failed to draw UI: {}", e);
                break;
            }

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if crossterm::event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                            self.should_quit = true;
                        }
                        _ => {}
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                // Check for file changes
                while let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(event) => {
                            if event.kind.is_modify() || event.kind.is_create() {
                                let _ = self.read_file_content();
                            }
                        }
                        Err(e) => {
                            eprintln!("Watch error: {:?}", e);
                        }
                    }
                }

                // Even if watching fails, periodically refresh the content
                if last_tick.elapsed() >= Duration::from_secs(1) {
                    let _ = self.read_file_content();
                    last_tick = Instant::now();
                } else {
                    last_tick = Instant::now();
                }
            }
        }

        Ok(())
    }

    fn read_file_content(&mut self) -> Result<()> {
        // Try to open and read the file
        match File::open(&self.file_path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                self.lines = reader
                    .lines()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_else(|_| vec!["Error reading file".to_string()]);
            }
            Err(_) => {
                self.lines = vec!["File not found".to_string()];
            }
        }

        Ok(())
    }

    fn ui(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(frame.area());

        // Title
        let title = Paragraph::new("File Tail Viewer")
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Ratatui Tail"));
        frame.render_widget(title, chunks[0]);

        // Content
        let items: Vec<ListItem> = self
            .lines
            .iter()
            .skip(self.lines.len().saturating_sub(chunks[1].height as usize * 2))
            .map(|line| {
                ListItem::new(line.clone())
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Content"))
            .style(Style::default().fg(Color::White));
        frame.render_widget(list, chunks[1]);

        // Help
        let help_text = "Press 'q' or 'ESC' to quit | Ctrl+C to quit";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Yellow))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Help"));
        frame.render_widget(help, chunks[2]);
    }
}
