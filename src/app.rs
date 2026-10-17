//! Application state and the event loop. Keys and source events change the
//! state here; `ui` only draws it.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use chrono::Local;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::buffer::{Buffer, Entry, Seq};
use crate::color::{Library, PaneColor, Scheme};
use crate::pattern::Pattern;
use crate::source::{SourceEvent, SourceId, SourceStatus};
use crate::view::{HistoryView, Merged, Pos, Target};

pub struct Pane {
    pub name: String,
    /// `None` until the source reports something.
    pub status: Option<SourceStatus>,
    /// Times the source came back after ending.
    pub restarts: u32,
    pub color: PaneColor,
    pub buffer: Buffer,
    /// While paused, the pane shows lines before this one; the buffer
    /// keeps receiving.
    pub paused: Option<Seq>,
    /// Lines the pane shows (`E`/`e`); the history keeps everything.
    pub filter: Option<Pattern>,
}

impl Pane {
    /// One past the last line the pane shows.
    pub fn view_end(&self) -> Seq {
        self.paused.unwrap_or(self.buffer.end_seq()).min(self.buffer.end_seq())
    }

    fn toggle_pause(&mut self) {
        self.paused = match self.paused {
            Some(_) => None,
            None => Some(self.buffer.end_seq()),
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectPurpose {
    Scrollback,
    Clear,
    Color,
    Pause,
    Filter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Panes,
    /// Popup asking which pane an action applies to.
    Select { purpose: SelectPurpose, selected: usize },
    Scrollback(Scrollback),
    Detail(Detail),
}

/// Full-screen history of one pane (`b`) or of all of them merged (`B`),
/// with a line cursor, a filter and a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrollback {
    pub target: Target,
    /// Selected line; `None` sits on the last line and follows new ones.
    pub cursor: Option<Pos>,
    /// Written by the renderer: first position on screen, and screen height.
    pub top: Pos,
    pub page: usize,
    /// Columns scrolled to the right when lines are not wrapped.
    pub hscroll: usize,
    /// Only lines matching this are shown (`&`).
    pub filter: Option<Pattern>,
    pub search: Option<Search>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub pattern: Pattern,
    pub forward: bool,
    /// Rank of the selected match and number of matches, when last searched.
    pub rank: usize,
    pub total: usize,
}

impl Scrollback {
    fn new(target: Target, filter: Option<Pattern>) -> Self {
        Self { target, cursor: None, top: 0, page: 0, hscroll: 0, filter, search: None }
    }

    pub fn view<'a>(&'a self, panes: &'a [Pane]) -> HistoryView<'a> {
        HistoryView::new(panes, &self.target, self.filter.as_ref())
    }
}

/// One line on its own, JSON pretty-printed; back to `back` on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub back: Scrollback,
    pub pane: usize,
    pub seq: Seq,
    pub scroll: usize,
    /// Written by the renderer.
    pub max_scroll: usize,
    pub page: usize,
}

/// What a line typed in the prompt is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Search { forward: bool },
    /// `&` in a history view.
    ViewFilter,
    /// `E` (all panes) or `e` (one).
    PaneFilter(Option<usize>),
    /// `/` on the panes: highlight everywhere.
    Highlight,
}

/// A one-line text input shown in the key bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
    /// In characters.
    pub cursor: usize,
    pub error: Option<String>,
    /// Index in the history while browsing it with ↑/↓.
    browsing: Option<usize>,
}

impl Prompt {
    fn new(kind: PromptKind, input: String) -> Self {
        let cursor = input.chars().count();
        Self { kind, input, cursor, error: None, browsing: None }
    }

    pub fn label(&self) -> String {
        match self.kind {
            PromptKind::Search { forward: true } => "/".into(),
            PromptKind::Search { forward: false } => "?".into(),
            PromptKind::ViewFilter => "&".into(),
            PromptKind::PaneFilter(None) => "filter all (!x excludes): ".into(),
            PromptKind::PaneFilter(Some(p)) => format!("filter {} (!x excludes): ", p + 1),
            PromptKind::Highlight => "highlight: ".into(),
        }
    }

