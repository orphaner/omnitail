//! Config file loading and merging with command-line options.
//!
//! ```toml
//! history = 20000
//!
//! [[source]]
//! type = "compose"
//! file = "docker-compose.yml"   # relative to this config file
//!
//! [[source]]
//! type = "file"
//! path = "logs/app.log"
//! name = "app"
//! lines = 100
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::Cli;
use crate::color::SchemeDef;
use crate::spec::{SourceConfig, SourceSpec};

const DEFAULT_HISTORY: usize = 10_000;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub history: Option<usize>,
    pub strip_ansi: Option<bool>,
    /// omnitail's own coloring, on by default.
    pub color: Option<bool>,
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceConfig>,
    /// User color schemes: `[scheme.<name>]` with a `rules` list.
    #[serde(default)]
    pub scheme: HashMap<String, SchemeDef>,
}

impl ConfigFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut config: ConfigFile = toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        config.resolve_paths(path.parent().unwrap_or(Path::new("")));
        Ok(config)
    }

    /// Paths in a config file are relative to the file, not to where omnitail runs.
    fn resolve_paths(&mut self, base: &Path) {
        let resolve = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        for source in &mut self.sources {
            match &mut source.spec {
                SourceSpec::File { path } => resolve(path),
                SourceSpec::Compose { file: Some(file), .. } => resolve(file),
                _ => {}
            }
        }
    }
}

/// Everything omnitail runs with, once the config file and the CLI are merged.
#[derive(Debug)]
pub struct Settings {
    pub sources: Vec<SourceConfig>,
    pub history: usize,
    pub strip_ansi: bool,
    pub lines: Option<usize>,
    pub color: bool,
    pub schemes: HashMap<String, SchemeDef>,
    /// Level filter for every pane, over each source's own.
    pub level: Option<crate::level::Level>,
}

impl Settings {
    /// Config file sources come first, then command-line ones; command-line
    /// options win over the file.
    pub fn resolve(cli: Cli) -> Result<Self> {
        let file = match &cli.config {
            Some(path) => ConfigFile::load(path)?,
            None => ConfigFile::default(),
        };
        let sources: Vec<SourceConfig> = file.sources.into_iter().chain(cli.sources.into_iter().map(Into::into)).collect();
        if sources.is_empty() {
            bail!("no sources to follow");
        }
        if sources.iter().filter(|s| s.spec == SourceSpec::Stdin).count() > 1 {
            bail!("stdin (`-`) can only be followed once");
        }
        Ok(Self {
            sources,
            history: cli.history.or(file.history).unwrap_or(DEFAULT_HISTORY),
            strip_ansi: cli.strip_ansi || file.strip_ansi.unwrap_or(false),
            lines: cli.lines,
            color: !cli.no_color && file.color.unwrap_or(true),
            schemes: file.scheme,
            level: cli.level,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn write_config(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("omnitail.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn loads_sources_with_options_and_resolves_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            history = 50
            [[source]]
            type = "file"
            path = "app.log"
            name = "app"
            lines = 5
            level = "WARNING"
            [[source]]
            type = "compose"
            file = "/abs/compose.yml"
            services = ["web"]
            [[source]]
            type = "k8s"
            target = "app=api"
            namespace = "prod"
            restart = false
            "#,
        );
        let config = ConfigFile::load(&path).unwrap();
        assert_eq!(config.history, Some(50));
        let [app, compose, k8s] = &config.sources[..] else { panic!("{config:?}") };
        assert_eq!(app.spec, SourceSpec::File { path: dir.path().join("app.log") });
        assert_eq!((app.title(), app.lines), ("app".into(), Some(5)));
        assert_eq!((app.level, compose.level), (Some(crate::level::Level::Warn), None));
        assert_eq!(
            compose.spec,
            SourceSpec::Compose { services: vec!["web".into()], file: Some("/abs/compose.yml".into()) }
        );
        assert_eq!(k8s.title(), "k8s:app=api -n prod");
        assert!(!k8s.restart());
    }

    #[test]
    fn schemes_and_color_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            color = false
            [scheme.mine]
            rules = [{ regex = 'x(\d)', group = 1, style = "red" }]
            [[source]]
            type = "cmd"
            command = "date"
            scheme = ["mine", "generic"]
            [[source]]
            type = "cmd"
            command = "ls"
            scheme = "none"
            "#,
        );
        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path.to_str().unwrap()]).unwrap()).unwrap();
        assert!(!s.color);
        assert_eq!(s.schemes["mine"].rules[0].group, 1);
        assert!(matches!(&s.sources[0].color_choice(), crate::color::Choice::Named(n) if n == &["mine", "generic"]));
        assert!(matches!(s.sources[1].color_choice(), crate::color::Choice::Off));
        assert!(matches!(SourceConfig::from(SourceSpec::Stdin).color_choice(), crate::color::Choice::Auto));
    }

    #[test]
    fn rejects_unknown_keys_and_types() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ConfigFile::load(&write_config(dir.path(), "histroy = 1")).is_err());
        assert!(ConfigFile::load(&write_config(dir.path(), "[[source]]\ntype = \"nope\"")).is_err());
    }

    #[test]
    fn cli_adds_sources_and_overrides_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "history = 50\n[[source]]\ntype = \"cmd\"\ncommand = \"date\"");
        let path = path.to_str().unwrap();

        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path, "b.log"]).unwrap()).unwrap();
        let titles: Vec<_> = s.sources.iter().map(SourceConfig::title).collect();
        assert_eq!(titles, ["cmd:date", "file:b.log"]);
        assert_eq!(s.history, 50);

        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path, "--history", "7"]).unwrap()).unwrap();
        assert_eq!(s.history, 7);
    }

    #[test]
    fn stdin_only_once() {
        assert!(Settings::resolve(Cli::try_parse_from(["omnitail", "-", "-"]).unwrap()).is_err());
    }
}
