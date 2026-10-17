use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Emitter, Source, SourceEvent, SourceStatus, decode_line};

/// Follows a file like `tail -F`: survives truncation, rotation, in-place
/// rewrites (editors) and the file not existing yet.
pub struct FileSource {
    follower: FileFollower,
}

impl FileSource {
    pub fn new(path: PathBuf, initial_lines: usize) -> Self {
        Self { follower: FileFollower::new(path, initial_lines) }
    }
}

/// Safety net for missed notifications (and for directories that do not
/// exist yet, which cannot be watched).
const POLL_INTERVAL: Duration = Duration::from_secs(1);

impl Source for FileSource {
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()> {
        let mut follower = self.follower;
        tokio::spawn(async move {
            let (wake_tx, mut wake_rx) = mpsc::unbounded_channel();
            // Watch the directory rather than the file, so that rotation and
            // re-creation are seen too. Any event just triggers a re-check.
            let _watcher = notify::recommended_watcher(move |_| {
                let _ = wake_tx.send(());
            })
            .and_then(|mut w| {
                w.watch(watch_dir(&follower.path), RecursiveMode::NonRecursive)?;
                Ok(w)
            })
            .inspect_err(|e| tracing::warn!(path = %follower.path.display(), "no fs notifications: {e}"))
            .ok();

            let mut tick = tokio::time::interval(POLL_INTERVAL);
            loop {
                for event in follower.poll() {
                    if !out.emit(event).await {
                        return;
                    }
                }
                tokio::select! {
                    Some(()) = wake_rx.recv() => while wake_rx.try_recv().is_ok() {},
                    _ = tick.tick() => {}
                }
            }
        })
    }
}

fn watch_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// The synchronous part of following a file. Each [`poll`](Self::poll)
/// looks at the file and returns whatever happened since the last one.
pub struct FileFollower {
    path: PathBuf,
    /// Lines of existing content shown when opening with `tail_on_open`.
    initial_lines: usize,
    /// True for the first open and after a rewrite: show the last lines.
    /// Files that appear later (rotation, creation) are read from the start.
    tail_on_open: bool,
    current: Option<OpenFile>,
    last_status: Option<SourceStatus>,
}

struct OpenFile {
    file: File,
    id: Option<FileId>,
    pos: u64,
    /// Bytes after the last newline, held until the line is complete.
    partial: Vec<u8>,
    /// The last bytes before `pos`, to notice the file being rewritten in
    /// place with different content (its size alone does not tell).
    recent: Vec<u8>,
}

const RECENT_LEN: usize = 256;

impl FileFollower {
    pub fn new(path: PathBuf, initial_lines: usize) -> Self {
        Self { path, initial_lines, tail_on_open: true, current: None, last_status: None }
    }

    pub fn poll(&mut self) -> Vec<SourceEvent> {
        let mut out = Vec::new();
        let meta = fs::metadata(&self.path).ok();

        if let Some(cur) = &mut self.current {
            let same_file = meta.as_ref().is_some_and(|m| file_id(m) == cur.id);
            if same_file {
                let len = meta.as_ref().map_or(0, Metadata::len);
                if len < cur.pos {
                    tracing::info!(path = %self.path.display(), "truncated");
                    cur.reset();
                    out.push(SourceEvent::Reset("truncated"));
                } else if cur.rewritten().unwrap_or(true) {
                    // New content, not a continuation: reopen as if new.
                    tracing::info!(path = %self.path.display(), "rewritten in place");
                    out.push(SourceEvent::Reset("rewritten"));
                    self.current = None;
                    self.tail_on_open = true;
                }
                if let Some(cur) = &mut self.current
                    && let Err(e) = cur.read_new(&mut out)
                {
                    self.fail(e, &mut out);
                }
            } else {
                // Rotated or deleted: drain what was written before the
                // switch, then fall through to opening the new file.
                tracing::info!(path = %self.path.display(), "rotated or removed");
                let _ = cur.read_new(&mut out);
                cur.flush_partial(&mut out);
                self.current = None;
            }
        }

        if self.current.is_none() {
            match meta {
                Some(_) => match self.open() {
                    Ok(mut cur) => {
                        self.set_status(SourceStatus::Running, &mut out);
                        if let Err(e) = cur.read_new(&mut out) {
                            self.fail(e, &mut out);
                        } else {
                            self.current = Some(cur);
                        }
                    }
                    Err(e) => self.set_status(SourceStatus::Waiting(e.to_string()), &mut out),
                },
                None => self.set_status(SourceStatus::Waiting("file not found".into()), &mut out),
            }
        }
        out
    }

