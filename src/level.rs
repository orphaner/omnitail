//! Severity of log lines, read from the line itself whatever its format,
//! for the level filter (`L`/`l`).

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

/// From the least to the most severe. Fatal, critical and the like count
/// as errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// The level filter after `current` (`L`/`l`): everything, then
    /// debug and up, … error only, then everything again.
    pub fn cycle(current: Option<Level>) -> Option<Level> {
        match current {
            None => Some(Level::Debug),
            Some(Level::Trace | Level::Debug) => Some(Level::Info),
            Some(Level::Info) => Some(Level::Warn),
            Some(Level::Warn) => Some(Level::Error),
            Some(Level::Error) => None,
        }
    }

    /// Whether a line of level `line` passes a `min` filter. Lines with no
    /// known level always do: hiding what cannot be classified would lose it.
    pub fn passes(line: Option<Level>, min: Option<Level>) -> bool {
        match (line, min) {
            (Some(line), Some(min)) => line >= min,
            _ => true,
        }
    }

    /// From a level name, as logs write them (any case).
    fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "trace" | "verbose" | "finest" | "finer" => Level::Trace,
            "debug" | "dbug" | "fine" => Level::Debug,
            "info" | "information" | "informational" | "notice" | "inf" => Level::Info,
            "warn" | "warning" | "wrn" => Level::Warn,
            "error" | "err" | "eror" | "fatal" | "crit" | "critical" | "panic" | "dpanic" | "emerg" | "emergency"
            | "alert" | "severe" => Level::Error,
            _ => return None,
        })
    }

    /// From a numeric level: pino and bunyan (10 = trace … 60 = fatal),
    /// or syslog severities (0 = emergency … 7 = debug), as GELF uses.
    fn from_number(n: u32) -> Option<Self> {
        Some(match n {
            0..=3 => Level::Error,
            4 => Level::Warn,
            5 | 6 => Level::Info,
            7 => Level::Debug,
            10..=19 => Level::Trace,
            20..=29 => Level::Debug,
            30..=39 => Level::Info,
            40..=49 => Level::Warn,
            50.. => Level::Error,
            _ => return None,
        })
    }

    /// One-letter levels: klog (`E1018 …`) and Android (`E/Tag`).
    fn from_letter(letter: &str) -> Option<Self> {
        Some(match letter {
            "V" => Level::Trace,
            "D" => Level::Debug,
            "I" => Level::Info,
            "W" => Level::Warn,
            "E" | "F" => Level::Error,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Level::from_name(s).ok_or_else(|| format!("unknown level `{s}` (trace, debug, info, warn, error)"))
    }
}

impl<'de> serde::Deserialize<'de> for Level {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// A level field: JSON (`"level":"warn"`, `"level":40`, also nested as in
/// ECS `"log":{"level":…}`) or logfmt (`level=warn`).
static FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:"(?:level|lvl|severity|loglevel|log\.level|levelname)"\s*:\s*(?:"(\w+)"|(\d+))|(?:^|\s)(?:level|lvl|severity)=(?:"(\w+)"|(\w+)))"#,
    )
    .unwrap()
});

/// A level word in text logs, near the start of the line: uppercase
/// (`ERROR`, `WARN:`), or any case between brackets (`[error]`).
static WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:^|[\s\[(<|])(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRIT|CRITICAL|PANIC|SEVERE|EMERG|ALERT)(?:$|[\s\]):|,>])",
        r"|\[(?i:(trace|debug|info|notice|warn|warning|error|err|fatal|crit|critical|emerg|alert))\]",
    ))
    .unwrap()
});

/// klog (`E1018 14:25:23.123456 …`, Kubernetes components) and Android
/// logcat (`E/Tag: …`, or `… 1234 1234 E Tag: …` with threadtime).
static LETTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:([IWEF])\d{4} \d{2}:\d{2}:\d{2}|([VDIWEF])/[^\s(:]+(?:\(\s*\d+\))?:|\d{2}-\d{2} [\d:.]+\s+\d+\s+\d+ ([VDIWEF]) )")
        .unwrap()
});

/// How far into a text line a level word is looked for: past that, an
/// "error" is part of the message, not the line's level.
const WORD_WINDOW: usize = 80;

