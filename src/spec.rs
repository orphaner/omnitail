//! Declarative description of a source, shared by the CLI and (later) config files.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceSpec {
    File { path: PathBuf },
    Cmd { command: String },
    Docker { container: String },
}

impl FromStr for SourceSpec {
    type Err = String;

    /// Parses `file:<path>`, `cmd:<command>` or `docker:<container>`.
    /// An argument without a known prefix is a file path.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((prefix, rest)) = s.split_once(':') else {
            return non_empty(s).map(|p| SourceSpec::File { path: p.into() });
        };
        match prefix {
            "file" => non_empty(rest).map(|p| SourceSpec::File { path: p.into() }),
            "cmd" => non_empty(rest).map(|c| SourceSpec::Cmd { command: c.into() }),
            "docker" => non_empty(rest).map(|c| SourceSpec::Docker { container: c.into() }),
            // Something like `dokcer:x` is far more likely a typo than a file name.
            p if !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()) => Err(format!(
                "unknown source type `{p}` (expected file, cmd or docker; \
                 use `file:{s}` for a path containing a colon)"
            )),
            _ => Ok(SourceSpec::File { path: s.into() }),
        }
    }
}

fn non_empty(s: &str) -> Result<&str, String> {
    if s.trim().is_empty() {
        Err("empty source".into())
    } else {
        Ok(s)
    }
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceSpec::File { path } => write!(f, "file:{}", path.display()),
            SourceSpec::Cmd { command } => write!(f, "cmd:{command}"),
            SourceSpec::Docker { container } => write!(f, "docker:{container}"),
        }
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
    fn deserializes_from_tagged_map() {
        // Shape a config file loader will feed in.
        let spec: SourceSpec = serde::Deserialize::deserialize(
            serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(
                [("type", "docker"), ("container", "db")].into_iter(),
            ),
        )
        .unwrap();
        assert_eq!(spec, SourceSpec::Docker { container: "db".into() });
    }
}