    fn byte_index(&self) -> usize {
        self.input.char_indices().nth(self.cursor).map_or(self.input.len(), |(i, _)| i)
    }
}

pub struct App {
    pub panes: Vec<Pane>,
    pub mode: Mode,
    pub strip_ansi: bool,
    /// Wrap long lines, or cut them (`w`).
    pub wrap: bool,
    /// Show arrival times in history views (`t`).
    pub times: bool,
    /// Highlighted in all panes (`/` on the panes).
    pub highlight: Option<Pattern>,
    pub prompt: Option<Prompt>,
    /// Patterns typed so far, oldest first.
    history: Vec<String>,
    pub quit: bool,
    /// Shown in the key bar until the next key.
    pub notice: Option<String>,
    /// Text for the terminal clipboard, sent by the event loop.
    pub clipboard: Option<String>,
    /// Arrival rank of the next line, across all sources.
    arrival: u64,
    library: Arc<Library>,
}

/// Columns moved by ←/→ when lines are cut.
const HSCROLL_STEP: usize = 8;

impl App {
    /// One pane per `(name, color)`, in source order.
    pub fn new(panes: Vec<(String, PaneColor)>, library: Arc<Library>, history: usize, strip_ansi: bool) -> Self {
        let panes = panes
            .into_iter()
            .map(|(name, color)| Pane {
                name,
                status: None,
                restarts: 0,
                color,
                buffer: Buffer::new(history),
                paused: None,
                filter: None,
            })
            .collect();
        Self {
            panes,
            mode: Mode::Panes,
            strip_ansi,
            wrap: true,
            times: false,
            highlight: None,
            prompt: None,
            history: Vec::new(),
            quit: false,
            notice: None,
            clipboard: None,
            arrival: 0,
            library,
        }
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

    /// A scheme by name, for views that need a specific one (JSON details).
    pub fn scheme(&self, name: &str) -> Option<Arc<Scheme>> {
        self.library.resolve(&[name.into()]).ok()
    }

    pub fn ingest(&mut self, id: SourceId, event: SourceEvent) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        match event {
            SourceEvent::Line(text) => {
                pane.color.observe(&text, &self.library);
                let seq = pane.buffer.end_seq();
                pane.buffer.push_entry(Entry { text, arrival: self.arrival, at: Local::now() });
                self.arrival += 1;
                // An open merged view follows arrivals as they come.
                let merged = match &mut self.mode {
                    Mode::Scrollback(sb) => Some(&mut sb.target),
                    Mode::Detail(d) => Some(&mut d.back.target),
                    _ => None,
                };
                if let Some(Target::Merged(m)) = merged {
                    m.push(id, seq, &self.panes);
                }
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
        self.notice = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(prompt) = self.prompt.take() {
            self.prompt_key(prompt, key);
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Panes) {
            Mode::Panes => self.panes_key(key),
            Mode::Select { purpose, selected } => self.select_key(purpose, selected, key),
            Mode::Scrollback(sb) => self.scrollback_key(sb, key),
            Mode::Detail(d) => self.detail_key(d, key),
        }
    }

    fn panes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('N') => {
                let now = Local::now();
                self.panes.iter_mut().for_each(|p| p.buffer.clear_view(now, "cleared"));
            }
            KeyCode::Char('n') => self.select(SelectPurpose::Clear),
            KeyCode::Char('b') => self.select(SelectPurpose::Scrollback),
            KeyCode::Char('B') => {
                self.mode = Mode::Scrollback(Scrollback::new(Target::Merged(Merged::build(&self.panes)), None))
            }
            KeyCode::Char('c') => self.select(SelectPurpose::Color),
            KeyCode::Char('C') => {
                // Off if any pane is colored, otherwise on everywhere.
                let on = !self.panes.iter().any(|p| p.color.active().is_some());
                self.panes.iter_mut().filter(|p| p.color.available()).for_each(|p| p.color.enabled = on);
            }
            KeyCode::Char('p') => self.select(SelectPurpose::Pause),
            KeyCode::Char('P') => {
                // Pause everything unless everything is already paused.
                let pause = self.panes.iter().any(|p| p.paused.is_none());
                for p in &mut self.panes {
                    if p.paused.is_none() == pause {
                        p.toggle_pause();
                    }
                }
            }
            KeyCode::Char('e') => self.select(SelectPurpose::Filter),
            KeyCode::Char('E') => {
                // Start from the common filter, if all panes share one.
                let first = self.panes[0].filter.as_ref().map(|f| f.text.clone());
                let shared = self.panes.iter().all(|p| p.filter.as_ref().map(|f| &f.text) == first.as_ref());
                let input = if shared { first.unwrap_or_default() } else { String::new() };
                self.prompt = Some(Prompt::new(PromptKind::PaneFilter(None), input));
            }
            KeyCode::Char('/') => {
                let input = self.highlight.as_ref().map(|h| h.text.clone()).unwrap_or_default();
                self.prompt = Some(Prompt::new(PromptKind::Highlight, input));
            }
            KeyCode::Char('w') => self.wrap = !self.wrap,
            _ => {}
        }
    }

