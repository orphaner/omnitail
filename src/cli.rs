use std::path::PathBuf;

use clap::Parser;

use crate::spec::SourceSpec;

/// Follow several logs at once: files, commands, containers.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Sources to follow: `file:<path>`, `cmd:<command>`, `docker:<container>`.
    /// A bare argument is a file path.
    #[arg(required = true, value_name = "SOURCE")]
    pub sources: Vec<SourceSpec>,

    /// Lines kept in memory per source (scrollback size).
    #[arg(long, default_value_t = 10_000, value_name = "N")]
    pub history: usize,

    /// Existing lines shown when a source starts [default: enough to fill its pane].
    #[arg(short = 'n', long, value_name = "N")]
    pub lines: Option<usize>,

    /// Strip ANSI colors instead of rendering them.
    #[arg(long)]
    pub strip_ansi: bool,

    /// Where mtr writes its own logs (level set by MTR_LOG, default `info`).
    #[arg(long, value_name = "PATH", default_value_os_t = std::env::temp_dir().join("mtr.log"))]
    pub log_file: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sources_and_options() {
        let cli = Cli::try_parse_from(["mtr", "-n", "50", "a.log", "cmd:ping x", "docker:db"]).unwrap();
        assert_eq!(cli.lines, Some(50));
        assert_eq!(cli.sources.len(), 3);
        assert_eq!(cli.sources[1], SourceSpec::Cmd { command: "ping x".into() });
    }

    #[test]
    fn requires_a_source() {
        assert!(Cli::try_parse_from(["mtr"]).is_err());
    }

    #[test]
    fn reports_bad_source() {
        assert!(Cli::try_parse_from(["mtr", "dokcer:db"]).is_err());
    }
}
