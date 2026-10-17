//! Application state and the event loop. Keys and source events change the
//! state here; `ui` only draws it.

use anyhow::Result;
use chrono::Local;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use std::sync::Arc;

use crate::buffer::{Buffer, Seq};
use crate::color::{Library, PaneColor};
use crate::source::{SourceEvent, SourceId, SourceStatus};

pub struct Pane {
    pub name: String,
    /// `None` until the source reports something.
    pub status: Option<SourceStatus>,
    /// Times the source came back after ending.
    pub restarts: u32,
    pub color: PaneColor,
    pub buffer: Buffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectPurpose {
    Scrollback,
    Clear,
    Color,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Panes,
    /// Popup asking which pane an action applies to.
    Select { purpose: SelectPurpose, selected: usize },
    Scrollback(Scrollback),
}

/// Full-screen view of one pane's whole history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrollback {
    pub pane: usize,
    /// Line at the top of the screen; `None` follows the end. The renderer
    /// clamps it and switches back to following once the end is visible.
    pub top: Option<Seq>,
    /// Written by the renderer: what was actually on screen last frame.
    pub shown_top: Seq,
    pub page: usize,
}

pub struct App {
    pub panes: Vec<Pane>,
    pub mode: Mode,
    pub strip_ansi: bool,
    pub quit: bool,
    library: Arc<Library>,
}

impl App {
    /// One pane per `(name, color)`, in source order.
    pub fn new(panes: Vec<(String, PaneColor)>, library: Arc<Library>, history: usize, strip_ansi: bool) -> Self {
        let panes = panes
            .into_iter()
            .map(|(name, color)| Pane { name, status: None, restarts: 0, color, buffer: Buffer::new(history) })
            .collect();
        Self { panes, mode: Mode::Panes, strip_ansi, quit: false, library }
    }

    /// Panes with automatic coloring, for tests.
    #[cfg(test)]
    pub fn with_names(names: &[&str]) -> Self {
        let library = Arc::new(Library::new(&Default::default()).unwrap());
        let panes = names
            .iter()
            .map(|n| (n.to_string(), PaneColor::new(crate::color::Choice::Auto, &library, true).unwrap()))
            .collect();
        Self::new(panes, library, 100, false)
    }

