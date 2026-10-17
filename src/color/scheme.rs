use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

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
    pub description: Option<String>,
    /// Regexes on file paths: files matching one get this scheme by default.
    #[serde(default)]
    pub paths: Vec<String>,
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

/// Schemes of an embedded file, in the order they are written.
#[derive(Deserialize)]
struct SchemeFile {
    scheme: toml::Table,
}

/// Where a scheme comes from, in increasing priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Multitail,
    Mtr,
    Config,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Multitail => "multitail",
            Origin::Mtr => "mtr",
            Origin::Config => "config",
        }
    }
}

pub struct SchemeInfo {
    pub origin: Origin,
    pub description: Option<String>,
}

/// Every scheme mtr knows: converted from multitail, then mtr's own, then
/// the user's. A later one replaces an earlier one of the same name, except
/// that a multitail scheme shadowed by one of mtr's stays available as
/// `<name>-multitail`.
pub struct Library {
    schemes: HashMap<String, Slot>,
    info: BTreeMap<String, SchemeInfo>,
    /// File path regexes, in loading order.
    paths: Vec<(Regex, String, Origin)>,
}

/// A scheme compiled on first use: hundreds of multitail rules would
/// otherwise be compiled at every start for nothing.
struct Slot {
    def: SchemeDef,
    compiled: OnceLock<Result<Arc<Scheme>, String>>,
}

impl Slot {
    fn get(&self, name: &str) -> Result<Arc<Scheme>, String> {
        self.compiled.get_or_init(|| Scheme::compile(name, &self.def).map(Arc::new)).clone()
    }
}

/// mtr's own schemes, in the same format as user config files.
const BUILTIN: &str = include_str!("builtin.toml");
/// Schemes converted from multitail (scripts/convert-multitail.py).
const MULTITAIL: &str = include_str!("multitail.toml");

impl Library {
    pub fn new(user: &HashMap<String, SchemeDef>) -> Result<Self, String> {
        let mut lib = Self { schemes: HashMap::new(), info: BTreeMap::new(), paths: Vec::new() };
        for (text, origin) in [(MULTITAIL, Origin::Multitail), (BUILTIN, Origin::Mtr)] {
            let file: SchemeFile = toml::from_str(text).map_err(|e| format!("{} schemes: {e}", origin.label()))?;
            for (name, value) in file.scheme {
                let def: SchemeDef = value.try_into().map_err(|e| format!("{} scheme `{name}`: {e}", origin.label()))?;
                lib.add(&name, &def, origin)?;
            }
        }
        for (name, def) in user.iter().collect::<BTreeMap<_, _>>() {
            lib.add(name, def, Origin::Config)?;
        }
        Ok(lib)
    }

    fn add(&mut self, name: &str, def: &SchemeDef, origin: Origin) -> Result<(), String> {
        if origin == Origin::Mtr && self.info.get(name).is_some_and(|i| i.origin == Origin::Multitail) {
            self.rename(name, &format!("{name}-multitail"));
        }
        for path in &def.paths {
            let regex = Regex::new(path).map_err(|e| format!("scheme `{name}`: path {e}"))?;
            self.paths.push((regex, name.to_owned(), origin));
        }
        let slot = Slot { def: def.clone(), compiled: OnceLock::new() };
        // Ours and the user's are checked now, so config errors show at start;
        // multitail's are checked by the test suite.
        if origin != Origin::Multitail {
            slot.get(name)?;
        }
        self.schemes.insert(name.to_owned(), slot);
        self.info.insert(name.to_owned(), SchemeInfo { origin, description: def.description.clone() });
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) {
        if let Some(slot) = self.schemes.remove(from) {
            self.schemes.insert(to.to_owned(), Slot { def: slot.def, compiled: OnceLock::new() });
        }
        if let Some(info) = self.info.remove(from) {
            self.info.insert(to.to_owned(), info);
        }
        for (_, name, _) in self.paths.iter_mut().filter(|(_, n, _)| n == from) {
            *name = to.to_owned();
        }
    }

