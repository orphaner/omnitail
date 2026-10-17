mod app;
mod buffer;
mod cli;
mod config;
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
use crate::config::Settings;
use crate::source::Emitter;

/// Room for bursts between two frames; beyond that, sources wait.
const EVENT_QUEUE: usize = 8192;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    init_logging(&cli)?;
    let settings = Settings::resolve(cli)?;
    // Before the TUI starts, so that errors (e.g. no compose project) print normally.
    let sources = source::expand(settings.sources).await?;

    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let pane_heights = pane_heights(sources.len());
    for (id, config) in sources.iter().enumerate() {
        let lines = config.lines.or(settings.lines).unwrap_or(pane_heights[id]);
        source::build(config, lines).spawn(Emitter::new(id, tx.clone()));
    }
    drop(tx);

    let mut app = App::new(sources.iter().map(|s| s.title()).collect(), settings.history, settings.strip_ansi);
    // `init` also installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = app::run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    result
}

/// Content rows of each pane on the current screen: by default, sources
/// load just enough existing lines to fill their pane.
fn pane_heights(n: usize) -> Vec<usize> {
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
