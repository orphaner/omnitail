use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use super::{Emitter, Source, SourceStatus};

/// Builds the command line of each run. `since` is when the previous run
/// ended (`None` on the first one), so log streams that support it resume
/// there on restart instead of replaying their tail.
pub type Argv = Box<dyn Fn(Option<DateTime<Utc>>) -> Vec<String> + Send>;

/// Runs a process and follows its stdout and stderr, merged into one
/// stream, optionally starting it again whenever it ends.
pub struct CommandSource {
    argv: Argv,
    restart: Option<Backoff>,
}

impl CommandSource {
    pub fn new(argv: Argv) -> Self {
        Self { argv, restart: None }
    }

    /// Runs `command` through `sh -c`, so quoting and pipes work as in a shell.
    pub fn shell(command: &str) -> Self {
        let command = command.to_owned();
        Self::new(Box::new(move |_| vec!["sh".into(), "-c".into(), command.clone()]))
    }

    pub fn restart(mut self, restart: bool) -> Self {
        self.restart = restart.then(Backoff::default);
        self
    }
}

/// Delay before a restart: doubles while runs keep failing fast, so a
/// missing container or a broken command does not spin.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    min: Duration,
    max: Duration,
    /// A run that lasted this long resets the delay.
    healthy: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { min: Duration::from_secs(1), max: Duration::from_secs(30), healthy: Duration::from_secs(10) }
    }
}

impl Source for CommandSource {
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut since = None;
            let mut delay = self.restart.map(|b| b.min).unwrap_or_default();
            loop {
                let started = Instant::now();
                let status = run_once((self.argv)(since), &out).await;
                if !out.status(status).await {
                    return;
                }
                let Some(backoff) = self.restart else { return };
                since = Some(Utc::now());
                if started.elapsed() >= backoff.healthy {
                    delay = backoff.min;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(backoff.max);
            }
        })
    }
}

/// Runs the command to completion and returns how it ended.
async fn run_once(argv: Vec<String>, out: &Emitter) -> SourceStatus {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        // The TUI owns the terminal: children must not read from it.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so that everything the command starts (`a | b`,
    // background jobs) can be stopped together.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return SourceStatus::Error(format!("{}: {e}", argv[0])),
    };
    // Dropped when this run ends or when the task is dropped (app exit).
    let _group = child.id().map(ProcessGroup);
    if !out.status(SourceStatus::Running).await {
        return SourceStatus::Exited(None);
    }

    let stdout = tokio::spawn(pump(child.stdout.take().unwrap(), out.clone()));
    let stderr = tokio::spawn(pump(child.stderr.take().unwrap(), out.clone()));
    let _ = tokio::join!(stdout, stderr);

    match child.wait().await {
        Ok(s) => SourceStatus::Exited(s.code()),
        Err(e) => SourceStatus::Error(e.to_string()),
    }
}

/// Terminates a whole process group when dropped.
struct ProcessGroup(u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: plain syscall; a group that is already gone gives ESRCH.
        unsafe {
            libc::killpg(self.0 as libc::pid_t, libc::SIGTERM);
        }
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
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;

    use super::*;
    use crate::source::SourceEvent;

    async fn collect(source: CommandSource) -> Vec<SourceEvent> {
        let (tx, mut rx) = mpsc::channel(64);
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
        let events = collect(CommandSource::new(Box::new(|_| vec!["/nonexistent/omnitail-test".into()]))).await;
        assert!(matches!(events[..], [SourceEvent::Status(SourceStatus::Error(_))]));
    }

    #[tokio::test]
    async fn restarts_with_since_and_backoff() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let mut source = CommandSource::new(Box::new(move |since| {
            record.lock().unwrap().push(since.is_some());
            vec!["sh".into(), "-c".into(), "echo run".into()]
        }))
        .restart(true);
        source.restart = Some(Backoff {
            min: Duration::from_millis(10),
            max: Duration::from_millis(40),
            healthy: Duration::from_secs(3600),
        });

        let (tx, mut rx) = mpsc::channel(64);
        let task = Box::new(source).spawn(Emitter::new(0, tx));
        let mut runs = 0;
        while runs < 4 {
            if let (_, SourceEvent::Line(_)) = rx.recv().await.unwrap() {
                runs += 1;
            }
        }
        task.abort();
        assert_eq!(seen.lock().unwrap()[..4], [false, true, true, true], "only the first run replays the tail");
    }

    #[tokio::test]
    async fn dropping_the_source_kills_the_whole_process_group() {
        let (tx, mut rx) = mpsc::channel(64);
        // The grandchild prints its pid, then outlives `sh` unless killed.
        let task = Box::new(CommandSource::shell("sleep 300 & echo $!; wait")).spawn(Emitter::new(0, tx));
        let pid = loop {
            if let (_, SourceEvent::Line(l)) = rx.recv().await.unwrap() {
                break l.parse::<i32>().unwrap();
            }
        };
        task.abort();
        let _ = task.await;
        let alive = |pid| unsafe { libc::kill(pid, 0) == 0 };
        for _ in 0..50 {
            if !alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("grandchild {pid} survived");
    }
}
