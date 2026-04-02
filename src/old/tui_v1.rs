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
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use notify::{RecursiveMode, Watcher};

// Reuse the structures from v1.rs
struct FileBuffer {
    reader: BufReader<File>,
}

struct OffsetTracker {
    offsets: HashMap<String, FileBuffer>,
}

impl OffsetTracker {
    fn new() -> Self {
        Self {
            offsets: HashMap::new(),
        }
    }

    fn add_offset(&mut self, file_path: &str) -> Result<()> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);

        let tracker = FileBuffer {
            reader,
        };

        self.offsets.insert(file_path.to_string(), tracker);
        Ok(())
    }

    fn get_offset(&mut self, file_path: &str) -> Result<&mut FileBuffer> {
        let x = self.offsets.get_mut(file_path).unwrap();
        Ok(x)
    }
}

pub struct TailApp {
    file_paths: Vec<String>,
    file_contents: HashMap<String, Vec<String>>,
    should_quit: bool,
    offset_tracker: OffsetTracker,
}

impl TailApp {
    pub fn new(file_paths: Vec<String>) -> Self {
        let mut file_contents = HashMap::new();
        for path in &file_paths {
            file_contents.insert(path.clone(), Vec::new());
        }

        Self {
            file_paths,
            file_contents,
            should_quit: false,
            offset_tracker: OffsetTracker::new(),
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

        // Initialize the offset tracker for all files
        for file_path in &self.file_paths {
            if let Err(e) = self.offset_tracker.add_offset(file_path) {
                eprintln!("Failed to add offset for {}: {}", file_path, e);
            }
        }

        // Setup file watcher
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
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

        // Watch all files
        for file_path in &self.file_paths {
            let _ = watcher.watch(Path::new(file_path), RecursiveMode::NonRecursive);
        }

        // Read initial content for all files
        self.read_all_files_content()?;

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
                            if event.kind.is_modify() {
                                // Find which file was modified and update only that file
                                if let Some(path) = event.paths.first() {
                                    if let Some(path_str) = path.to_str() {
                                        self.read_file_content(path_str)?;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Watch error: {:?}", e);
                        }
                    }
                }
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    fn read_all_files_content(&mut self) -> Result<()> {
        let file_paths: Vec<String> = self.file_paths.clone();
        for file_path in &file_paths {
            self.read_file_content(file_path)?;
        }
        Ok(())
    }

    fn read_file_content(&mut self, file_path: &str) -> Result<()> {
        if let Some(lines) = self.file_contents.get_mut(file_path) {
            // Reuse the v1.rs approach for reading the file
            read_line_by_line2(file_path, &mut self.offset_tracker, lines)?;
        }
        Ok(())
    }

    fn ui(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Min(1),    // Content area (flexible)
                Constraint::Length(3), // Help
            ])
            .split(frame.area());

        // Title
        let title = Paragraph::new("Multi-File Tail Viewer (v1 integration)")
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Ratatui Tail"));
        frame.render_widget(title, chunks[0]);

        // Content - Split vertically for multiple files
        self.render_content(frame, chunks[1]);

        // Help
        let help_text = "Press 'q' or 'ESC' to quit | Ctrl+C to quit";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Yellow))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Help"));
        frame.render_widget(help, chunks[2]);
    }

    fn render_content(&self, frame: &mut Frame, area: Rect) {
        if self.file_paths.is_empty() {
            let empty_msg = Paragraph::new("No files to display")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty_msg, area);
            return;
        }

        // Create constraints for vertical split - equal height for each file
        let mut constraints = vec![];
        for _ in 0..self.file_paths.len() {
            constraints.push(Constraint::Ratio(1, self.file_paths.len() as u32));
        }

        let file_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Render each file in its own chunk
        for (i, file_path) in self.file_paths.iter().enumerate() {
            if i < file_chunks.len() {
                self.render_file_content(frame, file_chunks[i], file_path);
            }
        }
    }

    fn render_file_content(&self, frame: &mut Frame, area: Rect, file_path: &str) {
        let empty_vec = Vec::new();
        let lines = self.file_contents.get(file_path).unwrap_or(&empty_vec);

        // Create a title for this file
        let file_title = format!(" File: {} ", file_path);

        // Display content
        let items: Vec<ListItem> = lines
            .iter()
            .skip(lines.len().saturating_sub(area.height as usize * 2))
            .map(|line| {
                ListItem::new(line.clone())
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(file_title))
            .style(Style::default().fg(Color::White));
        frame.render_widget(list, area);
    }
}

// Adapted version of read_line_by_line2 from v1.rs that collects lines instead of printing them
fn read_line_by_line2(file_path: &str, offset_tracker: &mut OffsetTracker, lines: &mut Vec<String>) -> Result<()> {
    lines.clear(); // Clear previous content

    // Reopen the file to reset the reader position
    offset_tracker.add_offset(file_path)?;
    let fb = offset_tracker.get_offset(file_path)?;
    let mut buf = String::new();

    loop {
        let num_bytes = fb.reader.read_line(&mut buf)?;
        if num_bytes == 0 {
            break;
        }
        lines.push(buf.clone()); // Store the line instead of printing it
        buf.clear();
    }


    Ok(())
}
