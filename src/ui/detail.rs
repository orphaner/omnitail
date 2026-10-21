use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use serde::Deserialize;
use serde_json::Value;

use super::text::Render;
use crate::app::{Detail, Pane};
use crate::color::Scheme;

/// Splits `text` into a prefix and a JSON object or array that runs to the
/// end of the line: `{…}`, but also `[pod/x] {…}` or `2026-10-17T… {…}`.
pub fn split_json(text: &str) -> Option<(&str, Value)> {
    // Only the first few candidates: a line is not worth more parsing.
    for (i, _) in text.match_indices(['{', '[']).take(8) {
        let mut de = serde_json::Deserializer::from_str(&text[i..]);
        if let Ok(value) = Value::deserialize(&mut de)
            && de.end().is_ok()
            && (value.is_object() || value.is_array())
        {
            return Some((text[..i].trim_end(), value));
        }
    }
    None
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    d: &mut Detail,
    pane: &Pane,
    strip: bool,
    json: Option<&Scheme>,
    highlight: Option<&crate::pattern::Pattern>,
) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let scheme = pane.buffer.entry(d.seq).map_or(pane.color.active(), |e| pane.scheme_for(e));
    let render = Render { width: body.width as usize, strip, scheme, wrap: true, hscroll: 0, highlight };

    let (rows, kind) = match pane.buffer.get(d.seq) {
        None => (vec![Line::styled("(this line is no longer in history)", super::dim())], ""),
        Some(raw) => match split_json(&super::plain_text(raw)) {
            Some((prefix, value)) => {
                let mut rows = if prefix.is_empty() { Vec::new() } else { render.rows(prefix) };
                let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
                let json_render = Render { scheme: json, ..render };
                rows.extend(pretty.lines().flat_map(|l| json_render.rows(l)));
                (rows, " · json")
            }
            None => (render.rows(raw), ""),
        },
    };

    let height = body.height as usize;
    d.max_scroll = rows.len().saturating_sub(height);
    d.scroll = d.scroll.min(d.max_scroll);
    d.page = height;

    let more = if d.max_scroll > 0 { format!(" · {}/{} rows", d.scroll + height.min(rows.len()), rows.len()) } else { String::new() };
    let right = [format!("line {}{kind}{more}", d.seq + 1), format!("line {}", d.seq + 1)];
    super::render_title(frame, title, d.pane, pane, &right);
    frame.render_widget(Paragraph::new(rows).scroll((d.scroll as u16, 0)), body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_json_alone_or_after_a_prefix() {
        let (prefix, v) = split_json(r#"{"a":1,"b":[true]}"#).unwrap();
        assert_eq!((prefix, v["a"].as_i64()), ("", Some(1)));

        let (prefix, v) = split_json(r#"[pod/api-1/app] {"level":"info"}"#).unwrap();
        assert_eq!((prefix, v["level"].as_str()), ("[pod/api-1/app]", Some("info")));

        let (prefix, _) = split_json(r#"2026-10-17T12:00:00Z INFO {"x":{"y":2}}"#).unwrap();
        assert_eq!(prefix, "2026-10-17T12:00:00Z INFO");
    }

    #[test]
    fn rejects_non_json_and_trailing_text() {
        assert!(split_json("[INFO] plain text").is_none());
        assert!(split_json(r#"{"a":1} trailing"#).is_none());
        assert!(split_json("42").is_none(), "scalars are not worth a detail view");
    }

    #[test]
    fn keeps_key_order_when_pretty_printing() {
        let (_, v) = split_json(r#"{"z":1,"a":2}"#).unwrap();
        assert_eq!(serde_json::to_string_pretty(&v).unwrap(), "{\n  \"z\": 1,\n  \"a\": 2\n}");
    }
}
