//! Declarative description of sources, shared by the CLI and config files.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What to follow. Config files use the same shape, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceSpec {
    File {
        path: PathBuf,
    },
    Cmd {
        command: String,
    },
    Docker {
        container: String,
    },
    /// Services of a compose project; each one gets its own pane. No
    /// services means all of them.
    Compose {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        services: Vec<String>,
        /// Compose file; the project is the one in the current directory otherwise.
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
    },
    K8s {
        /// `pod/x`, `deploy/x`, any `kubectl logs` target, or a label selector (`app=api`).
        target: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        container: Option<String>,
        /// Extra `kubectl logs` arguments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Label selector of a workload's pods, found at start: their logs
        /// are followed together, each line tagged with its pod.
        #[serde(skip)]
        pods: Option<String>,
    },
    /// systemd journal, of one unit or of everything.
    Journal {
        #[serde(skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    Stdin,
}

impl FromStr for SourceSpec {
    type Err = String;

    /// Parses `type:arg` arguments; an argument without a known prefix is a
    /// file path, and `-` is stdin.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "-" {
            return Ok(SourceSpec::Stdin);
        }
        let Some((prefix, rest)) = s.split_once(':') else {
            return non_empty(s).map(|p| SourceSpec::File { path: p.into() });
        };
        match prefix {
            "file" => non_empty(rest).map(|p| SourceSpec::File { path: p.into() }),
            "cmd" => non_empty(rest).map(|c| SourceSpec::Cmd { command: c.into() }),
            "docker" | "d" => non_empty(rest).map(|c| SourceSpec::Docker { container: c.into() }),
            "compose" | "dc" => Ok(SourceSpec::Compose {
                services: rest.split(',').map(str::trim).filter(|s| !s.is_empty()).map(Into::into).collect(),
                file: None,
            }),
            "k8s" => {
                // `k8s:deploy/api -n prod`: a target, then kubectl arguments.
                let mut words = rest.split_whitespace();
                let target = words.next().ok_or("empty source")?.to_owned();
                let (mut namespace, mut context, mut container, mut args) = (None, None, None, Vec::new());
                while let Some(word) = words.next() {
                    let (flag, inline) = match word.split_once('=') {
                        Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                        _ => (word, None),
                    };
                    let slot = match flag {
                        "-n" | "--namespace" => &mut namespace,
                        "--context" => &mut context,
                        "-c" | "--container" => &mut container,
                        _ => {
                            args.push(word.to_owned());
                            continue;
                        }
                    };
                    *slot = Some(inline.or_else(|| words.next().map(Into::into)).ok_or(format!("{word} needs a value"))?);
                }
                Ok(SourceSpec::K8s { target, namespace, context, container, args, pods: None })
            }
            "journal" => Ok(SourceSpec::Journal { unit: Some(rest.trim()).filter(|u| !u.is_empty()).map(Into::into) }),
            // Something like `dokcer:x` is far more likely a typo than a file name.
            p if !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()) => Err(format!(
                "unknown source type `{p}` (expected file, cmd, docker/d, compose/dc, k8s or journal; \
                 use `file:{s}` for a path containing a colon)"
            )),
            _ => Ok(SourceSpec::File { path: s.into() }),
        }
    }
}

fn non_empty(s: &str) -> Result<&str, String> {
    if s.trim().is_empty() { Err("empty source".into()) } else { Ok(s) }
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceSpec::File { path } => write!(f, "file:{}", path.display()),
            SourceSpec::Cmd { command } => write!(f, "cmd:{command}"),
            SourceSpec::Docker { container } => write!(f, "docker:{container}"),
            SourceSpec::Compose { services, .. } if services.is_empty() => write!(f, "compose"),
            SourceSpec::Compose { services, .. } => write!(f, "compose:{}", services.join(",")),
            SourceSpec::K8s { target, namespace, container, .. } => {
                write!(f, "k8s:{target}")?;
                if let Some(c) = container {
                    write!(f, " · {c}")?;
                }
                match namespace {
                    Some(ns) => write!(f, " -n {ns}"),
                    None => Ok(()),
                }
            }
            SourceSpec::Journal { unit: Some(unit) } => write!(f, "journal:{unit}"),
            SourceSpec::Journal { unit: None } => write!(f, "journal"),
            SourceSpec::Stdin => write!(f, "stdin"),
        }
    }
}

/// A source plus the options any source can have. On the command line only
/// the spec can be given; config files can set the rest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SourceConfig {
    #[serde(flatten)]
    pub spec: SourceSpec,
    /// Pane title, instead of the source description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Existing lines read at start, kept on top of the history (default:
    /// fill the pane).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    /// Start the source again when it ends. Default: on for containers,
    /// whose log streams end whenever the container restarts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart: Option<bool>,
    /// Color scheme(s), in priority order; `"none"` turns omnitail's coloring
    /// off. Default: guessed from the first lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<SchemeRef>,
    /// Show only lines of this level and up (`L`/`l` change it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<crate::level::Level>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SchemeRef {
    One(String),
    Many(Vec<String>),
}

