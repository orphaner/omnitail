//! mtr's own coloring: regex rules grouped in schemes, built-in or from the
//! config file, applied at render time to lines the source did not color.

mod detect;
mod scheme;
mod style;

use std::sync::Arc;

pub use scheme::{Library, Scheme, SchemeDef};

/// Scheme every pane falls back to, after its specific one.
pub const FALLBACK: &str = "generic";

/// Coloring state of one pane.
pub struct PaneColor {
    pub enabled: bool,
    pub scheme: Option<Arc<Scheme>>,
    /// Short name for the title: the scheme, or the detected format.
    pub label: String,
    /// Lines kept while guessing the format; `None` once settled.
    sample: Option<Vec<String>>,
}

/// How a pane's scheme is chosen.
pub enum Choice {
    /// Guess from the first lines, then chain with the fallback.
    Auto,
    Named(Vec<String>),
    Off,
}

impl PaneColor {
    pub fn new(choice: Choice, library: &Library, enabled: bool) -> Result<Self, String> {
        Ok(match choice {
            Choice::Auto => Self {
                enabled,
                scheme: Some(library.resolve(&[FALLBACK.into()])?),
                label: "auto".into(),
                sample: Some(Vec::new()),
            },
            Choice::Named(names) => Self {
                enabled,
                scheme: Some(library.resolve(&names)?),
                label: names.join("+"),
                sample: None,
            },
            Choice::Off => Self { enabled: false, scheme: None, label: "none".into(), sample: None },
        })
    }

    /// Feeds a new line to format detection, while it is still running.
    pub fn observe(&mut self, line: &str, library: &Library) {
        let Some(sample) = &mut self.sample else { return };
        sample.push(line.to_owned());
        let guess = detect::detect(sample.iter().map(String::as_str));
        let settled = sample.len() >= detect::SAMPLE;
        if settled || guess.is_some() {
            let names: Vec<String> = guess.into_iter().chain([FALLBACK]).map(Into::into).collect();
            if let Ok(scheme) = library.resolve(&names) {
                self.scheme = Some(scheme);
            }
            self.label = guess.unwrap_or(FALLBACK).into();
        }
        if settled {
            self.sample = None;
        }
    }

    /// The scheme to render with, when coloring is on.
    pub fn active(&self) -> Option<&Scheme> {
        self.scheme.as_deref().filter(|_| self.enabled)
    }

    /// Whether `c`/`C` can do anything for this pane.
    pub fn available(&self) -> bool {
        self.scheme.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn auto_detects_then_settles() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let mut color = PaneColor::new(Choice::Auto, &lib, true).unwrap();
        assert_eq!(color.label, "auto");
        color.observe(r#"{"level":"info"}"#, &lib);
        assert_eq!(color.label, "json");
        assert_eq!(color.active().unwrap().name, "json+generic");
        for _ in 0..detect::SAMPLE {
            color.observe("plain", &lib);
        }
        // The majority is now plain text: the final guess drops json.
        assert_eq!(color.label, "generic");
        assert!(color.sample.is_none());
        color.observe(r#"{"level":"info"}"#, &lib);
        assert_eq!(color.label, "generic", "settled: no more guessing");
    }

    #[test]
    fn off_and_disabled() {
        let lib = Library::new(&HashMap::new()).unwrap();
        let off = PaneColor::new(Choice::Off, &lib, true).unwrap();
        assert!(off.active().is_none() && !off.available());
        let mut named = PaneColor::new(Choice::Named(vec!["http".into()]), &lib, true).unwrap();
        assert!(named.active().is_some());
        named.enabled = false;
        assert!(named.active().is_none());
        assert!(PaneColor::new(Choice::Named(vec!["nope".into()]), &lib, true).is_err());
    }
}
