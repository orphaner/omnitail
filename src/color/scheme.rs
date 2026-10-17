use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use regex::{Regex, RegexSet};
use serde::Deserialize;

use super::style;

/// A rule as written in TOML: `{ regex = '…', group = 1, style = "red bold" }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    pub regex: String,
    /// Capture group to color; 0 (default) is the whole match.
    #[serde(default)]
    pub group: usize,
    pub style: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemeDef {
    pub rules: Vec<RuleDef>,
}

#[derive(Debug, Clone)]
struct Rule {
    regex: Regex,
    group: usize,
    style: Style,
}

/// An ordered list of rules. Where two rules match the same text, the
/// earlier one wins.
#[derive(Debug)]
pub struct Scheme {
    pub name: String,
    rules: Vec<Rule>,
    /// Tells which rules match at all, in one pass, before running them.
    set: RegexSet,
}

impl Scheme {
    pub fn compile(name: &str, def: &SchemeDef) -> Result<Self, String> {
        let rules = def
            .rules
            .iter()
            .map(|r| {
                let regex = Regex::new(&r.regex).map_err(|e| format!("scheme `{name}`: {e}"))?;
                if r.group >= regex.captures_len() {
                    return Err(format!("scheme `{name}`: no group {} in `{}`", r.group, r.regex));
                }
                let style = style::parse(&r.style).map_err(|e| format!("scheme `{name}`: {e}"))?;
                Ok(Rule { regex, group: r.group, style })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self::from_rules(name.to_owned(), rules))
    }

    fn from_rules(name: String, rules: Vec<Rule>) -> Self {
        let set = RegexSet::new(rules.iter().map(|r| r.regex.as_str())).expect("rules compiled one by one");
        Self { name, rules, set }
    }

    /// Rules of `schemes` one after the other: the first scheme has priority.
    fn chain(schemes: &[Arc<Scheme>]) -> Self {
        let name = schemes.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("+");
        Self::from_rules(name, schemes.iter().flat_map(|s| s.rules.iter().cloned()).collect())
    }

    /// Styled byte ranges of `text`, in order and without overlap.
    pub fn spans(&self, text: &str) -> Vec<(Range<usize>, Style)> {
        const FREE: usize = usize::MAX;
        let mut owner = vec![FREE; text.len()];
        for i in self.set.matches(text).iter() {
            let rule = &self.rules[i];
            let mut claim = |range: Range<usize>| {
                for o in &mut owner[range] {
                    if *o == FREE {
                        *o = i;
                    }
                }
            };
            if rule.group == 0 {
                rule.regex.find_iter(text).for_each(|m| claim(m.range()));
            } else {
                for caps in rule.regex.captures_iter(text) {
                    if let Some(m) = caps.get(rule.group) {
                        claim(m.range());
                    }
                }
            }
        }

        let mut spans = Vec::new();
        let mut start = 0;
        for end in 1..=owner.len() {
            if end == owner.len() || owner[end] != owner[start] {
                if owner[start] != FREE {
                    spans.push((start..end, self.rules[owner[start]].style));
                }
                start = end;
            }
        }
        spans
    }

    pub fn line(&self, text: &str) -> Line<'static> {
        let mut out = Vec::new();
        let mut pos = 0;
        for (range, style) in self.spans(text) {
            if range.start > pos {
                out.push(Span::raw(text[pos..range.start].to_owned()));
            }
            out.push(Span::styled(text[range.clone()].to_owned(), style));
            pos = range.end;
        }
        if pos < text.len() {
            out.push(Span::raw(text[pos..].to_owned()));
        }
        Line::from(out)
    }
}

#[derive(Deserialize)]
struct SchemeFile {
    scheme: HashMap<String, SchemeDef>,
}

/// Every scheme mtr knows: the built-in ones, then the user's (which can
/// replace a built-in one by using its name).
pub struct Library {
    schemes: HashMap<String, Arc<Scheme>>,
}

/// Built-in schemes, in the same format as user config files.
const BUILTIN: &str = include_str!("builtin.toml");