    fn open(&mut self) -> io::Result<OpenFile> {
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let pos = if self.tail_on_open { tail_offset(&mut file, meta.len(), self.initial_lines)? } else { 0 };
        let recent = read_at(&mut file, pos.saturating_sub(RECENT_LEN as u64), pos)?;
        file.seek(SeekFrom::Start(pos))?;
        self.tail_on_open = false;
        Ok(OpenFile { file, id: file_id(&meta), pos, partial: Vec::new(), recent })
    }

    fn fail(&mut self, e: io::Error, out: &mut Vec<SourceEvent>) {
        self.current = None;
        self.set_status(SourceStatus::Error(e.to_string()), out);
    }

    fn set_status(&mut self, status: SourceStatus, out: &mut Vec<SourceEvent>) {
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            out.push(SourceEvent::Status(status));
        }
    }
}

impl OpenFile {
    fn read_new(&mut self, out: &mut Vec<SourceEvent>) -> io::Result<()> {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = self.file.read(&mut chunk)?;
            if n == 0 {
                return Ok(());
            }
            self.pos += n as u64;
            self.remember(&chunk[..n]);
            let mut rest = &chunk[..n];
            while let Some(i) = rest.iter().position(|&b| b == b'\n') {
                self.partial.extend_from_slice(&rest[..i]);
                out.push(SourceEvent::Line(decode_line(&self.partial)));
                self.partial.clear();
                rest = &rest[i + 1..];
            }
            self.partial.extend_from_slice(rest);
        }
    }

    fn flush_partial(&mut self, out: &mut Vec<SourceEvent>) {
        if !self.partial.is_empty() {
            out.push(SourceEvent::Line(decode_line(&self.partial)));
            self.partial.clear();
        }
    }

    fn remember(&mut self, bytes: &[u8]) {
        let bytes = &bytes[bytes.len().saturating_sub(RECENT_LEN)..];
        let excess = (self.recent.len() + bytes.len()).saturating_sub(RECENT_LEN);
        self.recent.drain(..excess);
        self.recent.extend_from_slice(bytes);
    }

    /// Whether the bytes just before our position are no longer the ones
    /// we read there.
    fn rewritten(&mut self) -> io::Result<bool> {
        if self.recent.is_empty() {
            return Ok(false);
        }
        let now = read_at(&mut self.file, self.pos - self.recent.len() as u64, self.pos)?;
        self.file.seek(SeekFrom::Start(self.pos))?;
        Ok(now != self.recent)
    }

    fn reset(&mut self) {
        // Seeking cannot fail on a regular file opened for reading.
        let _ = self.file.seek(SeekFrom::Start(0));
        self.pos = 0;
        self.partial.clear();
        self.recent.clear();
    }
}

