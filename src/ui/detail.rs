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

/// A row of the pretty-printed JSON: JSON itself, or the text of a
/// multi-line message unfolded (a stack trace), colored as the pane's lines.
#[derive(Debug, PartialEq, Eq)]
enum Row {
    Json(String),
    Text(String),
}

/// Fields whose multi-line values are unfolded: `\n`, `\r\n` and `\t`
/// become real line breaks and indents. Display only, not valid JSON.
fn unfolds(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(key.as_str(), "msg" | "message")
        || ["exception", "stack", "trace", "error"].iter().any(|k| key.contains(k))
}

/// Like `serde_json::to_string_pretty` (2-space indent, key order kept),
/// except for the multi-line strings of the fields `unfolds` picks.
fn pretty(value: &Value) -> Vec<Row> {
    let mut rows = Vec::new();
    write(&mut rows, 0, None, value, "", false);
    rows
}

fn write(rows: &mut Vec<Row>, indent: usize, key: Option<&str>, value: &Value, comma: &str, unfold: bool) {
    let pad = " ".repeat(indent);
    let key_part = key.map(|k| format!("{}: ", serde_json::to_string(k).unwrap_or_default())).unwrap_or_default();
    let unfold = unfold || key.is_some_and(unfolds);
    match value {
        Value::Object(map) if !map.is_empty() => {
            rows.push(Row::Json(format!("{pad}{key_part}{{")));
            for (i, (k, v)) in map.iter().enumerate() {
                write(rows, indent + 2, Some(k), v, if i + 1 < map.len() { "," } else { "" }, false);
            }
            rows.push(Row::Json(format!("{pad}}}{comma}")));
        }
        Value::Array(items) if !items.is_empty() => {
            rows.push(Row::Json(format!("{pad}{key_part}[")));
            for (i, v) in items.iter().enumerate() {
                write(rows, indent + 2, None, v, if i + 1 < items.len() { "," } else { "" }, unfold);
            }
            rows.push(Row::Json(format!("{pad}]{comma}")));
        }
        Value::String(s) if unfold && s.contains(['\n', '\r', '\t']) => {
            let text = s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
            let lines: Vec<&str> = text.lines().collect();
            let last = lines.len().saturating_sub(1);
            for (i, line) in lines.iter().enumerate() {
                let end = if i == last { format!("\"{comma}") } else { String::new() };
                rows.push(match i {
                    0 => Row::Json(format!("{pad}{key_part}\"{line}{end}")),
                    // Continuation lines, under the key.
                    _ => Row::Text(format!("{pad}    {line}{end}")),
                });
            }
        }
        _ => rows.push(Row::Json(format!("{pad}{key_part}{}{comma}", serde_json::to_string(value).unwrap_or_default()))),
    }
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
    let filter = d.back.filter.as_ref();
    let render = Render { width: body.width as usize, strip, scheme, wrap: true, hscroll: 0, highlight, filter };

    let (rows, kind) = match pane.buffer.get(d.seq) {
        None => (vec![Line::styled("(this line is no longer in history)", super::dim())], ""),
        Some(raw) => match split_json(&super::plain_text(raw)) {
            Some((prefix, value)) => {
                let mut rows = if prefix.is_empty() { Vec::new() } else { render.rows(prefix) };
                let json_render = Render { scheme: json, ..render };
                rows.extend(pretty(&value).into_iter().flat_map(|row| match row {
                    Row::Json(l) => json_render.rows(&l),
                    Row::Text(l) => render.rows(&l),
                }));
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

    fn text(rows: Vec<Row>) -> String {
        let lines: Vec<String> = rows
            .into_iter()
            .map(|r| match r {
                Row::Json(l) | Row::Text(l) => l,
            })
            .collect();
        lines.join("\n")
    }

    #[test]
    fn pretty_prints_like_serde_keeping_key_order() {
        for json in [r#"{"z":1,"a":2}"#, r#"{"a":{"b":[1,"x\ny",{}],"c":[]},"msg":"one line","n":null}"#, r#"[true,{"k":"v"}]"#] {
            let v: Value = serde_json::from_str(json).unwrap();
            assert_eq!(text(pretty(&v)), serde_json::to_string_pretty(&v).unwrap(), "{json}");
        }
    }

    #[test]
    fn unfolds_multi_line_messages_and_traces() {
        let v: Value = serde_json::from_str(
            r#"{"level":"error","message":"boom\r\nsecond","error":{"stack_trace":"java.lang.X: y\n\tat a.B.c(B.java:1)"},"other":"a\nb"}"#,
        )
        .unwrap();
        let rows = pretty(&v);
        assert_eq!(
            text(rows),
            [
                "{",
                r#"  "level": "error","#,
                r#"  "message": "boom"#,
                r#"      second","#,
                r#"  "error": {"#,
                r#"    "stack_trace": "java.lang.X: y"#,
                r#"            at a.B.c(B.java:1)""#,
                "  },",
                r#"  "other": "a\nb""#,
                "}",
            ]
            .join("\n")
        );
        assert!(matches!(&pretty(&v)[3], Row::Text(_)), "unfolded lines are colored as text");
        let tab: Value = serde_json::from_str(r#"{"msg":"a\tb"}"#).unwrap();
        assert_eq!(text(pretty(&tab)), "{\n  \"msg\": \"a    b\"\n}", "one line stays one row");
    }
}
