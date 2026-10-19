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

use std::collections::HashMap;

use crate::buffer::{Buffer, Entry, Origin, Seq};
use crate::spec::K8sPod;
use crate::color::{Choice, FALLBACK, Library, PaneColor, Scheme};
use crate::level::Level;
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
    /// Lowest level shown (`L`/`l`); lines with no known level always are.
    pub level: Option<Level>,
    /// The source feeding the pane, and which of its lines it gets.
    pub feed: SourceId,
    pub scope: Scope,
    /// What a line's badges say in this pane.
    pub badges: Badges,
    /// Format detection per container, for panes mixing them.
    pub by_container: HashMap<Arc<str>, PaneColor>,
}

/// The lines of a source a pane gets: all of them, a pod's, or a container's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Pod(Arc<str>),
    Container(Arc<str>, Arc<str>),
}

impl Scope {
    fn accepts(&self, origin: Option<&Origin>) -> bool {
        match (self, origin) {
            (Scope::All, _) => true,
            (Scope::Pod(pod), Some(o)) => o.pod == *pod,
            (Scope::Container(pod, c), Some(o)) => o.pod == *pod && o.container == *c,
            _ => false,
        }
    }
}

/// Which badges lines get in a pane: their pod's when it shows several,
/// their container's (padded to `width`) when it mixes them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Badges {
    pub pod: bool,
    pub container: Option<usize>,
}

/// What the app knows of a source: its pods, when it has some.
pub struct Feed {
    pub name: String,
    /// Lines carry their pod and container (`kubectl logs --prefix`).
    pub pods_prefixed: bool,
    /// Pods found at start, with their containers.
    pub pods: Vec<K8sPod>,
}

impl Pane {
    fn new(name: String, color: PaneColor, history: usize, feed: SourceId) -> Self {
        Self {
            name,
            status: None,
            restarts: 0,
            color,
            buffer: Buffer::new(history),
            paused: None,
            filter: None,
            level: None,
            feed,
            scope: Scope::All,
            badges: Badges::default(),
            by_container: HashMap::new(),
        }
    }

    /// The scheme for `entry`: the pane's own, or when it mixes containers
    /// and guesses formats, the one guessed for the entry's container.
    pub fn scheme_for(&self, entry: &Entry) -> Option<&Scheme> {
        let guessed = entry.origin.as_ref().filter(|_| self.color.choice == "auto").and_then(|o| self.by_container.get(&o.container));
        match guessed {
            Some(color) if self.color.enabled => color.scheme.as_deref(),
            _ => self.color.active(),
        }
    }

    /// Feeds a line to format detection.
    fn observe(&mut self, text: &str, origin: Option<&Origin>, library: &Library) {
        self.color.observe(text, library);
        if let (Some(o), Some(_)) = (origin, self.badges.container) {
            if !self.by_container.contains_key(&o.container)
                && let Ok(color) = PaneColor::new(Choice::Auto, library, true)
            {
                self.by_container.insert(o.container.clone(), color);
            }
            if let Some(color) = self.by_container.get_mut(&o.container) {
                color.observe(text, library);
            }
        }
    }

    /// Stores a line that arrived, its level read or inherited.
    fn receive(&mut self, mut entry: Entry, stated: Option<Level>, library: &Library) -> Seq {
        self.observe(&entry.text, entry.origin.as_ref(), library);
        let seq = self.buffer.end_seq();
        // A line without a level (a stack trace, a wrapped message)
        // belongs with the line before it.
        entry.level = stated.or_else(|| seq.checked_sub(1).and_then(|s| self.buffer.entry(s)).and_then(|e| e.level));
        self.buffer.push_entry(entry);
        seq
    }

    /// One past the last line the pane shows.
    pub fn view_end(&self) -> Seq {
        self.paused.unwrap_or(self.buffer.end_seq()).min(self.buffer.end_seq())
    }

    /// Whether the pane shows `entry`, given its filter and level.
    pub fn shows(&self, entry: &Entry) -> bool {
        Level::passes(entry.level, self.level) && self.filter.as_ref().is_none_or(|f| f.accepts_entry(entry))
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
    Split,
    Clear,
    Color,
    Pause,
    Filter,
    Scheme,
    Level,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Panes,
    /// Popup asking which pane an action applies to.
    Select { purpose: SelectPurpose, selected: usize },
    Scrollback(Scrollback),
    Detail(Detail),
    /// Popup choosing a pane's color scheme.
    Schemes(SchemePicker),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemePicker {
    pub pane: usize,
    /// Typed text narrowing the list.
    pub query: String,
    /// Index in the filtered list.
    pub selected: usize,
    /// The history view it was opened from, to go back to.
    pub back: Option<Scrollback>,
}

/// One line of the scheme popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeEntry {
    pub name: String,
    pub origin: &'static str,
    pub description: String,
}

/// Lines of history fed to detection when a pane goes back to `auto`.
const REDETECT_LINES: u64 = 20;

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
    /// While paused (`p`), the view ends at this position.
    pub paused: Option<Pos>,
    /// Lowest level shown (`l`).
    pub level: Option<Level>,
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
    fn new(target: Target, filter: Option<Pattern>, level: Option<Level>) -> Self {
        Self { target, cursor: None, top: 0, page: 0, hscroll: 0, filter, search: None, paused: None, level }
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(Target::Pane(0), None, None)
    }

    pub fn view<'a>(&'a self, panes: &'a [Pane]) -> HistoryView<'a> {
        HistoryView::new(panes, &self.target, self.filter.as_ref()).until(self.paused).level(self.level)
    }

