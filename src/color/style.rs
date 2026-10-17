//! Style strings used by color rules: `"red bold"`, `"203"`, `"#ff5f5f"`,
//! `"fg:white bg:24 underline"`.

use ratatui::style::{Color, Modifier, Style};

/// Color names map to fixed 256-palette entries, like the rest of mtr's own
/// colors, so they look the same whatever the terminal theme.
fn named(name: &str) -> Option<Color> {
    let index = match name {
        "black" => 16,
        "red" => 203,
        "green" => 114,
        "yellow" => 221,
        "blue" => 75,
        "magenta" => 176,
        "cyan" => 80,
        "white" => 231,
        "gray" | "grey" => 245,
        "orange" => 215,
        _ => return None,
    };
    Some(Color::Indexed(index))
}

fn color(s: &str) -> Result<Color, String> {
    if let Some(c) = named(s) {
        return Ok(c);
    }
    if let Ok(index) = s.parse::<u8>() {
        return Ok(Color::Indexed(index));
    }
    if let Some(hex) = s.strip_prefix('#')
        && hex.len() == 6
        && let Ok(rgb) = u32::from_str_radix(hex, 16)
    {
        return Ok(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
    }
    Err(format!("unknown color `{s}` (a name, 0-255 or #rrggbb)"))
}

pub fn parse(s: &str) -> Result<Style, String> {
    let mut style = Style::new();
    for word in s.split_whitespace() {
        style = match word {
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "italic" => style.add_modifier(Modifier::ITALIC),
            "underline" => style.add_modifier(Modifier::UNDERLINED),
            "reverse" => style.add_modifier(Modifier::REVERSED),
            w => match w.split_once(':') {
                Some(("fg", c)) => style.fg(color(c)?),
                Some(("bg", c)) => style.bg(color(c)?),
                Some(_) => return Err(format!("unknown style `{w}`")),
                None => style.fg(color(w)?),
            },
        };
    }
    if style == Style::new() {
        return Err("empty style".into());
    }
    Ok(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_styles() {
        assert_eq!(parse("red bold"), Ok(Style::new().fg(Color::Indexed(203)).add_modifier(Modifier::BOLD)));
        assert_eq!(parse("42"), Ok(Style::new().fg(Color::Indexed(42))));
        assert_eq!(parse("#ff5f00"), Ok(Style::new().fg(Color::Rgb(255, 95, 0))));
        assert_eq!(parse("fg:white bg:24"), Ok(Style::new().fg(Color::Indexed(231)).bg(Color::Indexed(24))));
    }

    #[test]
    fn rejects_bad_styles() {
        assert!(parse("").is_err());
        assert!(parse("rouge").is_err());
        assert!(parse("300").is_err());
        assert!(parse("#12345").is_err());
        assert!(parse("xx:red").is_err());
    }
}
