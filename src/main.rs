mod app;
mod buffer;
mod cli;
mod color;
mod config;
mod pattern;
mod source;
mod spec;
mod ui;
mod view;

use std::fs::File;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::color::{Choice, Library, PaneColor};
use crate::config::{ConfigFile, Settings};
use crate::spec::{SourceConfig, SourceSpec};
use crate::source::Emitter;

/// Room for bursts between two frames; beyond that, sources wait.
const EVENT_QUEUE: usize = 8192;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    if cli.list_schemes {
        return list_schemes(&cli);
    }
    init_logging(&cli)?;
    let settings = Settings::resolve(cli)?;
    // Before the TUI starts, so that errors (e.g. no compose project) print normally.
    let sources = source::expand(settings.sources).await?;

    // Validates scheme names and rules before anything starts.
    let library = Arc::new(Library::new(&settings.schemes).map_err(|e| anyhow!(e))?);
    let panes = sources
        .iter()
        .map(|s| {
            let color = PaneColor::new(color_choice(s, &library), &library, settings.color).map_err(|e| anyhow!(e))?;
            Ok((s.title(), color))
        })
        .collect::<Result<Vec<_>>>()?;

    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let pane_heights = pane_heights(sources.len());
    for (id, config) in sources.iter().enumerate() {
        let lines = config.lines.or(settings.lines).unwrap_or(pane_heights[id]);
        source::build(config, lines).spawn(Emitter::new(id, tx.clone()));
    }
    drop(tx);

    let mut app = App::new(panes, library, settings.history, settings.strip_ansi);
    // `init` also installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = app::run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    result
}

/// The source's scheme setting, or for a file without one, the scheme its
/// path calls for (`paths` in schemes, e.g. /var/log/mail/ → postfix).
fn color_choice(source: &SourceConfig, library: &Library) -> Choice {
    let choice = source.color_choice();
    if let (Choice::Auto, SourceSpec::File { path }) = (&choice, &source.spec) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        if let Some(name) = library.for_path(&path.to_string_lossy()) {
            return Choice::Named(vec![name.to_owned(), color::FALLBACK.to_owned()]);
        }
    }
    choice
}

fn list_schemes(cli: &cli::Cli) -> Result<()> {
    let user = match &cli.config {
        Some(path) => ConfigFile::load(path)?.scheme,
        None => Default::default(),
    };
    let library = Library::new(&user).map_err(|e| anyhow!(e))?;
    let width = library.list().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, info) in library.list() {
        let description = info.description.as_deref().unwrap_or("");
        println!("{name:width$}  {:<9}  {description}", info.origin.label());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_a_scheme_from_their_path_unless_set() {
        let library = Library::new(&Default::default()).unwrap();
        let file = |path: &str| SourceConfig::from(SourceSpec::File { path: path.into() });
        assert!(matches!(color_choice(&file("/var/log/fail2ban.log"), &library), Choice::Named(n) if n == ["f2b", "generic"]));
        assert!(matches!(color_choice(&file("/tmp/app.log"), &library), Choice::Auto));
        let explicit = SourceConfig { scheme: Some(crate::spec::SchemeRef::One("json".into())), ..file("/var/log/fail2ban.log") };
        assert!(matches!(color_choice(&explicit, &library), Choice::Named(n) if n == ["json"]));
    }
}
