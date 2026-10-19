use std::path::PathBuf;

use clap::Parser;

use crate::spec::SourceSpec;

/// Follow several logs at once: files, commands, containers.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Sources to follow: `<path>`, `file:<path>`, `cmd:<command>`,
    /// `docker:<container>` (or `d:`), `compose:[svc,…]` (or `dc:`), `k8s:<target> [kubectl args]`,
    /// `journal:[unit]`, `-` (stdin). None: the compose project of the current
    /// directory, if it has a compose file.
    #[arg(value_name = "SOURCE")]
    pub sources: Vec<SourceSpec>,

    /// TOML file listing sources (`[[source]]` tables) and options.
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Lines kept in memory per source (scrollback size) [default: 10000].
    #[arg(long, value_name = "N")]
    pub history: Option<usize>,

    /// Existing lines read when a source starts, kept in its history on top
    /// of --history [default: enough to fill its pane].
    #[arg(short = 'n', long, value_name = "N")]
    pub tail: Option<usize>,

    /// Show only lines of this level and up, in every pane (trace, debug,
    /// info, warn, error; change it with L/l). Lines with no level stay.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<crate::level::Level>,

    /// Strip ANSI colors instead of rendering them.
    #[arg(long)]
    pub strip_ansi: bool,

    /// Start with omnitail's own coloring off (toggle with c/C).
    #[arg(long)]
    pub no_color: bool,

    /// Print the sources and options as a config file (for -c), one source
    /// per compose service, and exit.
    #[arg(long)]
    pub print_config: bool,

    /// List the color schemes (with those of --config) and exit.
    #[arg(long)]
    pub list_schemes: bool,

    /// Where omnitail writes its own logs (level set by OMNITAIL_LOG, default `info`).
    #[arg(long, value_name = "PATH", default_value_os_t = std::env::temp_dir().join("omnitail.log"))]
    pub log_file: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sources_and_options() {
        let cli = Cli::try_parse_from(["omnitail", "-n", "50", "a.log", "cmd:ping x", "docker:db"]).unwrap();
        assert_eq!(cli.tail, Some(50));
        assert_eq!(cli.sources.len(), 3);
        assert_eq!(cli.sources[1], SourceSpec::Cmd { command: "ping x".into() });
        let cli = Cli::try_parse_from(["omnitail", "--level", "warn", "a.log"]).unwrap();
        assert_eq!(cli.level, Some(crate::level::Level::Warn));
        assert!(Cli::try_parse_from(["omnitail", "--level", "loud", "a.log"]).is_err());
    }

    #[test]
    fn sources_are_optional() {
        let cli = Cli::try_parse_from(["omnitail", "--print-config"]).unwrap();
        assert!(cli.sources.is_empty() && cli.print_config);
    }

    #[test]
    fn reports_bad_source() {
        assert!(Cli::try_parse_from(["omnitail", "dokcer:db"]).is_err());
    }
}
