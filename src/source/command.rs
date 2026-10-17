use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use super::{Emitter, Source, SourceStatus};

/// Runs a process and follows its stdout and stderr, merged into one stream.
pub struct CommandSource {
    argv: Vec<String>,
}

impl CommandSource {
    pub fn new(argv: Vec<String>) -> Self {
        assert!(!argv.is_empty(), "empty argv");
        Self { argv }
    }

    /// Runs `command` through `sh -c`, so quoting and pipes work as in a shell.
    pub fn shell(command: &str) -> Self {
        Self::new(vec!["sh".into(), "-c".into(), command.into()])
    }
}

impl Source for CommandSource {
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut child = match Command::new(&self.argv[0])
                .args(&self.argv[1..])
                // The TUI owns the terminal: children must not read from it.
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Dropping the task (app exit) kills the process.
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    out.status(SourceStatus::Error(format!("{}: {e}", self.argv[0]))).await;
                    return;
                }
            };
            out.status(SourceStatus::Running).await;

            let stdout = tokio::spawn(pump(child.stdout.take().unwrap(), out.clone()));
            let stderr = tokio::spawn(pump(child.stderr.take().unwrap(), out.clone()));
            let _ = tokio::join!(stdout, stderr);

            let status = match child.wait().await {
                Ok(s) => SourceStatus::Exited(s.code()),
                Err(e) => SourceStatus::Error(e.to_string()),
            };
            out.status(status).await;
        })
    }
}

async fn pump(stream: impl AsyncRead + Unpin, out: Emitter) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if !out.line(super::decode_line(&buf)).await {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::source::SourceEvent;

    async fn collect(source: CommandSource) -> Vec<SourceEvent> {
        let (tx, mut rx) = mpsc::channel(16);
        Box::new(source).spawn(Emitter::new(0, tx)).await.unwrap();
        let mut events = Vec::new();
        while let Ok((_, e)) = rx.try_recv() {
            events.push(e);
        }
        events
    }

    #[tokio::test]
    async fn streams_lines_then_exit_status() {
        let events = collect(CommandSource::shell("printf 'a\\nb\\r\\n'; exit 3")).await;
        assert_eq!(
            events,
            vec![
                SourceEvent::Status(SourceStatus::Running),
                SourceEvent::Line("a".into()),
                SourceEvent::Line("b".into()),
                SourceEvent::Status(SourceStatus::Exited(Some(3))),
            ]
        );
    }

    #[tokio::test]
    async fn merges_stderr() {
        let events = collect(CommandSource::shell("echo oops >&2")).await;
        assert!(events.contains(&SourceEvent::Line("oops".into())));
    }

    #[tokio::test]
    async fn reports_spawn_failure() {
        let events = collect(CommandSource::new(vec!["/nonexistent/mtr-test".into()])).await;
        assert!(matches!(events[..], [SourceEvent::Status(SourceStatus::Error(_))]));
    }
}
