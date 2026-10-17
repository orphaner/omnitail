//! Log sources. Each source runs as its own tokio task and pushes
//! [`SourceEvent`]s to the app; the app never knows what kind of source it is.

mod command;
mod file;
mod stdin;
mod tools;

use std::fmt;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use anyhow::Result;

use crate::spec::{SourceConfig, SourceSpec};

pub use command::CommandSource;
pub use file::FileSource;
pub use stdin::StdinSource;

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
    /// The stream is over (end of stdin).
    Ended,
    Error(String),
}

impl fmt::Display for SourceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceStatus::Running => write!(f, "running"),
            SourceStatus::Waiting(why) => write!(f, "waiting: {why}"),
            SourceStatus::Exited(Some(code)) => write!(f, "exited: {code}"),
            SourceStatus::Exited(None) => write!(f, "killed"),
            SourceStatus::Ended => write!(f, "ended"),
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

/// Turns sources that stand for several streams into one source per pane.
/// Today that is compose: one pane per service.
pub async fn expand(sources: Vec<SourceConfig>) -> Result<Vec<SourceConfig>> {
    let mut out = Vec::new();
    for source in sources {
        let SourceSpec::Compose { services, file } = &source.spec else {
            out.push(source);
            continue;
        };
        let services = match services.len() {
            1 => {
                out.push(source);
                continue;
            }
            0 => tools::compose_services(file.as_deref()).await?,
            _ => services.clone(),
        };
        for service in services {
            out.push(SourceConfig {
                spec: SourceSpec::Compose { services: vec![service.clone()], file: file.clone() },
                name: source.name.as_ref().map(|n| format!("{n}:{service}")),
                ..source.clone()
            });
        }
    }
    Ok(out)
}

/// `lines`: existing lines to show when the source starts.
pub fn build(config: &SourceConfig, lines: usize) -> Box<dyn Source> {
    let command = |argv| Box::new(CommandSource::new(argv).restart(config.restart()));
    match &config.spec {
        SourceSpec::File { path } => Box::new(FileSource::new(path.clone(), lines)),
        SourceSpec::Cmd { command: c } => Box::new(CommandSource::shell(c).restart(config.restart())),
        SourceSpec::Docker { container } => command(tools::docker(container.clone(), lines)),
        SourceSpec::Compose { services, file } => command(tools::compose(file.clone(), services.clone(), lines)),
        SourceSpec::K8s { target, namespace, context, container, args } => command(tools::k8s(
            tools::K8sTarget {
                target: target.clone(),
                namespace: namespace.clone(),
                context: context.clone(),
                container: container.clone(),
                args: args.clone(),
            },
            lines,
        )),
        SourceSpec::Journal { unit } => command(tools::journal(unit.clone(), lines)),
        SourceSpec::Stdin => Box::new(StdinSource),
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