    /// Lines that arrived since the view was paused.
    pub fn arrived_since_pause(&self, panes: &[Pane]) -> Option<u64> {
        let end = HistoryView::new(panes, &self.target, None).end();
        self.paused.map(|p| end.saturating_sub(p))
    }
}

/// One line on its own, JSON pretty-printed; back to `back` on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub back: Scrollback,
    /// Position of the line in `back`'s view, for ←/→.
    pub pos: Pos,
    pub pane: usize,
    pub seq: Seq,
    pub scroll: usize,
    /// Written by the renderer.
    pub max_scroll: usize,
    pub page: usize,
}

/// The key reference screen (`h`), over whatever view is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Help {
    /// Every view's keys, not just the current one's (`Tab`).
    pub all: bool,
    pub scroll: usize,
    /// Written by the renderer.
    pub max_scroll: usize,
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
    pub help: Option<Help>,
    /// Patterns typed so far, oldest first.
    history: Vec<String>,
    pub quit: bool,
    /// A single source: its history is the whole app, `q` there quits.
    pub solo: bool,
    /// Shown in the key bar until the next key.
    pub notice: Option<String>,
    /// Text for the terminal clipboard, sent by the event loop.
    pub clipboard: Option<String>,
    /// Arrival rank of the next line, across all sources.
    arrival: u64,
    /// One per source, by `SourceId`.
    pub feeds: Vec<Feed>,
    /// Screens left by splitting a pane (`Enter`), restored by `Backspace`.
    /// Their panes keep receiving lines.
    pub stack: Vec<Vec<Pane>>,
    /// Depth of `stack` on the first screen, where `Esc` goes back to.
    home_depth: usize,
    /// Badge colors given so far, by pod and by container name: each keeps
    /// its color everywhere.
    pod_colors: HashMap<Arc<str>, usize>,
    container_colors: HashMap<Arc<str>, usize>,
    library: Arc<Library>,
}

/// Columns moved by ←/→ when lines are cut.
const HSCROLL_STEP: usize = 8;

