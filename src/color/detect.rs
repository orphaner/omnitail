//! Guesses a source's log format from its first lines.

use std::sync::LazyLock;

use regex::Regex;

/// Lines looked at before the guess is final.
pub const SAMPLE: usize = 20;
/// Share of sampled lines a format must match.
const THRESHOLD: f64 = 0.6;

static HTTP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\S+ \S+ \S+ \[[^\]]+\] "[A-Z]+ \S+[^"]*" \d{3} "#).unwrap());
static SYSLOG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\w{3} [ \d]\d \d{2}:\d{2}:\d{2} \S+ [^\s:\[]+(?:\[\d+\])?: ").unwrap());
static LOGFMT_PAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:^|\s)[\w.\-]+=(?:"[^"]*"|\S*)"#).unwrap());

fn looks_json(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('{') && line.ends_with('}')
}

fn looks_logfmt(line: &str) -> bool {
    LOGFMT_PAIR.find_iter(line).count() >= 3
}

/// Name of the built-in scheme for the dominant format of `lines`, if any.
/// Lines already colored by their source are not representative and are skipped.
pub fn detect<'a>(lines: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let lines: Vec<&str> = lines.into_iter().filter(|l| !l.trim().is_empty() && !l.contains('\x1b')).collect();
    if lines.is_empty() {
        return None;
    }
    type Matcher = fn(&str) -> bool;
    let formats: [(&'static str, Matcher); 4] = [
        ("json", looks_json),
        ("http", |l| HTTP.is_match(l)),
        ("syslog", |l| SYSLOG.is_match(l)),
        ("logfmt", looks_logfmt),
    ];
    formats.into_iter().find_map(|(name, matches)| {
        let hits = lines.iter().filter(|l| matches(l)).count();
        (hits as f64 / lines.len() as f64 >= THRESHOLD).then_some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(detect([r#"{"level":"info","msg":"up"}"#, r#"{"level":"warn"}"#]), Some("json"));
        assert_eq!(
            detect([r#"10.0.0.1 - - [17/Oct/2026:21:00:00 +0200] "GET /x HTTP/1.1" 200 512 "-" "curl""#]),
            Some("http")
        );
        assert_eq!(detect(["Oct 17 21:03:04 box sshd[123]: Accepted key"]), Some("syslog"));
        assert_eq!(detect(["time=1 level=info msg=\"hello world\" user=bob"]), Some("logfmt"));
        assert_eq!(detect(["plain text line", "another one"]), None);
        assert_eq!(detect(["", "  "]), None);
    }

    #[test]
    fn needs_a_majority_and_skips_ansi_lines() {
        let mixed = [r#"{"a":1}"#, "text", "text", "text"];
        assert_eq!(detect(mixed), None);
        assert_eq!(detect(["\x1b[31mred\x1b[0m", r#"{"a":1}"#]), Some("json"));
    }
}