    fn select_key(&mut self, purpose: SelectPurpose, mut selected: usize, key: KeyEvent) {
        let n = self.panes.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return,
            KeyCode::Up | KeyCode::Char('k') => selected = (selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => selected = (selected + 1) % n,
            KeyCode::Enter => return self.apply(purpose, selected),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => {
                return self.apply(purpose, c as usize - '1' as usize);
            }
            _ => {}
        }
        self.mode = Mode::Select { purpose, selected };
    }

    fn scrollback_key(&mut self, mut sb: Scrollback, key: KeyEvent) {
        let view = sb.view(&self.panes);
        let current = view.resolve(sb.cursor);
        let last = view.last_visible();
        // Moves `n` visible lines; reaching the last one follows the end again.
        let step = |n: usize, forward: bool| {
            let mut pos = current?;
            for _ in 0..n {
                match view.next(pos, forward) {
                    Some(p) => pos = p,
                    None => break,
                }
            }
            Some(pos)
        };
        let down = |n| step(n, true).filter(|&p| Some(p) != last);
        let page = sb.page.max(1);
        let selected = current.and_then(|p| view.line(p)).map(|l| (l.pane, l.seq));
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return,
            KeyCode::Up | KeyCode::Char('k') => sb.cursor = step(1, false),
            KeyCode::Down | KeyCode::Char('j') => sb.cursor = down(1),
            KeyCode::PageUp | KeyCode::Char('u') => sb.cursor = step(page, false),
            KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => sb.cursor = down(page),
            KeyCode::Home | KeyCode::Char('g') => sb.cursor = view.first_visible(),
            KeyCode::End | KeyCode::Char('G') => sb.cursor = None,
            KeyCode::Left | KeyCode::Char('h') if !self.wrap => sb.hscroll = sb.hscroll.saturating_sub(HSCROLL_STEP),
            KeyCode::Right | KeyCode::Char('l') if !self.wrap => sb.hscroll += HSCROLL_STEP,
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                sb.hscroll = 0;
            }
            KeyCode::Char('t') => self.times = !self.times,
            KeyCode::Char('/') | KeyCode::Char('?') => {
                let forward = key.code == KeyCode::Char('/');
                self.prompt = Some(Prompt::new(PromptKind::Search { forward }, String::new()));
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if let Some(search) = &sb.search {
                    let forward = search.forward == (key.code == KeyCode::Char('n'));
                    self.find(&mut sb, forward);
                }
            }
            KeyCode::Char('&') => {
                let input = sb.filter.as_ref().map(|f| f.text.clone()).unwrap_or_default();
                self.prompt = Some(Prompt::new(PromptKind::ViewFilter, input));
            }
            KeyCode::Enter => {
                if let Some((pane, seq)) = selected {
                    let back = sb.clone();
                    self.mode = Mode::Detail(Detail { back, pane, seq, scroll: 0, max_scroll: 0, page: 0 });
                    return;
                }
            }
            KeyCode::Char('y') => {
                let text = selected.and_then(|(p, s)| self.panes[p].buffer.get(s)).map(crate::ui::plain_text);
                self.copy(text);
            }
            _ => {}
        }
        self.mode = Mode::Scrollback(sb);
    }

    /// Moves the cursor to the next match of the current search.
    fn find(&mut self, sb: &mut Scrollback, forward: bool) {
        let Some(search) = &sb.search else { return };
        let view = sb.view(&self.panes);
        let from = view.resolve(sb.cursor).unwrap_or(view.end());
        match view.search(from, forward, &search.pattern) {
            Some((pos, wrapped)) => {
                let (rank, total) = view.count(&search.pattern, pos);
                if wrapped {
                    self.notice = Some(format!("search wrapped to the {}", if forward { "top" } else { "bottom" }));
                }
                sb.cursor = Some(pos);
                if let Some(search) = &mut sb.search {
                    (search.rank, search.total) = (rank, total);
                }
            }
            None => self.notice = Some(format!("not found: {}", search.pattern.text)),
        }
    }

    fn detail_key(&mut self, mut d: Detail, key: KeyEvent) {
        let page = d.page.max(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                self.mode = Mode::Scrollback(d.back);
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => d.scroll = (d.scroll + 1).min(d.max_scroll),
            KeyCode::PageUp | KeyCode::Char('u') => d.scroll = d.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => d.scroll = (d.scroll + page).min(d.max_scroll),
            KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => d.scroll = d.max_scroll,
            KeyCode::Char('y') => {
                let text = self.panes[d.pane].buffer.get(d.seq).map(crate::ui::plain_text);
                self.copy(text);
            }
            _ => {}
        }
        self.mode = Mode::Detail(d);
    }

    fn prompt_key(&mut self, mut prompt: Prompt, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        prompt.error = None;
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                if let Err(e) = self.submit(prompt.kind, prompt.input.trim()) {
                    prompt.error = Some(e);
                    self.prompt = Some(prompt);
                }
                return;
            }
            KeyCode::Char('u') if ctrl => {
                prompt.input.clear();
                prompt.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => prompt.cursor = 0,
            KeyCode::Char('e') if ctrl => prompt.cursor = prompt.input.chars().count(),
            KeyCode::Char(c) if !ctrl => {
                let at = prompt.byte_index();
                prompt.input.insert(at, c);
                prompt.cursor += 1;
            }
            KeyCode::Backspace if prompt.cursor > 0 => {
                prompt.cursor -= 1;
                let at = prompt.byte_index();
                prompt.input.remove(at);
            }
            KeyCode::Delete if prompt.cursor < prompt.input.chars().count() => {
                let at = prompt.byte_index();
                prompt.input.remove(at);
            }
            KeyCode::Left => prompt.cursor = prompt.cursor.saturating_sub(1),
            KeyCode::Right => prompt.cursor = (prompt.cursor + 1).min(prompt.input.chars().count()),
            KeyCode::Home => prompt.cursor = 0,
            KeyCode::End => prompt.cursor = prompt.input.chars().count(),
            KeyCode::Up | KeyCode::Down if !self.history.is_empty() => {
                let len = self.history.len();
                let index = match (prompt.browsing, key.code) {
                    (None, KeyCode::Up) => Some(len - 1),
                    (None, _) => None,
                    (Some(i), KeyCode::Up) => Some(i.saturating_sub(1)),
                    (Some(i), _) if i + 1 < len => Some(i + 1),
                    (Some(_), _) => None,
                };
                prompt.browsing = index;
                prompt.input = index.map(|i| self.history[i].clone()).unwrap_or_default();
                prompt.cursor = prompt.input.chars().count();
            }
            _ => {}
        }
        self.prompt = Some(prompt);
    }

    /// Applies what was typed; an error keeps the prompt open.
    fn submit(&mut self, kind: PromptKind, input: &str) -> Result<(), String> {
        let negation = matches!(kind, PromptKind::ViewFilter | PromptKind::PaneFilter(_));
        let pattern = if input.is_empty() { None } else { Some(Pattern::parse(input, negation)?) };
        if !input.is_empty() && self.history.last().is_none_or(|h| h != input) {
            self.history.push(input.to_owned());
        }
        match kind {
            PromptKind::Highlight => self.highlight = pattern,
            PromptKind::PaneFilter(target) => {
                for (i, pane) in self.panes.iter_mut().enumerate() {
                    if target.is_none_or(|t| t == i) {
                        pane.filter = pattern.clone();
                    }
                }
            }
            PromptKind::ViewFilter => {
                if let Mode::Scrollback(sb) = &mut self.mode {
                    sb.filter = pattern;
                    sb.search = None;
                }
            }
            PromptKind::Search { forward } => {
                let mut mode = std::mem::replace(&mut self.mode, Mode::Panes);
                if let Mode::Scrollback(sb) = &mut mode {
                    // An empty search repeats the previous pattern, as in less.
                    let pattern = pattern.or_else(|| sb.search.take().map(|s| s.pattern));
                    if let Some(pattern) = pattern {
                        sb.search = Some(Search { pattern, forward, rank: 0, total: 0 });
                        self.find(sb, forward);
                    }
                }
                self.mode = mode;
            }
        }
        Ok(())
    }

    /// Queues `text` for the clipboard; the event loop sends it and
    /// reports how in the notice.
    fn copy(&mut self, text: Option<String>) {
        if text.is_none() {
            self.notice = Some("line no longer in history".into());
        }
        self.clipboard = text;
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
        self.mode = Mode::Panes;
        match purpose {
            SelectPurpose::Clear => self.panes[pane].buffer.clear_view(Local::now(), "cleared"),
            SelectPurpose::Scrollback => {
                let filter = self.panes[pane].filter.clone();
                self.mode = Mode::Scrollback(Scrollback::new(Target::Pane(pane), filter));
            }
            SelectPurpose::Pause => self.panes[pane].toggle_pause(),
            SelectPurpose::Color => {
                let color = &mut self.panes[pane].color;
                color.enabled = !color.enabled && color.available();
            }
            SelectPurpose::Filter => {
                let input = self.panes[pane].filter.as_ref().map(|f| f.text.clone()).unwrap_or_default();
                self.prompt = Some(Prompt::new(PromptKind::PaneFilter(Some(pane)), input));
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
                    if let Some(text) = app.clipboard.take() {
                        app.notice = Some(match copy_to_clipboard(&text) {
                            Ok(how) => format!("copied to clipboard ({how})"),
                            Err(e) => format!("copy failed: {e}"),
                        });
                    }
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

/// System clipboard tools, tried in order when mtr runs locally.
const CLIPBOARD_TOOLS: &[&[&str]] = &[
    &["pbcopy"],
    &["wl-copy"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
];

/// Puts `text` in the clipboard and says how. Locally, through the system's
/// clipboard tool, which works whatever the terminal (iTerm2 ignores OSC 52
/// unless allowed in its settings). Over SSH, or without such a tool, asks
/// the terminal with OSC 52: the only way to reach the user's machine.
fn copy_to_clipboard(text: &str) -> std::io::Result<&'static str> {
    let over_ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    if !over_ssh {
        for tool in CLIPBOARD_TOOLS {
            if pipe_to(tool, text).is_ok() {
                return Ok(tool[0]);
            }
        }
    }
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()?;
    Ok("OSC 52")
}

fn pipe_to(argv: &[&str], text: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().expect("piped").write_all(text.as_bytes())?;
    let status = child.wait()?;
    if status.success() { Ok(()) } else { Err(std::io::Error::other(format!("{} failed", argv[0]))) }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
        assert!(matches!(a.mode, Mode::Scrollback(Scrollback { target: Target::Pane(1), cursor: None, .. })));
    }

    #[test]
    fn single_pane_skips_picker() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        assert!(matches!(a.mode, Mode::Scrollback(Scrollback { target: Target::Pane(0), .. })));
    }

    fn cursor(a: &App) -> Option<Seq> {
        match &a.mode {
            Mode::Scrollback(sb) => sb.cursor,
            m => panic!("not in scrollback: {m:?}"),
        }
    }

    #[test]
    fn scrollback_cursor_navigation() {
        // 5 lines (0..5): following means the cursor sits on line 4.
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        if let Mode::Scrollback(sb) = &mut a.mode {
            sb.page = 2; // as if the renderer had a 2-row screen
        }
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(cursor(&a), Some(3));
        press(&mut a, KeyCode::PageUp);
        assert_eq!(cursor(&a), Some(1));
        press(&mut a, KeyCode::PageUp);
        assert_eq!(cursor(&a), Some(0), "clamped at oldest line");
        press(&mut a, KeyCode::Char('j'));
        assert_eq!(cursor(&a), Some(1));
        press(&mut a, KeyCode::PageDown);
        assert_eq!(cursor(&a), Some(3));
        press(&mut a, KeyCode::Char('j'));
        assert_eq!(cursor(&a), None, "last line: following again");
        press(&mut a, KeyCode::Char('g'));
        assert_eq!(cursor(&a), Some(0));
        press(&mut a, KeyCode::Char('G'));
        assert_eq!(cursor(&a), None);
        press(&mut a, KeyCode::Char('q'));
        assert_eq!(a.mode, Mode::Panes);
    }

    #[test]
    fn detail_opens_on_cursor_line_and_returns_there() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('k'));
        press(&mut a, KeyCode::Enter);
        let Mode::Detail(d) = &mut a.mode else { panic!("{:?}", a.mode) };
        assert_eq!(d.seq, 3);
        d.max_scroll = 1;
        press(&mut a, KeyCode::Char('j'));
        press(&mut a, KeyCode::Char('j'));
        assert!(matches!(&a.mode, Mode::Detail(d) if d.scroll == 1), "scroll clamped");
        press(&mut a, KeyCode::Esc);
        assert_eq!(cursor(&a), Some(3));
    }

    #[test]
    fn copy_sends_plain_text_to_clipboard() {
        let mut a = app(1);
        a.ingest(0, SourceEvent::Line("\x1b[31mred\x1b[0m line".into()));
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('y'));
        assert_eq!(a.clipboard.as_deref(), Some("red line"));
        a.notice = Some("copied".into()); // what the event loop does after sending it
        press(&mut a, KeyCode::Char('k'));
        assert!(a.notice.is_none(), "notice lasts until the next key");
    }

    #[test]
    fn base64_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("été".as_bytes()), "w6l0w6k=");
    }

    #[test]
    fn pause_one_and_all() {
        let mut a = app(2);
        press(&mut a, KeyCode::Char('p'));
        press(&mut a, KeyCode::Char('1'));
        assert_eq!(a.panes[0].paused, Some(5));
        a.ingest(0, SourceEvent::Line("late".into()));
        assert_eq!(a.panes[0].view_end(), 5, "paused pane shows nothing new");
        assert_eq!(a.panes[0].buffer.end_seq(), 6, "but keeps receiving");

        press(&mut a, KeyCode::Char('P'));
        assert!(a.panes.iter().all(|p| p.paused.is_some()), "P pauses the rest");
        assert_eq!(a.panes[0].paused, Some(5), "already paused panes keep their point");
        press(&mut a, KeyCode::Char('P'));
        assert!(a.panes.iter().all(|p| p.paused.is_none()), "all paused: P resumes all");
        assert_eq!(a.panes[0].view_end(), 6);
    }

    #[test]
    fn wrap_toggle_resets_horizontal_scroll() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('w'));
        assert!(!a.wrap);
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Right);
        press(&mut a, KeyCode::Right);
        assert!(matches!(&a.mode, Mode::Scrollback(sb) if sb.hscroll == 2 * HSCROLL_STEP));
        press(&mut a, KeyCode::Char('w'));
        assert!(a.wrap);
        assert!(matches!(&a.mode, Mode::Scrollback(sb) if sb.hscroll == 0));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn line(text: &str) -> SourceEvent {
        SourceEvent::Line(text.into())
    }

    #[test]
    fn prompt_editing_history_and_errors() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "erx");
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Left);
        type_text(&mut a, "r");
        assert_eq!(a.prompt.as_ref().unwrap().input, "err", "typed at the cursor");
        press(&mut a, KeyCode::Enter);
        assert!(a.prompt.is_none());
        assert_eq!(a.highlight.as_ref().unwrap().text, "err");

        press(&mut a, KeyCode::Char('/'));
        press(&mut a, KeyCode::Char('u'));
        a.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut a, "(");
        press(&mut a, KeyCode::Enter);
        assert!(a.prompt.as_ref().unwrap().error.is_some(), "bad regex keeps the prompt open");
        press(&mut a, KeyCode::Up);
        assert_eq!(a.prompt.as_ref().unwrap().input, "err", "history");
        press(&mut a, KeyCode::Esc);
        assert!(a.prompt.is_none());
        assert_eq!(a.highlight.as_ref().unwrap().text, "err", "Esc changes nothing");
    }

    #[test]
    fn pane_filters_all_and_one() {
        let mut a = app(2);
        press(&mut a, KeyCode::Char('E'));
        type_text(&mut a, "!-0");
        press(&mut a, KeyCode::Enter);
        assert!(a.panes.iter().all(|p| p.filter.as_ref().is_some_and(|f| !f.accepts("1-0") && f.accepts("1-1"))));

        press(&mut a, KeyCode::Char('e'));
        press(&mut a, KeyCode::Char('2'));
        assert_eq!(a.prompt.as_ref().unwrap().input, "!-0", "starts from the pane's filter");
        a.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        press(&mut a, KeyCode::Enter);
        assert!(a.panes[0].filter.is_some() && a.panes[1].filter.is_none(), "empty clears one pane");

        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('1'));
        assert!(matches!(&a.mode, Mode::Scrollback(sb) if sb.filter.as_ref().unwrap().text == "!-0"), "inherited");
    }

    #[test]
    fn merged_history_search_and_view_filter() {
        let mut a = App::with_names(&["web", "api"]);
        for (p, t) in [(0, "GET / 200"), (1, "level=info"), (0, "GET /x 500"), (1, "level=error"), (0, "GET / 200")] {
            a.ingest(p, line(t));
        }
        press(&mut a, KeyCode::Char('B'));
        press(&mut a, KeyCode::Char('?'));
        type_text(&mut a, "500|error");
        press(&mut a, KeyCode::Enter);
        let sb = |a: &App| match &a.mode {
            Mode::Scrollback(sb) => sb.clone(),
            m => panic!("{m:?}"),
        };
        assert_eq!(sb(&a).cursor, Some(3), "backward from the end: level=error");
        assert_eq!((sb(&a).search.as_ref().unwrap().rank, sb(&a).search.unwrap().total), (2, 2));
        press(&mut a, KeyCode::Char('n'));
        assert_eq!(sb(&a).cursor, Some(2), "n keeps going backward");
        press(&mut a, KeyCode::Char('n'));
        assert_eq!(sb(&a).cursor, Some(3), "wrapped");
        assert!(a.notice.as_ref().unwrap().contains("wrapped"));
        press(&mut a, KeyCode::Char('N'));
        assert_eq!(sb(&a).cursor, Some(2), "N goes the other way");

        // New lines keep coming into the open merged view.
        a.ingest(1, line("level=warn"));
        assert_eq!(sb(&a).view(&a.panes).end(), 6);

        press(&mut a, KeyCode::Char('&'));
        type_text(&mut a, "level");
        press(&mut a, KeyCode::Enter);
        let view_sb = sb(&a);
        let view = view_sb.view(&a.panes);
        assert_eq!((0..6).filter(|&p| view.visible(p).is_some()).count(), 3);

        press(&mut a, KeyCode::Enter);
        assert!(matches!(&a.mode, Mode::Detail(d) if d.pane == 1), "detail of the merged line's own pane");
        press(&mut a, KeyCode::Char('q'));
        press(&mut a, KeyCode::Char('t'));
        assert!(a.times);
    }

    #[test]
    fn empty_search_repeats_and_missing_is_reported() {
        let mut a = app(1);
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('?'));
        type_text(&mut a, "0-1");
        press(&mut a, KeyCode::Enter);
        assert_eq!(cursor(&a), Some(1));
        press(&mut a, KeyCode::Char('?'));
        press(&mut a, KeyCode::Enter);
        assert_eq!(cursor(&a), Some(1), "only one match: wraps onto itself");
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "nope");
        press(&mut a, KeyCode::Enter);
        assert!(a.notice.as_ref().unwrap().contains("not found"));
    }
}