    /// The scheme files at `path` get by default, if any: the user's over
    /// mtr's over multitail's, then the first declared (as in multitail).
    pub fn for_path(&self, path: &str) -> Option<&str> {
        let matches = self.paths.iter().filter(|(r, _, _)| r.is_match(path));
        let best = matches.fold(None, |best: Option<&(Regex, String, Origin)>, m| match best {
            Some(b) if b.2 >= m.2 => Some(b),
            _ => Some(m),
        });
        best.map(|(_, name, _)| name.as_str())
    }

    /// All schemes, by name.
    pub fn list(&self) -> impl Iterator<Item = (&str, &SchemeInfo)> {
        self.info.iter().map(|(n, i)| (n.as_str(), i))
    }

    /// One scheme, or several chained in priority order.
    pub fn resolve(&self, names: &[String]) -> Result<Arc<Scheme>, String> {
        let schemes = names
            .iter()
            .map(|n| {
                self.schemes
                    .get(n)
                    .ok_or_else(|| format!("unknown color scheme `{n}` (see mtr --list-schemes)"))?
                    .get(n)
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
            description: None,
            paths: vec![],
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
            description: None,
            paths: vec![],
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
            SchemeDef {
                description: None,
                paths: vec![],
                rules: vec![RuleDef { regex: "x".into(), group: 0, style: "bold".into() }],
            },
        )]);
        let lib = Library::new(&user).unwrap();
        let generic = lib.resolve(&["generic".into()]).unwrap();
        assert_eq!(generic.spans("x"), [(0..1, Style::new().add_modifier(Modifier::BOLD))]);
        assert!(lib.resolve(&["nope".into()]).unwrap_err().contains("--list-schemes"));
    }

    #[test]
    fn multitail_schemes_all_compile_with_paths() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let from_multitail: Vec<&str> = lib.list().filter(|(_, i)| i.origin == Origin::Multitail).map(|(n, _)| n).collect();
        assert_eq!(from_multitail.len(), 62);
        for name in from_multitail {
            if let Err(e) = lib.resolve(&[name.into()]) {
                panic!("{e}");
            }
        }
        // Shadowed by mtr's own syslog, still reachable.
        assert_eq!(lib.list().find(|(n, _)| *n == "syslog").unwrap().1.origin, Origin::Mtr);
        assert!(lib.resolve(&["syslog-multitail".into()]).is_ok());
        assert!(lib.resolve(&["uucp".into()]).is_err(), "excluded");

        assert_eq!(lib.for_path("/var/log/mail/mail.log"), Some("postfix"), "first of postfix/sendmail/exim");
        assert_eq!(lib.for_path("/var/log/fail2ban.log"), Some("f2b"));
        assert_eq!(lib.for_path("/tmp/app.log"), None);
    }

    #[test]
    fn multitail_schemes_color_real_lines() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let colored = |scheme: &str, line: &str| {
            let s = lib.resolve(&[scheme.into()]).unwrap();
            s.spans(line).iter().map(|(r, _)| line[r.clone()].to_string()).collect::<Vec<_>>()
        };
        let ssh = colored("ssh", "Oct 17 21:03:04 box sshd[123]: Failed password for root from 10.0.0.1 port 22 ssh2");
        assert!(!ssh.is_empty(), "{ssh:?}");
        let f2b = colored("f2b", "2026-10-17 21:00:00,123 fail2ban.actions [1]: NOTICE [sshd] Ban 10.0.0.1");
        assert!(f2b.iter().any(|s| s.contains("Ban")), "{f2b:?}");
    }

    #[test]
    fn chain_keeps_priority_order() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let chained = lib.resolve(&["json".into(), "generic".into()]).unwrap();
        assert_eq!(chained.name, "json+generic");
    }
}
