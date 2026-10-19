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

/// Turns sources that stand for several streams into one source per pane:
/// compose, one pane per service; k8s, one per container.
pub async fn expand(sources: Vec<SourceConfig>) -> Result<Vec<SourceConfig>> {
    let mut out = Vec::new();
    for source in sources {
        if let SourceSpec::K8s { .. } = &source.spec {
            out.extend(expand_k8s(source).await?);
            continue;
        }
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

/// One pane per container, whose pods (all of a workload's) are followed
/// together. Containers are mixed only when asked (`--all-containers`).
async fn expand_k8s(source: SourceConfig) -> Result<Vec<SourceConfig>> {
    let SourceSpec::K8s { container, args, .. } = &source.spec else { unreachable!() };
    let target = k8s_target(&source.spec);
    let all_containers = args.iter().any(|a| a.starts_with("--all-containers"));
    if !target.is_workload() && (container.is_some() || all_containers) {
        return Ok(vec![source]);
    }
    let pods = tools::k8s_pods(&target).await?;
    let containers = match (container, all_containers) {
        (None, false) if pods.containers.len() > 1 => pods.containers,
        _ => vec![],
    };
    let with = |container: Option<String>| {
        let mut spec = source.spec.clone();
        if let SourceSpec::K8s { pods: p, container: c, .. } = &mut spec {
            *p = pods.selector.clone();
            if container.is_some() {
                *c = container.clone();
            }
        }
        let name = match (&source.name, &container) {
            (Some(n), Some(c)) => Some(format!("{n}:{c}")),
            _ => source.name.clone(),
        };
        SourceConfig { spec, name, ..source.clone() }
    };
    if containers.is_empty() {
        return Ok(vec![with(None)]);
    }
    Ok(containers.into_iter().map(|c| with(Some(c))).collect())
}

fn k8s_target(spec: &SourceSpec) -> tools::K8sTarget {
    let SourceSpec::K8s { target, namespace, context, container, args, pods } = spec else {
        panic!("not a k8s source: {spec}")
    };
    tools::K8sTarget {
        target: target.clone(),
        namespace: namespace.clone(),
        context: context.clone(),
        container: container.clone(),
        args: args.clone(),
        pods: pods.clone(),
    }
}

/// `tail`: existing lines to read when the source starts.
pub fn build(config: &SourceConfig, tail: usize) -> Box<dyn Source> {
    let command = |argv| Box::new(CommandSource::new(argv).restart(config.restart()));
    match &config.spec {
        SourceSpec::File { path } => Box::new(FileSource::new(path.clone(), tail)),
        SourceSpec::Cmd { command: c } => Box::new(CommandSource::shell(c).restart(config.restart())),
        SourceSpec::Docker { container } => command(tools::docker(container.clone(), tail)),
        SourceSpec::Compose { services, file } => command(tools::compose(file.clone(), services.clone(), tail)),
        spec @ SourceSpec::K8s { .. } => command(tools::k8s(k8s_target(spec), tail)),
        SourceSpec::Journal { unit } => command(tools::journal(unit.clone(), tail)),
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
