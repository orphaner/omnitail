use std::io::{BufRead, BufReader};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Emitter, Source, SourceStatus, decode_line};

/// Follows omnitail's own stdin (`cmd | omnitail -`). Keys still work: crossterm reads
/// them from /dev/tty when stdin is not a terminal.
pub struct StdinSource;

impl Source for StdinSource {
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()> {
        // A plain thread, not tokio's blocking pool: a blocked read must not
        // keep the runtime (and so omnitail) from exiting.
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1024);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(std::io::stdin().lock());
            loop {
                let mut buf = Vec::new();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        if tx.blocking_send(buf).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        tokio::spawn(async move {
            out.status(SourceStatus::Running).await;
            while let Some(bytes) = rx.recv().await {
                if !out.line(decode_line(&bytes)).await {
                    return;
                }
            }
            out.status(SourceStatus::Ended).await;
        })
    }
}
