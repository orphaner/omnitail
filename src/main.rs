mod app;
mod buffer;
mod cli;
mod source;
mod spec;
mod ui;

use std::fs::File;
use std::sync::Mutex;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::source::{Emitter, SourceOptions};

/// Room for bursts between two frames; beyond that, sources wait.
const EVENT_QUEUE: usize = 8192;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    init_logging(&cli)?;

    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let initial_lines = initial_lines(&cli);
    for (id, spec) in cli.sources.iter().enumerate() {
        let opts = SourceOptions { initial_lines: initial_lines[id] };
        source::build(spec, opts).spawn(Emitter::new(id, tx.clone()));
    }
    drop(tx);

    let mut app = App::new(cli.sources.iter().map(ToString::to_string).collect(), cli.history, cli.strip_ansi);
    // `init` also installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = app::run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    result
}

/// `-n` if given, otherwise what fills each pane on the current screen.
fn initial_lines(cli: &cli::Cli) -> Vec<usize> {
    let n = cli.sources.len();
    if let Some(lines) = cli.lines {
        return vec![lines; n];
    }
    match crossterm::terminal::size() {
        Ok((width, height)) => ui::pane_body_heights(ratatui::layout::Rect::new(0, 0, width, height), n),
        Err(_) => vec![10; n],
    }
}

/// The TUI owns stdout, so mtr's own logs go to a file.
fn init_logging(cli: &cli::Cli) -> Result<()> {
    let file = File::create(&cli.log_file).with_context(|| format!("cannot create {}", cli.log_file.display()))?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_env("MTR_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    Ok(())
}