/// The level a line states, if any. `line` has no ANSI codes.
pub fn detect(line: &str) -> Option<Level> {
    if let Some(c) = FIELD.captures(line) {
        if let Some(n) = c.get(2) {
            return n.as_str().parse().ok().and_then(Level::from_number);
        }
        let name = c.get(1).or(c.get(3)).or(c.get(4)).map(|m| m.as_str())?;
        return Level::from_name(name).or_else(|| name.parse().ok().and_then(Level::from_number));
    }
    if let Some(c) = LETTER.captures(line) {
        return c.iter().skip(1).flatten().next().and_then(|m| Level::from_letter(m.as_str()));
    }
    let mut end = WORD_WINDOW.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let c = WORD.captures(&line[..end])?;
    Level::from_name(c.get(1).or(c.get(2))?.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Level::*;

    #[test]
    fn structured_fields() {
        assert_eq!(detect(r#"{"time":"x","level":"error","msg":"boom"}"#), Some(Error));
        assert_eq!(detect(r#"{"lvl":"WARNING"}"#), Some(Warn));
        assert_eq!(detect(r#"{"level":30,"msg":"pino"}"#), Some(Info));
        assert_eq!(detect(r#"{"level":"50"}"#), Some(Error), "pino as a string");
        assert_eq!(detect(r#"{"severity":3,"short_message":"gelf"}"#), Some(Error), "syslog scale");
        assert_eq!(detect(r#"{"@timestamp":"x","log":{"level":"debug"},"message":"ecs"}"#), Some(Debug));
        assert_eq!(detect(r#"{"log.level":"info"}"#), Some(Info));
        assert_eq!(detect(r#"time=x level=warn msg="job processed" job=1"#), Some(Warn));
        assert_eq!(detect(r#"lvl="error" msg=x"#), Some(Error));
        assert_eq!(detect(r#"{"level":"bogus","msg":"ERROR in text"}"#), None, "a level field decides");
    }

    #[test]
    fn text_logs() {
        assert_eq!(detect("2026-10-18 14:25:23.123 ERROR [main] c.f.App - boom"), Some(Error));
        assert_eq!(detect("2026-10-18T14:25:23Z  WARN omnitail::source: slow"), Some(Warn));
        assert_eq!(detect("2026/10/18 14:25:23 [error] 12#0: *1 open() failed"), Some(Error), "nginx error log");
        assert_eq!(detect("[Sat Oct 18 14:25:23 2026] [notice] Apache configured"), Some(Info));
        assert_eq!(detect("WARNING:root:disk almost full"), Some(Warn), "python logging");
        assert_eq!(detect("2026-10-18 14:25:23 UTC [1] FATAL:  role does not exist"), Some(Error), "postgres");
        assert_eq!(detect("E1018 14:25:23.123456    1 reflector.go:1] failed"), Some(Error), "klog");
        assert_eq!(detect("I1018 14:25:23.123456    1 leader.go:1] ok"), Some(Info));
        assert_eq!(detect("W/ActivityManager( 123): slow"), Some(Warn), "android");
        assert_eq!(detect("10-18 14:25:23.123  1234  1234 D Tag: hi"), Some(Debug), "android threadtime");
    }

    #[test]
    fn no_level() {
        assert_eq!(detect("web request 15795"), None);
        assert_eq!(detect("Information about errors follows"), None, "words in a sentence are not levels");
        assert_eq!(detect("    at com.foo.Bar.baz(Bar.java:42)"), None);
        assert_eq!(detect("I/O error on /dev/sda"), None, "not an android tag");
        let far = format!("{} ERROR", "x ".repeat(60));
        assert_eq!(detect(&far), None, "too far into the line");
        assert_eq!(detect(r#"10.0.0.1 - - [18/Oct/2026:14:25:25 +0000] "GET / HTTP/1.1" 500 1"#), None);
        assert_eq!(detect("é".repeat(100).as_str()), None, "cut on a char boundary");
    }

    #[test]
    fn cycle_and_passes() {
        let mut level = None;
        let mut seen = Vec::new();
        for _ in 0..5 {
            level = Level::cycle(level);
            seen.push(level);
        }
        assert_eq!(seen, [Some(Debug), Some(Info), Some(Warn), Some(Error), None]);
        assert!(Level::passes(Some(Error), Some(Warn)));
        assert!(!Level::passes(Some(Info), Some(Warn)));
        assert!(Level::passes(None, Some(Error)), "unknown levels stay visible");
        assert!(Level::passes(Some(Trace), None));
        assert_eq!("WARNING".parse::<Level>(), Ok(Warn));
        assert!("loud".parse::<Level>().is_err());
    }
}
