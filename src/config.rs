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
//! tail = 100
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cli::Cli;
use crate::color::SchemeDef;
use crate::spec::{SourceConfig, SourceSpec};

const DEFAULT_HISTORY: usize = 10_000;

/// Files `docker compose` reads by default, in its order of preference.
const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub history: Option<usize>,
    pub strip_ansi: Option<bool>,
    /// Kubernetes through the kubectl CLI rather than the built-in client.
    pub kubectl: Option<bool>,
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
    pub kubectl: bool,
    pub tail: Option<usize>,
    pub color: bool,
    pub schemes: HashMap<String, SchemeDef>,
    /// Level filter for every pane, over each source's own.
    pub level: Option<crate::level::Level>,
}

impl Settings {
    /// Config file sources come first, then command-line ones; command-line
    /// options win over the file.
    /// With neither, stdin when something is piped into omnitail
    /// (`piped`), else the compose project of the current directory.
    pub fn resolve(cli: Cli, piped: bool) -> Result<Self> {
        let file = match &cli.config {
            Some(path) => ConfigFile::load(path)?,
            None => ConfigFile::default(),
        };
        let mut sources: Vec<SourceConfig> =
            file.sources.into_iter().chain(cli.sources.into_iter().map(Into::into)).collect();
        if sources.is_empty() && piped {
            sources.push(SourceSpec::Stdin.into());
        }
        if sources.is_empty() && cli.config.is_none() {
            sources.extend(compose_here(Path::new(".")));
        }
        if sources.is_empty() {
            bail!("no sources to follow, and no compose file here (see --help)");
        }
        if sources.iter().filter(|s| s.spec == SourceSpec::Stdin).count() > 1 {
            bail!("stdin (`-`) can only be followed once");
        }
        Ok(Self {
            sources,
            history: cli.history.or(file.history).unwrap_or(DEFAULT_HISTORY),
            strip_ansi: cli.strip_ansi || file.strip_ansi.unwrap_or(false),
            kubectl: cli.kubectl || file.kubectl.unwrap_or(false),
            tail: cli.tail,
            color: !cli.no_color && file.color.unwrap_or(true),
            schemes: file.scheme,
            level: cli.level,
        })
    }

    /// A config file for `-c` running the same thing as these settings,
    /// with `sources` as expanded (one per compose service). Options given
    /// for every pane (`--level`, `--tail`) go into each source.
    pub fn to_toml(&self, sources: &[SourceConfig]) -> Result<String> {
        #[derive(Serialize)]
        struct Printed<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            history: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            strip_ansi: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            kubectl: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            color: Option<bool>,
            source: Vec<SourceConfig>,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            scheme: BTreeMap<&'a String, &'a SchemeDef>,
        }
        let printed = Printed {
            history: Some(self.history).filter(|&h| h != DEFAULT_HISTORY),
            strip_ansi: self.strip_ansi.then_some(true),
            kubectl: self.kubectl.then_some(true),
            color: (!self.color).then_some(false),
            source: sources
                .iter()
                .map(|s| SourceConfig { tail: s.tail.or(self.tail), level: self.level.or(s.level), ..s.clone() })
                .collect(),
            scheme: self.schemes.iter().collect(),
        };
        let body = toml::to_string(&printed).context("cannot write the config")?;
        Ok(format!("# omnitail config, run it with: omnitail -c <this file>\n\n{body}"))
    }
}

/// The compose project of `dir`, if it has a compose file.
fn compose_here(dir: &Path) -> Option<SourceConfig> {
    let file = COMPOSE_FILES.iter().find(|f| dir.join(f).is_file())?;
    Some(SourceSpec::Compose { services: vec![], file: Some(file.into()) }.into())
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
            tail = 5
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
        assert_eq!((app.title(), app.tail), ("app".into(), Some(5)));
        assert_eq!((app.level, compose.level), (Some(crate::level::Level::Warn), None));
        assert_eq!(
            compose.spec,
            SourceSpec::Compose { services: vec!["web".into()], file: Some("/abs/compose.yml".into()) }
        );
        assert_eq!(k8s.title(), "k8s: prod > app=api");
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
        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path.to_str().unwrap()]).unwrap(), false).unwrap();
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

        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path, "b.log"]).unwrap(), false).unwrap();
        let titles: Vec<_> = s.sources.iter().map(SourceConfig::title).collect();
        assert_eq!(titles, ["cmd:date", "file:b.log"]);
        assert_eq!(s.history, 50);

        let s = Settings::resolve(Cli::try_parse_from(["omnitail", "-c", path, "--history", "7"]).unwrap(), false).unwrap();
        assert_eq!(s.history, 7);
    }

    #[test]
    fn compose_file_of_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(compose_here(dir.path()), None);
        std::fs::write(dir.path().join("docker-compose.yml"), "").unwrap();
        std::fs::write(dir.path().join("compose.yaml"), "").unwrap();
        let spec = compose_here(dir.path()).map(|s| s.spec);
        assert_eq!(spec, Some(SourceSpec::Compose { services: vec![], file: Some("compose.yaml".into()) }));
    }

    #[test]
    fn printed_config_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "color = false\n[scheme.mine]\nrules = [{ regex = 'x', style = \"red\" }]\n[[source]]\ntype = \"cmd\"\ncommand = \"date\"\ntail = 3",
        );
        let cli = Cli::try_parse_from(["omnitail", "-c", path.to_str().unwrap(), "--level", "warn", "-n", "9"]).unwrap();
        let settings = Settings::resolve(cli, false).unwrap();
        let services = ["web", "db"].map(|s| SourceConfig {
            name: Some(s.into()),
            ..SourceSpec::Compose { services: vec![s.into()], file: Some("compose.yaml".into()) }.into()
        });
        let sources: Vec<_> = settings.sources.iter().cloned().chain(services).collect();
        let text = settings.to_toml(&sources).unwrap();
        assert!(text.contains("type = \"compose\"\nservices = [\"web\"]\nfile = \"compose.yaml\"\n"), "{text}");
        assert!(!text.contains("history"), "defaults are left out: {text}");

        let back = write_config(dir.path(), &text);
        let config = ConfigFile::load(&back).unwrap();
        assert_eq!(config.color, Some(false));
        assert_eq!(config.scheme, settings.schemes);
        let [date, web, db] = &config.sources[..] else { panic!("{text}") };
        assert_eq!((date.tail, date.level), (Some(3), Some(crate::level::Level::Warn)), "own tail wins, --level applies");
        assert_eq!((web.tail, web.title(), db.title()), (Some(9), "web".into(), "db".into()));
        assert_eq!(web.spec, SourceSpec::Compose { services: vec!["web".into()], file: Some(dir.path().join("compose.yaml")) });
    }

    #[test]
    fn stdin_only_once() {
        assert!(Settings::resolve(Cli::try_parse_from(["omnitail", "-", "-"]).unwrap(), false).is_err());
        let piped = |args: &[&str]| Settings::resolve(Cli::try_parse_from(args).unwrap(), true).unwrap().sources;
        assert_eq!(piped(&["omnitail"])[0].spec, SourceSpec::Stdin, "`cmd | omnitail`");
        assert_eq!(piped(&["omnitail", "a.log"]).len(), 1, "explicit sources: stdin only with `-`");
    }
}
