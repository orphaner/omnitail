//! Patterns typed by the user for search, filters and highlighting.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

/// A regex with smart case (case-insensitive unless it contains an
/// uppercase letter), optionally negated with a leading `!` (filters).
#[derive(Debug, Clone)]
pub struct Pattern {
    /// What the user typed, `!` included.
    pub text: String,
    regex: Regex,
    negated: bool,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Pattern {}

impl Pattern {
    /// `allow_negation`: whether a leading `!` means "lines not matching".
    pub fn parse(text: &str, allow_negation: bool) -> Result<Self, String> {
        let (negated, source) = match text.strip_prefix('!') {
            Some(rest) if allow_negation => (true, rest),
            _ => (false, text),
        };
        if source.is_empty() {
            return Err("empty pattern".into());
        }
        let regex = RegexBuilder::new(source)
            .case_insensitive(!source.chars().any(char::is_uppercase))
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(msg) => msg.lines().last().unwrap_or("invalid regex").trim().to_owned(),
                e => e.to_string(),
            })?;
        Ok(Self { text: text.to_owned(), regex, negated })
    }

    /// Whether a raw line passes: matches, or does not for a negated pattern.
    /// Source ANSI styling is ignored.
    pub fn accepts(&self, raw: &str) -> bool {
        let matched = if raw.contains('\x1b') {
            self.regex.is_match(&crate::ui::plain_text(raw))
        } else {
            self.regex.is_match(raw)
        };
        matched != self.negated
    }

    /// Byte ranges to highlight in displayed text (none for a negated pattern).
    pub fn find_ranges(&self, text: &str) -> Vec<Range<usize>> {
        if self.negated {
            return Vec::new();
        }
        self.regex.find_iter(text).map(|m| m.range()).filter(|r| !r.is_empty()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case() {
        let lower = Pattern::parse("error", false).unwrap();
        assert!(lower.accepts("ERROR: boom") && lower.accepts("an error"));
        let upper = Pattern::parse("Error", false).unwrap();
        assert!(upper.accepts("Error: x") && !upper.accepts("error: x"));
    }

    #[test]
    fn negation_only_where_allowed() {
        let not_info = Pattern::parse("!info", true).unwrap();
        assert!(!not_info.accepts("level=info") && not_info.accepts("level=warn"));
        assert!(not_info.find_ranges("warn").is_empty());
        let literal = Pattern::parse("!info", false).unwrap();
        assert!(literal.accepts("!info here") && !literal.accepts("info"));
    }

    #[test]
    fn ignores_source_colors_and_reports_bad_regex() {
        let p = Pattern::parse("red line", false).unwrap();
        assert!(p.accepts("\x1b[31mred\x1b[0m line"));
        assert_eq!(p.find_ranges("a red line").first(), Some(&(2..10)));
        assert!(Pattern::parse("(", false).unwrap_err().contains("unclosed"));
        assert!(Pattern::parse("", false).is_err());
        assert!(Pattern::parse("!", true).is_err());
    }
}