impl App {
    /// One pane per `(name, color)`, in source order.
    pub fn new(panes: Vec<(String, PaneColor)>, library: Arc<Library>, history: usize, strip_ansi: bool) -> Self {
        let feeds = panes.iter().map(|(name, _)| Feed { name: name.clone(), pods_prefixed: false, pods: vec![] }).collect();
        let panes = panes.into_iter().enumerate().map(|(i, (name, color))| Pane::new(name, color, history, i)).collect();
        Self {
            panes,
            feeds,
            stack: Vec::new(),
            home_depth: 0,
            mode: Mode::Panes,
            strip_ansi,
            wrap: true,
            times: false,
            highlight: None,
            prompt: None,
            help: None,
            history: Vec::new(),
            quit: false,
            solo: false,
            notice: None,
            clipboard: None,
            arrival: 0,
            pod_colors: HashMap::new(),
            container_colors: HashMap::new(),
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

    /// With a single source, opens straight on its history, following the end.
    fn start_solo(&mut self) {
        if let [pane] = &self.panes[..] {
            self.solo = true;
            self.mode = Mode::Scrollback(Scrollback::new(Target::Pane(0), pane.filter.clone(), pane.level));
        }
    }

    /// A scheme by name, for views that need a specific one (JSON details).
    pub fn scheme(&self, name: &str) -> Option<Arc<Scheme>> {
        self.library.resolve(&[name.into()]).ok()
    }

    /// Declares the pods a source runs: its lines carry their pod and
    /// container, and its panes can be split into them (`Enter`).
    pub fn set_pods(&mut self, feed: SourceId, pods: Vec<K8sPod>) {
        self.feeds[feed].pods_prefixed = true;
        self.feeds[feed].pods = pods;
        let badges = self.badges(feed, &Scope::All);
        self.panes.iter_mut().filter(|p| p.feed == feed).for_each(|p| p.badges = badges);
    }

    pub fn ingest(&mut self, id: SourceId, event: SourceEvent) {
        let Some(feed) = self.feeds.get(id) else { return };
        match event {
            SourceEvent::Line(mut text) => {
                let mut origin = None;
                if feed.pods_prefixed
                    && let Some((pod, container, rest)) = split_pod_prefix(&text)
                {
                    // `--all-containers` includes init containers, which are
                    // left out (native sidecars are listed with the others).
                    let listed = feed.pods.iter().find(|p| p.name == pod);
                    if listed.is_some_and(|p| !p.containers.iter().any(|c| c == container)) {
                        return;
                    }
                    origin = Some(self.origin(pod, container));
                    text = rest.to_owned();
                }
                let stated = if text.contains('\x1b') {
                    crate::level::detect(&crate::ui::plain_text(&text))
                } else {
                    crate::level::detect(&text)
                };
                let entry = Entry { text, arrival: self.arrival, at: Local::now(), level: None, origin };
                self.arrival += 1;
                // Panes of the screens left by a split keep up too.
                for pane in self.stack.iter_mut().flatten() {
                    if pane.feed == id && pane.scope.accepts(entry.origin.as_ref()) {
                        pane.receive(entry.clone(), stated, &self.library);
                    }
                }
                let mut received = Vec::new();
                for (i, pane) in self.panes.iter_mut().enumerate() {
                    if pane.feed == id && pane.scope.accepts(entry.origin.as_ref()) {
                        received.push((i, pane.receive(entry.clone(), stated, &self.library)));
                    }
                }
                // An open merged view follows arrivals as they come.
                let merged = match &mut self.mode {
                    Mode::Scrollback(sb) => Some(&mut sb.target),
                    Mode::Detail(d) => Some(&mut d.back.target),
                    _ => None,
                };
                if let Some(Target::Merged(m)) = merged {
                    for (pane, seq) in received {
                        m.push(pane, seq, &self.panes);
                    }
                }
            }
            SourceEvent::Reset(reason) => {
                tracing::info!(source = %feed.name, reason, "reset");
                let now = Local::now();
                for pane in self.panes.iter_mut().chain(self.stack.iter_mut().flatten()).filter(|p| p.feed == id) {
                    pane.buffer.clear_view(now, reason);
                }
            }
            SourceEvent::Status(status) => {
                tracing::info!(source = %feed.name, %status);
                for pane in self.panes.iter_mut().chain(self.stack.iter_mut().flatten()).filter(|p| p.feed == id) {
                    let was_down = matches!(pane.status, Some(SourceStatus::Exited(_) | SourceStatus::Error(_)));
                    if was_down && status == SourceStatus::Running {
                        pane.restarts += 1;
                    }
                    pane.status = Some(status.clone());
                }
            }
        }
    }

    /// The origin of a line from `pod`'s `container`, with the colors of
    /// their badges.
    fn origin(&mut self, pod: &str, container: &str) -> Origin {
        fn color(colors: &mut HashMap<Arc<str>, usize>, name: &str) -> (Arc<str>, usize) {
            if let Some((name, &color)) = colors.get_key_value(name) {
                return (name.clone(), color);
            }
            let (name, color): (Arc<str>, usize) = (name.into(), colors.len());
            colors.insert(name.clone(), color);
            (name, color)
        }
        let (pod_name, pod_color) = color(&mut self.pod_colors, pod);
        let (container, container_color) = color(&mut self.container_colors, container);
        Origin { badge: pod_badge(pod).into(), pod: pod_name, color: pod_color, container, container_color }
    }

    /// The badges lines need in a pane of `feed` showing `scope`.
    fn badges(&self, feed: SourceId, scope: &Scope) -> Badges {
        let f = &self.feeds[feed];
        if !f.pods_prefixed {
            return Badges::default();
        }
        let mut containers: Vec<&str> = Vec::new();
        for pod in f.pods.iter().filter(|p| match scope {
            Scope::Pod(name) => *p.name == **name,
            _ => true,
        }) {
            for c in &pod.containers {
                if !containers.contains(&c.as_str()) {
                    containers.push(c);
                }
            }
        }
        let mixed = !matches!(scope, Scope::Container(..)) && containers.len() != 1;
        Badges {
            pod: *scope == Scope::All && f.pods.len() != 1,
            // Unknown containers (none found at start) get a default width.
            container: mixed.then(|| containers.iter().map(|c| c.chars().count()).max().unwrap_or(8)),
        }
    }

    /// The panes `pane` splits into: its pods, or a pod's containers. A
    /// level with a single entry is skipped (one pod: its containers).
    fn children(&self, pane: &Pane) -> Vec<Pane> {
        let feed = &self.feeds[pane.feed];
        if !feed.pods_prefixed {
            return Vec::new();
        }
        let mut scope = pane.scope.clone();
        loop {
            let mut next: Vec<Scope> = Vec::new();
            let mut add = |s: Scope| {
                if !next.contains(&s) {
                    next.push(s);
                }
            };
            let seen = pane.buffer.entries().filter_map(|(_, e)| e.origin.as_ref());
            match &scope {
                Scope::All => {
                    feed.pods.iter().for_each(|p| add(Scope::Pod(p.name.as_str().into())));
                    seen.for_each(|o| add(Scope::Pod(o.pod.clone())));
                }
                Scope::Pod(pod) => {
                    let listed = feed.pods.iter().filter(|p| *p.name == **pod).flat_map(|p| &p.containers);
                    listed.for_each(|c| add(Scope::Container(pod.clone(), c.as_str().into())));
                    seen.filter(|o| o.pod == *pod).for_each(|o| add(Scope::Container(pod.clone(), o.container.clone())));
                }
                Scope::Container(..) => {}
            }
            match &next[..] {
                [only] => scope = only.clone(),
                _ => return next.into_iter().map(|s| self.child(pane, s)).collect(),
            }
        }
    }

    /// A pane showing `scope` of `parent`'s lines, starting with those it
    /// already has, and with its filter, level and scheme.
    fn child(&self, parent: &Pane, scope: Scope) -> Pane {
        let choice = match parent.color.choice.as_str() {
            "auto" => Choice::Auto,
            "none" => Choice::Off,
            name => Choice::Named(vec![name.into(), FALLBACK.into()]),
        };
        let color = PaneColor::new(choice, &self.library, parent.color.enabled)
            .or_else(|_| PaneColor::new(Choice::Off, &self.library, false))
            .expect("no coloring cannot fail");
        let feed = &self.feeds[parent.feed];
        let name = match &scope {
            Scope::All => feed.name.clone(),
            Scope::Pod(pod) => format!("{} > {pod}", feed.name),
            Scope::Container(pod, c) => format!("{} > {pod} > {c}", feed.name),
        };
        let mut pane = Pane::new(name, color, parent.buffer.capacity(), parent.feed);
        pane.status = parent.status.clone();
        pane.restarts = parent.restarts;
        pane.filter = parent.filter.clone();
        pane.level = parent.level;
        pane.badges = self.badges(parent.feed, &scope);
        pane.scope = scope;
        let scope = pane.scope.clone();
        for (_, entry) in parent.buffer.entries().filter(|(_, e)| scope.accepts(e.origin.as_ref())) {
            pane.observe(&entry.text, entry.origin.as_ref(), &self.library);
            pane.buffer.push_entry(entry.clone());
        }
        pane
    }

    /// Splits pane `index` into its pods or containers (`Enter`).
    fn split(&mut self, index: usize) {
        let children = self.children(&self.panes[index]);
        if children.is_empty() {
            self.notice = Some(format!("pane {}: nothing to split it into", index + 1));
            return;
        }
        self.stack.push(std::mem::replace(&mut self.panes, children));
    }

    /// Back to the screen before the last split (`Backspace`).
    fn unsplit(&mut self) {
        match self.stack.pop() {
            Some(panes) => self.panes = panes,
            None => self.notice = Some("not split: Enter splits a pane into its pods, then containers".into()),
        }
    }

    /// The first screen: a single source with pods opens on them (or on
    /// its containers, for a single pod); any other single source opens
    /// on its history.
    pub fn start(&mut self) {
        if self.panes.len() == 1 && !self.children(&self.panes[0]).is_empty() {
            self.split(0);
        } else {
            self.start_solo();
        }
        self.home_depth = self.stack.len();
    }

    /// Back to the first screen (`Esc`); quits from there.
    fn home(&mut self) {
        if self.stack.len() == self.home_depth {
            self.quit = true;
        } else if self.stack.len() > self.home_depth {
            self.panes = self.stack.swap_remove(self.home_depth);
            self.stack.truncate(self.home_depth);
        } else {
            // Gone up past the first screen: split down to it again.
            while self.stack.len() < self.home_depth && self.panes.len() == 1 {
                let before = self.stack.len();
                self.split(0);
                if self.stack.len() == before {
                    break;
                }
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
        if let Some(help) = self.help.take() {
            self.help_key(help, key);
            return;
        }
        let opens_help = matches!(key.code, KeyCode::Char('h') | KeyCode::F(1));
        // Not over popups: there, h picks a pane or is typed in the filter.
        if opens_help && !matches!(self.mode, Mode::Select { .. } | Mode::Schemes(_)) {
            self.help = Some(Help::default());
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Panes) {
            Mode::Panes => self.panes_key(key),
            Mode::Select { purpose, selected } => self.select_key(purpose, selected, key),
            Mode::Scrollback(sb) => self.scrollback_key(sb, key),
            Mode::Detail(d) => self.detail_key(d, key),
            Mode::Schemes(picker) => self.schemes_key(picker, key),
        }
    }

    /// The scheme popup's choices matching `query` (name or description).
    pub fn scheme_entries(&self, query: &str) -> Vec<SchemeEntry> {
        let special = [("auto", "guess from the lines (default)"), ("none", "no coloring by omnitail")]
            .map(|(name, description)| SchemeEntry { name: name.into(), origin: "", description: description.into() });
        let schemes = self.library.list().map(|(name, info)| SchemeEntry {
            name: name.to_owned(),
            origin: info.origin.label(),
            description: info.description.clone().unwrap_or_default(),
        });
        let query = query.to_lowercase();
        special
            .into_iter()
            .chain(schemes)
            .filter(|e| e.name.to_lowercase().contains(&query) || e.description.to_lowercase().contains(&query))
            .collect()
    }

    fn schemes_key(&mut self, mut picker: SchemePicker, key: KeyEvent) {
        let count = self.scheme_entries(&picker.query).len();
        let last = count.saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                if key.code == KeyCode::Enter
                    && let Some(entry) = self.scheme_entries(&picker.query).get(picker.selected)
                {
                    self.set_scheme(picker.pane, &entry.name);
                }
                if let Some(sb) = picker.back {
                    self.mode = Mode::Scrollback(sb);
                }
                return;
            }
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::PageUp => picker.selected = picker.selected.saturating_sub(10),
            KeyCode::PageDown => picker.selected = (picker.selected + 10).min(last),
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = last,
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.query.push(c);
                picker.selected = 0;
            }
            _ => {}
        }
        self.mode = Mode::Schemes(picker);
    }

    /// Gives `pane` the scheme `name` (or "auto", "none"), chained with
    /// the fallback like detected and path-chosen schemes.
    fn set_scheme(&mut self, pane: usize, name: &str) {
        let choice = match name {
            "auto" => Choice::Auto,
            "none" => Choice::Off,
            FALLBACK => Choice::Named(vec![FALLBACK.into()]),
            name => Choice::Named(vec![name.into(), FALLBACK.into()]),
        };
        match PaneColor::new(choice, &self.library, true) {
            Ok(mut color) => {
                // Detection starts again from what is already there.
                let buffer = &self.panes[pane].buffer;
                let from = buffer.end_seq().saturating_sub(REDETECT_LINES).max(buffer.first_seq());
                for seq in from..buffer.end_seq() {
                    color.observe(buffer.get(seq).unwrap_or_default(), &self.library);
                }
                self.notice = Some(format!("pane {}: {}", pane + 1, color.label));
                self.panes[pane].color = color;
            }
            Err(e) => self.notice = Some(e),
        }
    }

    fn panes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.home(),
            KeyCode::Char('N') => {
                let now = Local::now();
                self.panes.iter_mut().for_each(|p| p.buffer.clear_view(now, "cleared"));
            }
            KeyCode::Char('n') => self.select(SelectPurpose::Clear),
            KeyCode::Char('b') => self.select(SelectPurpose::Scrollback),
            KeyCode::Char('B') => {
                let target = Target::Merged(Merged::build(&self.panes));
                self.mode = Mode::Scrollback(Scrollback::new(target, None, self.shared_level()))
            }
            KeyCode::Char('l') => self.select(SelectPurpose::Level),
            KeyCode::Char('L') => {
                let level = Level::cycle(self.shared_level());
                self.panes.iter_mut().for_each(|p| p.level = level);
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
            KeyCode::Char('s') => self.select(SelectPurpose::Scheme),
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
            KeyCode::Enter => self.select(SelectPurpose::Split),
            KeyCode::Backspace => self.unsplit(),
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
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = self.solo;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => sb.cursor = step(1, false),
            KeyCode::Down | KeyCode::Char('j') => sb.cursor = down(1),
            KeyCode::PageUp | KeyCode::Char('u') => sb.cursor = step(page, false),
            KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => sb.cursor = down(page),
            KeyCode::Home | KeyCode::Char('g') => sb.cursor = view.first_visible(),
            KeyCode::End | KeyCode::Char('G') => sb.cursor = None,
            KeyCode::Left if !self.wrap => sb.hscroll = sb.hscroll.saturating_sub(HSCROLL_STEP),
            KeyCode::Right if !self.wrap => sb.hscroll += HSCROLL_STEP,
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                sb.hscroll = 0;
            }
            KeyCode::Char('t') => self.times = !self.times,
            // Colors of the pane, from its own history only.
            KeyCode::Char('c') => {
                if let Target::Pane(pane) = sb.target {
                    let color = &mut self.panes[pane].color;
                    color.enabled = !color.enabled && color.available();
                }
            }
            KeyCode::Char('s') => {
                if let Target::Pane(pane) = sb.target {
                    return self.open_schemes(pane, Some(sb));
                }
            }
            KeyCode::Char('l') => {
                sb.level = Level::cycle(sb.level);
                sb.search = None;
            }
            KeyCode::Char('p') => {
                sb.paused = match sb.paused {
                    Some(_) => None,
                    None => Some(HistoryView::new(&self.panes, &sb.target, None).end()),
                }
            }
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
                if let (Some((pane, seq)), Some(pos)) = (selected, current) {
                    let back = sb.clone();
                    self.mode = Mode::Detail(Detail { back, pos, pane, seq, scroll: 0, max_scroll: 0, page: 0 });
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
            KeyCode::Left | KeyCode::Right => {
                // Previous/next line of the history view we came from, with
                // its filter, pause and (merged) order.
                let forward = key.code == KeyCode::Right;
                let view = d.back.view(&self.panes);
                match view.next(d.pos, forward).and_then(|p| view.line(p).map(|l| (p, l.pane, l.seq))) {
                    Some((pos, pane, seq)) => {
                        (d.pos, d.pane, d.seq, d.scroll) = (pos, pane, seq, 0);
                        d.back.cursor = Some(pos);
                    }
                    None => self.notice = Some(if forward { "last line" } else { "first line" }.into()),
                }
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

    fn help_key(&mut self, mut help: Help, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::F(1) => return,
            KeyCode::Tab => {
                help.all = !help.all;
                help.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => help.scroll = (help.scroll + 1).min(help.max_scroll),
            KeyCode::Home | KeyCode::Char('g') => help.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => help.scroll = help.max_scroll,
            _ => {}
        }
        self.help = Some(help);
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

    /// The level filter all panes have, if they share one.
    fn shared_level(&self) -> Option<Level> {
        let first = self.panes.first().and_then(|p| p.level);
        first.filter(|_| self.panes.iter().all(|p| p.level == first))
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
            SelectPurpose::Split => self.split(pane),
            SelectPurpose::Scrollback => {
                let p = &self.panes[pane];
                self.mode = Mode::Scrollback(Scrollback::new(Target::Pane(pane), p.filter.clone(), p.level));
            }
            SelectPurpose::Level => self.panes[pane].level = Level::cycle(self.panes[pane].level),
            SelectPurpose::Pause => self.panes[pane].toggle_pause(),
            SelectPurpose::Color => {
                let color = &mut self.panes[pane].color;
                color.enabled = !color.enabled && color.available();
            }
            SelectPurpose::Filter => {
                let input = self.panes[pane].filter.as_ref().map(|f| f.text.clone()).unwrap_or_default();
                self.prompt = Some(Prompt::new(PromptKind::PaneFilter(Some(pane)), input));
            }
            SelectPurpose::Scheme => self.open_schemes(pane, None),
        }
    }

    /// The scheme popup of `pane`, on its current choice.
    fn open_schemes(&mut self, pane: usize, back: Option<Scrollback>) {
        let current = &self.panes[pane].color.choice;
        let selected = self.scheme_entries("").iter().position(|e| &e.name == current).unwrap_or(0);
        self.mode = Mode::Schemes(SchemePicker { pane, query: String::new(), selected, back });
    }
}

/// The pod, the container and the line of `kubectl logs --prefix` output
/// (`[pod/api-7f9c8d6b5-xk2p9/app] message`).
fn split_pod_prefix(line: &str) -> Option<(&str, &str, &str)> {
    let (inner, text) = line.strip_prefix("[pod/")?.split_once(']')?;
    let (pod, container) = inner.split_once('/')?;
    Some((pod, container, text.strip_prefix(' ').unwrap_or(text)))
}

/// A pod's badge: the random suffix of a replica (`api-7f9c8d6b5-xk2p9` →
/// `xk2p9`), otherwise the whole name (`db-0`).
fn pod_badge(pod: &str) -> &str {
    match pod.rsplit_once('-') {
        Some((_, suffix)) if suffix.len() == 5 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()) => suffix,
        _ => pod,
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

/// System clipboard tools, tried in order when omnitail runs locally.
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

/// SIGTERM, SIGHUP (terminal closed) and SIGINT end omnitail like `q` does, so
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
    fn single_source_opens_on_its_history_and_quits_from_it() {
        let mut a = app(2);
        a.start_solo();
        assert!(matches!(a.mode, Mode::Panes) && !a.solo, "several panes: nothing changes");

        let mut a = app(1);
        a.panes[0].level = Some(Level::Warn);
        a.start_solo();
        assert!(matches!(&a.mode, Mode::Scrollback(sb) if sb.target == Target::Pane(0) && sb.cursor.is_none()));
        assert!(matches!(&a.mode, Mode::Scrollback(sb) if sb.level == Some(Level::Warn)), "pane level kept");
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Char('q'));
        assert!(matches!(a.mode, Mode::Scrollback(_)) && !a.quit, "detail goes back to the history");
        press(&mut a, KeyCode::Esc);
        assert!(a.quit);
    }

    #[test]
    fn colors_and_scheme_from_a_pane_history() {
        let mut a = app(1);
        a.start_solo();
        press(&mut a, KeyCode::Char('s'));
        assert!(matches!(&a.mode, Mode::Schemes(p) if p.back.is_some()));
        type_text(&mut a, "logfmt");
        press(&mut a, KeyCode::Enter);
        assert!(matches!(a.mode, Mode::Scrollback(_)), "back to the history");
        assert_eq!(a.panes[0].color.choice, "logfmt");
        press(&mut a, KeyCode::Char('s'));
        press(&mut a, KeyCode::Esc);
        assert!(matches!(a.mode, Mode::Scrollback(_)) && !a.quit, "Esc leaves the popup only");
        let enabled = a.panes[0].color.enabled;
        press(&mut a, KeyCode::Char('c'));
        assert_ne!(a.panes[0].color.enabled, enabled);

        let mut a = app(2);
        press(&mut a, KeyCode::Char('B'));
        press(&mut a, KeyCode::Char('s'));
        assert!(matches!(a.mode, Mode::Scrollback(_)), "not in the merged view");
    }

    fn pods(names: &[&str], containers: &[&str]) -> Vec<K8sPod> {
        let containers: Vec<String> = containers.iter().map(|c| c.to_string()).collect();
        names.iter().map(|n| K8sPod { name: n.to_string(), containers: containers.clone() }).collect()
    }

    /// One k8s source with `pods`, a line from each pod and container.
    fn shop(pods: Vec<K8sPod>) -> App {
        let mut a = App::with_names(&["k8s: shop > deploy/shop"]);
        a.set_pods(0, pods.clone());
        for pod in &pods {
            for c in &pod.containers {
                a.ingest(0, line(&format!("[pod/{}/{c}] {c} of {}", pod.name, pod.name)));
            }
        }
        a
    }

    fn texts(pane: &Pane) -> Vec<String> {
        pane.buffer.entries().map(|(_, e)| e.text.clone()).collect()
    }

    #[test]
    fn pod_prefixes_become_origins() {
        assert_eq!(split_pod_prefix("[pod/api-7f9c8d6b5-xk2p9/app] hello"), Some(("api-7f9c8d6b5-xk2p9", "app", "hello")));
        assert_eq!(split_pod_prefix("[pod/db-0/db] "), Some(("db-0", "db", "")));
        assert_eq!(split_pod_prefix("[INFO] hello"), None);
        assert_eq!((pod_badge("api-7f9c8d6b5-xk2p9"), pod_badge("db-0"), pod_badge("debug")), ("xk2p9", "db-0", "debug"));

        let mut a = App::with_names(&["k8s: shop > deploy/shop", "plain"]);
        a.set_pods(0, pods(&["shop-5d8-aaaaa", "shop-5d8-bbbbb"], &["app", "proxy"]));
        a.ingest(0, line(r#"[pod/shop-5d8-aaaaa/app] {"level":"error","msg":"boom"}"#));
        a.ingest(0, line("[pod/shop-5d8-bbbbb/proxy] GET /"));
        a.ingest(1, line("[pod/x/y] not a k8s source"));
        let entry = |pane: usize, seq| a.panes[pane].buffer.entry(seq).unwrap().clone();
        let (first, second) = (entry(0, 0), entry(0, 1));
        assert_eq!(first.text, r#"{"level":"error","msg":"boom"}"#, "the line itself, for formats and levels");
        assert_eq!(first.level, Some(Level::Error));
        let o = first.origin.as_ref().unwrap();
        assert_eq!((&*o.pod, &*o.badge, &*o.container), ("shop-5d8-aaaaa", "aaaaa", "app"));
        assert_ne!(o.color, second.origin.as_ref().unwrap().color);
        assert_eq!(entry(1, 0).text, "[pod/x/y] not a k8s source");
        assert_eq!(a.panes[0].badges, Badges { pod: true, container: Some(5) });

        a.panes[0].filter = Some(Pattern::parse("bbbbb", true).unwrap());
        assert!(!a.panes[0].shows(&first) && a.panes[0].shows(&second), "e filters on the pod");
        a.panes[0].filter = Some(Pattern::parse("!proxy", true).unwrap());
        assert!(a.panes[0].shows(&first) && !a.panes[0].shows(&second), "and on the container");
    }

    #[test]
    fn split_into_pods_then_containers_and_back() {
        let mut a = shop(pods(&["shop-1-aaaaa", "shop-1-bbbbb", "shop-1-ccccc"], &["app", "proxy", "shipper"]));
        a.start();
        let names: Vec<_> = a.panes.iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            names,
            ["k8s: shop > deploy/shop > shop-1-aaaaa", "k8s: shop > deploy/shop > shop-1-bbbbb", "k8s: shop > deploy/shop > shop-1-ccccc"]
        );
        assert_eq!(texts(&a.panes[1]), ["app of shop-1-bbbbb", "proxy of shop-1-bbbbb", "shipper of shop-1-bbbbb"]);
        assert_eq!(a.panes[1].badges, Badges { pod: false, container: Some(7) });
        assert!(matches!(a.mode, Mode::Panes) && !a.solo);

        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Char('2'));
        let names: Vec<_> = a.panes.iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            names,
            [
                "k8s: shop > deploy/shop > shop-1-bbbbb > app",
                "k8s: shop > deploy/shop > shop-1-bbbbb > proxy",
                "k8s: shop > deploy/shop > shop-1-bbbbb > shipper"
            ]
        );
        assert_eq!(texts(&a.panes[1]), ["proxy of shop-1-bbbbb"]);
        assert_eq!(a.panes[1].badges, Badges::default());

        a.ingest(0, line("[pod/shop-1-bbbbb/migrate] init containers are left out"));
        a.ingest(0, line("[pod/shop-1-bbbbb/proxy] later"));
        a.ingest(0, line("[pod/shop-1-aaaaa/app] elsewhere"));
        assert_eq!(texts(&a.panes[1]), ["proxy of shop-1-bbbbb", "later"]);
        press(&mut a, KeyCode::Backspace);
        assert_eq!(a.panes.len(), 3);
        assert_eq!(texts(&a.panes[1]).last().unwrap(), "later", "hidden panes kept up");
        assert_eq!(texts(&a.panes[0]).last().unwrap(), "elsewhere");
        press(&mut a, KeyCode::Backspace);
        assert_eq!(a.panes.len(), 1);
        assert_eq!(a.panes[0].buffer.len(), 11);
        press(&mut a, KeyCode::Backspace);
        assert!(a.notice.as_ref().unwrap().starts_with("not split"));

        press(&mut a, KeyCode::Esc);
        assert_eq!(a.panes.len(), 3, "Esc: back down to the first screen");
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Char('1'));
        press(&mut a, KeyCode::Esc);
        assert!(a.panes[0].name.ends_with("shop-1-aaaaa") && a.panes.len() == 3 && !a.quit, "Esc: back up to it");
        assert_eq!(a.stack.len(), 1);
        press(&mut a, KeyCode::Esc);
        assert!(a.quit, "Esc quits from the first screen");
    }

    #[test]
    fn single_pods_and_containers_are_skipped() {
        let mut a = shop(pods(&["debug"], &["shell", "tcpdump"]));
        a.start();
        let names: Vec<_> = a.panes.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["k8s: shop > deploy/shop > debug > shell", "k8s: shop > deploy/shop > debug > tcpdump"], "one pod: its containers");

        let mut a = shop(pods(&["api-1-aaaaa", "api-1-bbbbb"], &["api"]));
        assert_eq!(a.panes[0].badges, Badges { pod: true, container: None }, "one container: no container badge");
        a.start();
        assert_eq!(a.panes.len(), 2);
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Char('1'));
        assert!(a.notice.as_ref().unwrap().contains("nothing to split"));