    pub fn ingest(&mut self, id: SourceId, event: SourceEvent) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        match event {
            SourceEvent::Line(line) => {
                pane.color.observe(&line, &self.library);
                pane.buffer.push(line);
            }
            SourceEvent::Reset(reason) => {
                tracing::info!(source = %pane.name, reason, "reset");
                pane.buffer.clear_view(Local::now(), reason);
            }
            SourceEvent::Status(status) => {
                tracing::info!(source = %pane.name, %status);
                let was_down = matches!(pane.status, Some(SourceStatus::Exited(_) | SourceStatus::Error(_)));
                if was_down && status == SourceStatus::Running {
                    pane.restarts += 1;
                }
                pane.status = Some(status);
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Panes => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Char('N') => {
                    let now = Local::now();
                    self.panes.iter_mut().for_each(|p| p.buffer.clear_view(now, "cleared"));
                }
                KeyCode::Char('n') => self.select(SelectPurpose::Clear),
                KeyCode::Char('b') => self.select(SelectPurpose::Scrollback),
                KeyCode::Char('c') => self.select(SelectPurpose::Color),
                KeyCode::Char('C') => {
                    // Off if any pane is colored, otherwise on everywhere.
                    let on = !self.panes.iter().any(|p| p.color.active().is_some());
                    self.panes.iter_mut().filter(|p| p.color.available()).for_each(|p| p.color.enabled = on);
                }
                _ => {}
            },
            Mode::Select { purpose, selected } => {
                let n = self.panes.len();
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Panes,
                    KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + n - 1) % n,
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => *selected = (*selected + 1) % n,
                    KeyCode::Enter => {
                        let (purpose, selected) = (*purpose, *selected);
                        self.apply(purpose, selected);
                    }
                    KeyCode::Char(c @ '1'..='9') => {
                        let idx = c as usize - '1' as usize;
                        if idx < n {
                            let purpose = *purpose;
                            self.apply(purpose, idx);
                        }
                    }
                    _ => {}
                }
            }
            Mode::Scrollback(sb) => {
                let first = self.panes[sb.pane].buffer.first_seq();
                let current = sb.top.unwrap_or(sb.shown_top);
                let page = sb.page.max(1) as Seq;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Panes,
                    KeyCode::Up | KeyCode::Char('k') => sb.top = Some(current.saturating_sub(1).max(first)),
                    KeyCode::Down | KeyCode::Char('j') => sb.top = sb.top.map(|t| t + 1),
                    KeyCode::PageUp | KeyCode::Char('u') => sb.top = Some(current.saturating_sub(page).max(first)),
                    KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => {
                        sb.top = sb.top.map(|t| t + page)
                    }
                    KeyCode::Home | KeyCode::Char('g') => sb.top = Some(first),
                    KeyCode::End | KeyCode::Char('G') => sb.top = None,
                    _ => {}
                }
            }
        }
    }

    /// Opens the pane picker, or applies right away when there is no choice.
    fn select(&mut self, purpose: SelectPurpose) {
        if self.panes.len() == 1 {
            self.apply(purpose, 0);
        } else {
            self.mode = Mode::Select { purpose, selected: 0 };
        }
    }

    fn apply(&mut self, purpose: SelectPurpose, pane: usize) {
        match purpose {
            SelectPurpose::Clear => {
                self.panes[pane].buffer.clear_view(Local::now(), "cleared");
                self.mode = Mode::Panes;
            }
            SelectPurpose::Scrollback => {
                self.mode = Mode::Scrollback(Scrollback { pane, top: None, shown_top: 0, page: 0 });
            }
            SelectPurpose::Color => {
                let color = &mut self.panes[pane].color;
                color.enabled = !color.enabled && color.available();
                self.mode = Mode::Panes;
            }
        }
    }
}

/// Upper bound of source events applied between two frames, so a flood
/// cannot starve keyboard input.
const MAX_BATCH: usize = 4096;

pub async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut events: mpsc::Receiver<(SourceId, SourceEvent)>,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut signals = Signals::new()?;
    let mut dirty = true;
    while !app.quit {
        if dirty {
            terminal.draw(|frame| crate::ui::draw(frame, app))?;
            dirty = false;
        }
        tokio::select! {
            input_event = input.next() => match input_event {
                Some(Ok(Event::Key(key))) => {
                    app.handle_key(key);
                    dirty = true;
                }
                Some(Ok(Event::Resize(..))) => dirty = true,
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            () = signals.recv() => break,
            Some((id, event)) = events.recv() => {
                app.ingest(id, event);
                for _ in 0..MAX_BATCH {
                    match events.try_recv() {
                        Ok((id, event)) => app.ingest(id, event),
                        Err(_) => break,
                    }
                }
                dirty = true;
            }
        }
    }
    Ok(())
}