/// Reads `start..end`, or less if the file is now shorter. Moves the cursor.
fn read_at(file: &mut File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((end - start) as usize);
    file.take(end - start).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Identity of a file on disk, to tell "same file, grown" from "replaced".
type FileId = (u64, u64);

#[cfg(unix)]
fn file_id(meta: &Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_: &Metadata) -> Option<FileId> {
    None
}

/// Offset where the last `n` complete lines start. A final line without a
/// newline counts as one of them.
fn tail_offset(file: &mut File, len: u64, n: usize) -> io::Result<u64> {
    if n == 0 || len == 0 {
        return Ok(len);
    }
    const CHUNK: u64 = 8 * 1024;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut end = len;
    let mut newlines = 0;
    let mut skip_last = true; // a trailing newline ends the last line, it does not start one
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let chunk = &mut buf[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        for (i, &b) in chunk.iter().enumerate().rev() {
            if skip_last {
                skip_last = false;
                if b == b'\n' {
                    continue;
                }
            }
            if b == b'\n' {
                newlines += 1;
                if newlines == n {
                    return Ok(start + i as u64 + 1);
                }
            }
        }
        end = start;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn lines(events: Vec<SourceEvent>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                SourceEvent::Line(l) => Some(l),
                _ => None,
            })
            .collect()
    }

    fn append(path: &Path, s: &str) {
        fs::OpenOptions::new().append(true).create(true).open(path).unwrap().write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn tail_offset_cases() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        for (content, n, expected) in [
            ("a\nb\nc\n", 2, "b\nc\n"),
            ("a\nb\nc", 2, "b\nc"),
            ("a\nb\n", 5, "a\nb\n"),
            ("a\nb\n", 0, ""),
            ("", 3, ""),
        ] {
            fs::write(&p, content).unwrap();
            let mut f = File::open(&p).unwrap();
            let off = tail_offset(&mut f, content.len() as u64, n).unwrap();
            assert_eq!(&content[off as usize..], expected, "{content:?} n={n}");
        }
    }

    #[test]
    fn tail_offset_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let content: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        fs::write(&p, &content).unwrap();
        let mut f = File::open(&p).unwrap();
        let off = tail_offset(&mut f, content.len() as u64, 3000).unwrap();
        assert!(content[off as usize..].starts_with("line 2000\n"));
    }

    #[test]
    fn initial_tail_then_appends() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        fs::write(&p, "1\n2\n3\n").unwrap();
        let mut f = FileFollower::new(p.clone(), 2);
        let first = f.poll();
        assert_eq!(first[0], SourceEvent::Status(SourceStatus::Running));
        assert_eq!(lines(first), ["2", "3"]);

        append(&p, "4\npart");
        assert_eq!(lines(f.poll()), ["4"]);
        append(&p, "ial\n");
        assert_eq!(lines(f.poll()), ["partial"]);
        assert!(f.poll().is_empty());
    }

    #[test]
    fn truncation_restarts_from_the_top() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        fs::write(&p, "old line one\nold line two\n").unwrap();
        let mut f = FileFollower::new(p.clone(), 10);
        f.poll();
        fs::write(&p, "new\n").unwrap();
        let events = f.poll();
        assert_eq!(events[0], SourceEvent::Reset("truncated"));
        assert_eq!(lines(events), ["new"]);
    }

    #[test]
    fn in_place_rewrite_by_an_editor_is_detected() {
        // Editors truncate and rewrite the same inode; by the time we look,
        // the file can already be longer than our read position.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        fs::write(&p, "a\nyo\n\n").unwrap();
        let mut f = FileFollower::new(p.clone(), 2);
        assert_eq!(lines(f.poll()), ["yo", ""]);

        fs::write(&p, "a\nyo\nde\nda\ndudu\n").unwrap();
        let events = f.poll();
        assert_eq!(events[0], SourceEvent::Reset("rewritten"));
        assert_eq!(lines(events), ["da", "dudu"], "last lines of the new content, not a misaligned read");
    }

    #[test]
    fn plain_append_is_not_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        fs::write(&p, "a\nb\n").unwrap();
        let mut f = FileFollower::new(p.clone(), 10);
        f.poll();
        // Same prefix written again (e.g. an editor that only appended).
        fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(f.poll(), [SourceEvent::Line("c".into())]);
    }

    #[test]
    fn rotation_drains_old_file_then_reads_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let rotated = dir.path().join("log.1");
        fs::write(&p, "a\n").unwrap();
        let mut f = FileFollower::new(p.clone(), 10);
        f.poll();

        fs::rename(&p, &rotated).unwrap();
        append(&rotated, "last words");
        fs::write(&p, "fresh\n").unwrap();
        assert_eq!(lines(f.poll()), ["last words", "fresh"]);
    }

    #[test]
    fn waits_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("later");
        let mut f = FileFollower::new(p.clone(), 10);
        assert!(matches!(f.poll()[..], [SourceEvent::Status(SourceStatus::Waiting(_))]));
        assert!(f.poll().is_empty(), "status is only reported on change");

        fs::write(&p, "x\n").unwrap();
        assert_eq!(lines(f.poll()), ["x"]);

        fs::remove_file(&p).unwrap();
        assert!(matches!(f.poll()[..], [SourceEvent::Status(SourceStatus::Waiting(_))]));
    }
}