        let mut a = shop(pods(&["db-0"], &["db"]));
        a.start();
        assert!(a.solo && a.stack.is_empty(), "nothing to split: a single source opens on its history");

        let mut a = app(2);
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Enter);
        assert!(a.notice.as_ref().unwrap().contains("nothing to split"), "not a k8s source");
    }

    #[test]
    fn mixed_containers_get_their_own_format() {
        let mut a = App::with_names(&["k8s: shop > deploy/shop"]);
        a.set_pods(0, pods(&["shop-1-aaaaa"], &["app", "proxy"]));
        a.ingest(0, line(r#"[pod/shop-1-aaaaa/app] {"level":"info","msg":"x"}"#));
        a.ingest(0, line(r#"[pod/shop-1-aaaaa/proxy] 10.0.0.1 - - [18/Oct/2026:14:25:25 +0000] "GET / HTTP/1.1" 200 1 "-" "curl""#));
        let pane = &a.panes[0];
        let scheme = |seq| pane.scheme_for(pane.buffer.entry(seq).unwrap()).map(|s| s.name.clone());
        assert_eq!(scheme(0).as_deref(), Some("json+generic"));
        assert_eq!(scheme(1).as_deref(), Some("http+generic"));
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
    fn detail_left_right_walk_the_history_view() {
        let mut a = App::with_names(&["a", "b"]);
        for (p, t) in [(0, "a0"), (1, "b0"), (0, "skip"), (1, "b1")] {
            a.ingest(p, line(t));
        }
        press(&mut a, KeyCode::Char('B'));
        press(&mut a, KeyCode::Char('&'));
        type_text(&mut a, "!skip");
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Enter); // detail of the last line, b1
        let shown = |a: &App| match &a.mode {
            Mode::Detail(d) => a.panes[d.pane].buffer.get(d.seq).unwrap().to_string(),
            m => panic!("{m:?}"),
        };
        assert_eq!(shown(&a), "b1");
        press(&mut a, KeyCode::Left);
        assert_eq!(shown(&a), "b0", "filtered line skipped");
        press(&mut a, KeyCode::Left);
        assert_eq!(shown(&a), "a0", "other pane, arrival order");
        press(&mut a, KeyCode::Left);
        assert_eq!(a.notice.as_deref(), Some("first line"));
        press(&mut a, KeyCode::Right);
        press(&mut a, KeyCode::Char('q'));
        assert_eq!(cursor(&a), Some(1), "back on the last line shown");
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
    fn levels_are_read_and_inherited() {
        let mut a = App::with_names(&["java"]);
        for line in [
            "starting",
            "2026-10-18 10:00:00 INFO ready",
            "2026-10-18 10:00:01 ERROR boom",
            "\tat com.foo.Bar(Bar.java:1)",
            "\x1b[33m2026-10-18 10:00:02 WARN\x1b[0m slow",
        ] {
            a.ingest(0, SourceEvent::Line(line.into()));
        }
        let levels: Vec<_> = a.panes[0].buffer.entries().map(|(_, e)| e.level).collect();
        use Level::*;
        assert_eq!(levels, [None, Some(Info), Some(Error), Some(Error), Some(Warn)], "trace follows its error; ANSI ignored");
    }

    #[test]
    fn level_filter_keys() {
        let mut a = app(2);
        a.ingest(0, SourceEvent::Line("ERROR bad".into()));
        a.ingest(0, SourceEvent::Line("INFO fine".into()));
        press(&mut a, KeyCode::Char('l'));
        assert!(matches!(a.mode, Mode::Select { purpose: SelectPurpose::Level, .. }));
        press(&mut a, KeyCode::Char('1'));
        assert_eq!((a.panes[0].level, a.panes[1].level), (Some(Level::Debug), None));

        press(&mut a, KeyCode::Char('L'));
        assert!(a.panes.iter().all(|p| p.level == Some(Level::Debug)), "not shared: L starts the cycle for all");
        for _ in 0..3 {
            press(&mut a, KeyCode::Char('L'));
        }
        assert!(a.panes.iter().all(|p| p.level == Some(Level::Error)));
        let shown: Vec<_> = a.panes[0].buffer.entries().filter(|(_, e)| a.panes[0].shows(e)).map(|(_, e)| e.text.clone()).collect();
        assert_eq!(&shown[5..], ["ERROR bad"], "info hidden, lines with no level kept: {shown:?}");

        // A pane's history starts with its level, and l changes it for the view only.
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('1'));
        let Mode::Scrollback(sb) = &a.mode else { panic!() };
        assert_eq!(sb.level, Some(Level::Error));
        assert_eq!(sb.view(&a.panes).count(&Pattern::parse("fine", false).unwrap(), 99), (0, 0), "hidden by the level");
        press(&mut a, KeyCode::Char('l'));
        let Mode::Scrollback(sb) = &a.mode else { panic!() };
        assert_eq!(sb.level, None);
        assert_eq!(sb.view(&a.panes).count(&Pattern::parse("fine", false).unwrap(), 99), (1, 1));
        assert_eq!(a.panes[0].level, Some(Level::Error), "the pane keeps its own");

        press(&mut a, KeyCode::Esc);
        press(&mut a, KeyCode::Char('L'));
        assert!(a.panes.iter().all(|p| p.level.is_none()), "after error: all levels again");
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
    fn pause_in_history_views() {
        for key in ['b', 'B'] {
            let mut a = app(1);
            press(&mut a, KeyCode::Char(key));
            press(&mut a, KeyCode::Char('p'));
            a.ingest(0, line("late"));
            let Mode::Scrollback(sb) = &a.mode else { panic!() };
            assert_eq!(sb.view(&a.panes).end(), 5, "{key}: frozen");
            assert_eq!(sb.arrived_since_pause(&a.panes), Some(1));
            press(&mut a, KeyCode::Char('p'));
            let Mode::Scrollback(sb) = &a.mode else { panic!() };
            assert_eq!(sb.view(&a.panes).end(), 6, "{key}: resumed");
        }
    }

    #[test]
    fn help_opens_everywhere_and_esc_closes_it() {
        let mut a = app(2);
        press(&mut a, KeyCode::Char('h'));
        assert!(a.help.is_some());
        press(&mut a, KeyCode::Char('q'));
        assert!(a.help.is_none() && !a.quit, "q closes the help, not omnitail");

        press(&mut a, KeyCode::Char('w'));
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Char('1'));
        press(&mut a, KeyCode::Char('h'));
        assert!(a.help.is_some(), "h is help in history views too, even with cut lines");
        a.help.as_mut().unwrap().max_scroll = 3;
        press(&mut a, KeyCode::Char('j'));
        press(&mut a, KeyCode::Char('G'));
        assert_eq!(a.help.as_ref().unwrap().scroll, 3);
        press(&mut a, KeyCode::Esc);
        assert!(a.help.is_none());
        assert!(matches!(a.mode, Mode::Scrollback(_)), "back where we were");

        press(&mut a, KeyCode::Char('q'));
        press(&mut a, KeyCode::Char('n'));
        press(&mut a, KeyCode::Char('h'));
        assert!(a.help.is_none(), "not over the pane picker");
    }

    #[test]
    fn scheme_popup_filters_and_applies() {
        let mut a = App::with_names(&["a", "b"]);
        a.ingest(1, line(r#"{"level":"info"}"#));
        press(&mut a, KeyCode::Char('s'));
        press(&mut a, KeyCode::Char('2'));
        let Mode::Schemes(p) = &a.mode else { panic!("{:?}", a.mode) };
        assert_eq!(a.scheme_entries("")[p.selected].name, "auto", "starts on the current choice");

        type_text(&mut a, "fail2ban");
        let Mode::Schemes(p) = &a.mode else { panic!("h typed in the filter must not open the help") };
        assert!(a.help.is_none());
        assert_eq!(a.scheme_entries(&p.query).iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["f2b"]);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.mode, Mode::Panes);
        assert_eq!((a.panes[1].color.choice.as_str(), a.panes[1].color.label.as_str()), ("f2b", "f2b"));

        // Back to auto: detection runs on the lines already there.
        press(&mut a, KeyCode::Char('s'));
        press(&mut a, KeyCode::Char('2'));
        type_text(&mut a, "auto");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.panes[1].color.label, "json");

        press(&mut a, KeyCode::Char('s'));
        press(&mut a, KeyCode::Char('1'));
        type_text(&mut a, "none");
        press(&mut a, KeyCode::Enter);
        assert!(a.panes[0].color.active().is_none());
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
