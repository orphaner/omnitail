//! Log sources. Each source runs as its own tokio task and pushes
//! [`SourceEvent`]s to the app; the app never knows what kind of source it is.

mod command;
mod file;

use std::fmt;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::spec::SourceSpec;

pub use command::CommandSource;
pub use file::FileSource;

/// Index of a source, in the order given on the command line.
pub type SourceId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// One line of output, without its trailing newline.
    Line(String),
    /// What came before is obsolete (file truncated or rewritten): the pane
    /// starts over, the scrollback keeps it behind a separator.
    Reset(&'static str),
    Status(SourceStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    Running,
    /// The source is alive but has nothing to read yet (e.g. a missing file).
    Waiting(String),
    /// The underlying process ended; `None` when killed by a signal.
    Exited(Option<i32>),
    Error(String),
}

impl fmt::Display for SourceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceStatus::Running => write!(f, "running"),
            SourceStatus::Waiting(why) => write!(f, "waiting: {why}"),
            SourceStatus::Exited(Some(code)) => write!(f, "exited: {code}"),
            SourceStatus::Exited(None) => write!(f, "killed"),
            SourceStatus::Error(e) => write!(f, "error: {e}"),
        }
    }
}

pub type EventTx = mpsc::Sender<(SourceId, SourceEvent)>;

/// Sends events for one source. Bounded channel: a chatty source waits for
/// the UI instead of growing memory without limit.
#[derive(Clone)]
pub struct Emitter {
    id: SourceId,
    tx: EventTx,
}

impl Emitter {
    pub fn new(id: SourceId, tx: EventTx) -> Self {
        Self { id, tx }
    }

    /// Returns `false` once the app has gone away, so the source can stop.
    pub async fn emit(&self, event: SourceEvent) -> bool {
        self.tx.send((self.id, event)).await.is_ok()
    }

    pub async fn line(&self, line: String) -> bool {
        self.emit(SourceEvent::Line(line)).await
    }

    pub async fn status(&self, status: SourceStatus) -> bool {
        self.emit(SourceEvent::Status(status)).await
    }
}

pub trait Source: Send {
    /// Starts producing events. The task ends when the source is exhausted
    /// or when the receiving side is dropped.
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()>;
}

pub struct SourceOptions {
    /// Existing lines to show when the source starts.
    pub initial_lines: usize,
}

pub fn build(spec: &SourceSpec, opts: SourceOptions) -> Box<dyn Source> {
    match spec {
        SourceSpec::File { path } => Box::new(FileSource::new(path.clone(), opts.initial_lines)),
        SourceSpec::Cmd { command } => Box::new(CommandSource::shell(command)),
        // POC: go through the docker CLI; a native client can replace this later.
        SourceSpec::Docker { container } => Box::new(CommandSource::new(vec![
            "docker".into(),
            "logs".into(),
            "--follow".into(),
            "--tail".into(),
            opts.initial_lines.to_string(),
            container.clone(),
        ])),
    }
}

/// Turns raw bytes of one line into display text: lossy UTF-8, no line
/// terminator, tabs expanded (they have no width in the terminal grid).
pub(crate) fn decode_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\n', '\r']);
    if !text.contains('\t') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut col = 0;
    for c in text.chars() {
        if c == '\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::decode_line;

    #[test]
    fn decode_strips_terminator_and_expands_tabs() {
        assert_eq!(decode_line(b"abc\r\n"), "abc");
        assert_eq!(decode_line(b"a\tb"), "a       b");
        assert_eq!(decode_line(b"\xffok"), "\u{fffd}ok");
    }
}