impl Library {
    pub fn new(user: &HashMap<String, SchemeDef>) -> Result<Self, String> {
        let builtin: SchemeFile = toml::from_str(BUILTIN).map_err(|e| format!("built-in schemes: {e}"))?;
        let mut schemes = HashMap::new();
        for (name, def) in builtin.scheme.iter().chain(user) {
            schemes.insert(name.clone(), Arc::new(Scheme::compile(name, def)?));
        }
        Ok(Self { schemes })
    }

    /// One scheme, or several chained in priority order.
    pub fn resolve(&self, names: &[String]) -> Result<Arc<Scheme>, String> {
        let schemes = names
            .iter()
            .map(|n| {
                self.schemes.get(n).cloned().ok_or_else(|| {
                    let mut known: Vec<_> = self.schemes.keys().map(String::as_str).collect();
                    known.sort();
                    format!("unknown color scheme `{n}` (known: {})", known.join(", "))
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        match &schemes[..] {
            [] => Err("no color scheme given".into()),
            [one] => Ok(one.clone()),
            _ => Ok(Arc::new(Scheme::chain(&schemes))),
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;

    fn scheme(rules: &[(&str, usize, &str)]) -> Scheme {
        let def = SchemeDef {
            rules: rules
                .iter()
                .map(|(regex, group, style)| RuleDef { regex: regex.to_string(), group: *group, style: style.to_string() })
                .collect(),
        };
        Scheme::compile("test", &def).unwrap()
    }

    fn red() -> Style {
        Style::new().fg(Color::Indexed(203))
    }

    #[test]
    fn colors_matches_and_groups() {
        let s = scheme(&[(r"user=(\w+)", 1, "red")]);
        assert_eq!(s.spans("a user=bob b"), [(7..10, red())]);
        let line = s.line("a user=bob b");
        assert_eq!(line.to_string(), "a user=bob b");
        assert_eq!(line.spans[1].content, "bob");
    }

    #[test]
    fn first_rule_wins_on_overlap() {
        let s = scheme(&[(r"ERROR", 0, "red"), (r"[A-Z]+ \d+", 0, "blue")]);
        let blue = Style::new().fg(Color::Indexed(75));
        assert_eq!(s.spans("ERROR 42"), [(0..5, red()), (5..8, blue)]);
    }

    #[test]
    fn handles_multibyte_text() {
        let s = scheme(&[(r"été", 0, "red")]);
        assert_eq!(s.line("un été chaud").spans[1].content, "été");
    }

    #[test]
    fn compile_errors() {
        let def = |regex: &str, group, style: &str| SchemeDef {
            rules: vec![RuleDef { regex: regex.into(), group, style: style.into() }],
        };
        assert!(Scheme::compile("x", &def("(", 0, "red")).is_err());
        assert!(Scheme::compile("x", &def("a", 1, "red")).is_err());
        assert!(Scheme::compile("x", &def("a", 0, "rouge")).is_err());
    }

    #[test]
    fn builtin_schemes_compile_and_user_ones_override() {
        let lib = Library::new(&HashMap::new()).unwrap();
        for name in ["generic", "http", "json", "logfmt", "syslog"] {
            assert!(lib.resolve(&[name.into()]).is_ok(), "{name}");
        }
        let user = HashMap::from([(
            "generic".to_string(),
            SchemeDef { rules: vec![RuleDef { regex: "x".into(), group: 0, style: "bold".into() }] },
        )]);
        let lib = Library::new(&user).unwrap();
        let generic = lib.resolve(&["generic".into()]).unwrap();
        assert_eq!(generic.spans("x"), [(0..1, Style::new().add_modifier(Modifier::BOLD))]);
        assert!(lib.resolve(&["nope".into()]).unwrap_err().contains("known: "));
    }

    #[test]
    fn chain_keeps_priority_order() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let chained = lib.resolve(&["json".into(), "generic".into()]).unwrap();
        assert_eq!(chained.name, "json+generic");
    }
}