/// SIGTERM, SIGHUP (terminal closed) and SIGINT end mtr like `q` does, so
/// that sources get dropped and their processes stopped. Ctrl+C itself
/// arrives as a key in raw mode.
struct Signals(#[cfg(unix)] [tokio::signal::unix::Signal; 3]);

impl Signals {
    fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self([
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
                signal(SignalKind::interrupt())?,
            ]))
        }
        #[cfg(not(unix))]
        Ok(Self())
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            let [a, b, c] = &mut self.0;
            tokio::select! {
                _ = a.recv() => {}
                _ = b.recv() => {}
                _ = c.recv() => {}
            }
        }
        #[cfg(not(unix))]
        std::future::pending::<()>().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn app(n: usize) -> App {
        let names: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
        let mut app = App::with_names(&names.iter().map(String::as_str).collect::<Vec<_>>());
        for (id, _) in (0..n).enumerate() {
            for l in 0..5 {
                app.ingest(id, SourceEvent::Line(format!("{id}-{l}")));
            }
        }
        app
    }

    #[test]
    fn quit_keys() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut a = app(2);
            press(&mut a, code);
            assert!(a.quit);
        }
        let mut a = app(2);
        press(&mut a, KeyCode::Char('b'));
        a.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.quit, "ctrl-c quits from any mode");
    }

    #[test]
    fn counts_restarts() {
        let mut a = app(1);
        let status = |a: &mut App, s| a.ingest(0, SourceEvent::Status(s));
        status(&mut a, SourceStatus::Running);
        status(&mut a, SourceStatus::Exited(Some(1)));
        status(&mut a, SourceStatus::Running);
        status(&mut a, SourceStatus::Error("x".into()));
        status(&mut a, SourceStatus::Running);
        assert_eq!(a.panes[0].restarts, 2);
    }

    #[test]
    fn color_toggles() {
        let mut a = app(2);
        let enabled = |a: &App| a.panes.iter().map(|p| p.color.enabled).collect::<Vec<_>>();
        assert_eq!(enabled(&a), [true, true]);
        press(&mut a, KeyCode::Char('c'));
        assert!(matches!(a.mode, Mode::Select { purpose: SelectPurpose::Color, .. }));
        press(&mut a, KeyCode::Char('2'));
        assert_eq!(enabled(&a), [true, false]);
        press(&mut a, KeyCode::Char('C'));
        assert_eq!(enabled(&a), [false, false], "some pane colored: C turns all off");
        press(&mut a, KeyCode::Char('C'));
        assert_eq!(enabled(&a), [true, true]);
    }

    #[test]
    fn clear_all() {
        let mut a = app(2);
        press(&mut a, KeyCode::Char('N'));
        assert!(a.panes.iter().all(|p| p.buffer.view_start() == 5));
    }

    #[test]
    fn clear_one_through_picker() {
        let mut a = app(3);
        press(&mut a, KeyCode::Char('n'));
        assert_eq!(a.mode, Mode::Select { purpose: SelectPurpose::Clear, selected: 0 });
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.mode, Mode::Panes);
        let starts: Vec<_> = a.panes.iter().map(|p| p.buffer.view_start()).collect();
        assert_eq!(starts, [0, 5, 0]);
    }

    #[test]
    fn picker_wraps_and_accepts_digits_and_cancel() {
        let mut a = app(3);
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Up);
        assert!(matches!(a.mode, Mode::Select { selected: 2, .. }));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.mode, Mode::Panes);

        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('2'));
        assert!(matches!(a.mode, Mode::Scrollback(Scrollback { pane: 1, top: None, .. })));
    }

    #[test]
    fn single_pane_skips_picker() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        assert!(matches!(a.mode, Mode::Scrollback(Scrollback { pane: 0, .. })));
    }

    #[test]
    fn scrollback_navigation() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        if let Mode::Scrollback(sb) = &mut a.mode {
            sb.shown_top = 3; // as if the renderer showed lines 3..5
            sb.page = 2;
        }
        let top = |a: &App| match &a.mode {
            Mode::Scrollback(sb) => sb.top,
            _ => panic!("left scrollback"),
        };
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(top(&a), Some(2));
        press(&mut a, KeyCode::PageUp);
        assert_eq!(top(&a), Some(0));
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(top(&a), Some(0), "clamped at oldest line");
        press(&mut a, KeyCode::Char('j'));
        assert_eq!(top(&a), Some(1));
        press(&mut a, KeyCode::Char('G'));
        assert_eq!(top(&a), None);
        press(&mut a, KeyCode::Char('q'));
        assert_eq!(a.mode, Mode::Panes);
    }
}