impl SourceConfig {
    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.spec.to_string())
    }

    pub fn color_choice(&self) -> crate::color::Choice {
        use crate::color::Choice;
        match &self.scheme {
            None => Choice::Auto,
            Some(SchemeRef::One(name)) if name == "none" => Choice::Off,
            Some(SchemeRef::One(name)) => Choice::Named(vec![name.clone()]),
            Some(SchemeRef::Many(names)) => Choice::Named(names.clone()),
        }
    }

    /// Whether lines come prefixed with their pod (`kubectl logs --prefix`).
    pub fn prefixes_pods(&self) -> bool {
        matches!(&self.spec, SourceSpec::K8s { target, pods, .. } if pods.is_some() || target.contains('='))
    }

    pub fn restart(&self) -> bool {
        self.restart.unwrap_or(matches!(
            self.spec,
            SourceSpec::Docker { .. } | SourceSpec::Compose { .. } | SourceSpec::K8s { .. }
        ))
    }
}

impl From<SourceSpec> for SourceConfig {
    fn from(spec: SourceSpec) -> Self {
        Self { spec, name: None, tail: None, restart: None, scheme: None, level: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<SourceSpec, String> {
        s.parse()
    }

    #[test]
    fn bare_path_is_a_file() {
        assert_eq!(parse("a.log"), Ok(SourceSpec::File { path: "a.log".into() }));
        assert_eq!(parse("/var/log/x"), Ok(SourceSpec::File { path: "/var/log/x".into() }));
    }

    #[test]
    fn prefixes() {
        assert_eq!(parse("file:a:b.log"), Ok(SourceSpec::File { path: "a:b.log".into() }));
        assert_eq!(
            parse("cmd:ping -c 3 localhost"),
            Ok(SourceSpec::Cmd { command: "ping -c 3 localhost".into() })
        );
        assert_eq!(parse("docker:nginx"), Ok(SourceSpec::Docker { container: "nginx".into() }));
        assert_eq!(parse("-"), Ok(SourceSpec::Stdin));
    }

    #[test]
    fn compose() {
        assert_eq!(parse("compose:"), Ok(SourceSpec::Compose { services: vec![], file: None }));
        assert_eq!(
            parse("compose:web, db"),
            Ok(SourceSpec::Compose { services: vec!["web".into(), "db".into()], file: None })
        );
    }

    #[test]
    fn short_prefixes() {
        assert_eq!(parse("d:nginx"), parse("docker:nginx"));
        assert_eq!(parse("dc:web,db"), parse("compose:web,db"));
        assert_eq!(parse("dc:"), parse("compose:"));
    }

    #[test]
    fn k8s_target_then_kubectl_args() {
        let spec = parse("k8s:deploy/api -n prod --since=1h --context=kind -c app").unwrap();
        let SourceSpec::K8s { target, namespace, context, container, args, .. } = &spec else { panic!() };
        assert_eq!(target, "deploy/api");
        assert_eq!((namespace.as_deref(), context.as_deref(), container.as_deref()), (Some("prod"), Some("kind"), Some("app")));
        assert_eq!(args, &["--since=1h"]);
        assert_eq!(spec.to_string(), "k8s:deploy/api · app -n prod");
        assert!(parse("k8s:").is_err());
        assert!(parse("k8s:pod/x -n").is_err(), "a flag without its value");
    }

    #[test]
    fn journal() {
        assert_eq!(parse("journal:nginx"), Ok(SourceSpec::Journal { unit: Some("nginx".into()) }));
        assert_eq!(parse("journal:"), Ok(SourceSpec::Journal { unit: None }));
    }

    #[test]
    fn errors() {
        assert!(parse("dokcer:nginx").is_err());
        assert!(parse("cmd:").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn non_lowercase_prefix_is_a_path() {
        assert_eq!(parse("C:/x.log"), Ok(SourceSpec::File { path: "C:/x.log".into() }));
        assert_eq!(parse("./a:b"), Ok(SourceSpec::File { path: "./a:b".into() }));
    }

    #[test]
    fn restart_defaults_to_containers_only() {
        let cfg = |s: &str| SourceConfig::from(parse(s).unwrap());
        assert!(cfg("docker:db").restart());
        assert!(cfg("compose:").restart());
        assert!(cfg("k8s:pod/x").restart());
        assert!(!cfg("cmd:make").restart());
        assert!(!cfg("a.log").restart());
        assert!(SourceConfig { restart: Some(true), ..cfg("cmd:make") }.restart());
    }
}
