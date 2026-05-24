use regex::Regex;
use std::sync::LazyLock;

static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("valid ansi regex"));

/// Strip all ANSI escape sequences (used for error messages, not for display).
pub fn strip_ansi(s: &str) -> String {
    ANSI_RE.replace_all(s, "").into_owned()
}

/// A run of text with an optional RGB foreground color.
#[derive(Debug, Clone)]
pub struct AnsiSpan {
    pub text: String,
    /// None = use the default display color
    pub rgb: Option<(u8, u8, u8)>,
}

/// Parse ANSI-escaped text into per-line spans.
/// One `Vec<AnsiSpan>` per line; newlines are consumed as separators.
pub fn parse_ansi_spans(s: &str) -> Vec<Vec<AnsiSpan>> {
    let mut lines: Vec<Vec<AnsiSpan>> = Vec::new();
    let mut cur_line: Vec<AnsiSpan> = Vec::new();
    let mut cur_color: Option<(u8, u8, u8)> = None;
    let mut buf = String::new();

    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            flush_span(&mut buf, cur_color, &mut cur_line);
            chars.next(); // consume '['
            let mut params = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == ';' {
                    params.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(&final_byte) = chars.peek() {
                chars.next();
                if final_byte == 'm' {
                    cur_color = apply_sgr(&params, cur_color);
                }
            }
        } else if ch == '\n' {
            flush_span(&mut buf, cur_color, &mut cur_line);
            lines.push(std::mem::take(&mut cur_line));
        } else if ch != '\r' {
            buf.push(ch);
        }
    }
    flush_span(&mut buf, cur_color, &mut cur_line);
    if !cur_line.is_empty() {
        lines.push(cur_line);
    }
    lines
}

fn flush_span(buf: &mut String, color: Option<(u8, u8, u8)>, line: &mut Vec<AnsiSpan>) {
    if !buf.is_empty() {
        line.push(AnsiSpan {
            text: std::mem::take(buf),
            rgb: color,
        });
    }
}

fn apply_sgr(params: &str, current: Option<(u8, u8, u8)>) -> Option<(u8, u8, u8)> {
    if params.is_empty() {
        return None;
    }
    let parts: Vec<u32> = params.split(';').filter_map(|p| p.parse().ok()).collect();
    if parts.is_empty() || (parts.len() == 1 && parts[0] == 0) {
        return None;
    }
    let bold = parts.iter().any(|&p| p == 1);
    let mut result = current;
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            0 => result = None,
            1 => {}
            30..=37 => result = Some(ansi_color(parts[i] - 30, bold)),
            90..=97 => result = Some(ansi_color(parts[i] - 90, true)),
            38 if i + 1 < parts.len() => {
                match parts[i + 1] {
                    2 if i + 4 < parts.len() => {
                        result = Some((parts[i + 2] as u8, parts[i + 3] as u8, parts[i + 4] as u8));
                        i += 4;
                    }
                    5 if i + 2 < parts.len() => {
                        result = Some(color256(parts[i + 2] as u8));
                        i += 2;
                    }
                    _ => {}
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    result
}

fn ansi_color(n: u32, bright: bool) -> (u8, u8, u8) {
    if bright {
        match n {
            0 => (0x55, 0x57, 0x53),
            1 => (0xef, 0x29, 0x29),
            2 => (0x8a, 0xe2, 0x34),
            3 => (0xfc, 0xe9, 0x4f),
            4 => (0x72, 0x9f, 0xcf),
            5 => (0xad, 0x7f, 0xa8),
            6 => (0x34, 0xe2, 0xe2),
            _ => (0xee, 0xee, 0xec),
        }
    } else {
        match n {
            0 => (0x1e, 0x1e, 0x1e),
            1 => (0xcc, 0x00, 0x00),
            2 => (0x4e, 0x9a, 0x06),
            3 => (0xc4, 0xa0, 0x00),
            4 => (0x34, 0x65, 0xa4),
            5 => (0x75, 0x50, 0x7b),
            6 => (0x06, 0x98, 0x9a),
            _ => (0xd3, 0xd7, 0xcf),
        }
    }
}

fn color256(n: u8) -> (u8, u8, u8) {
    match n {
        0..=7 => ansi_color(n as u32, false),
        8..=15 => ansi_color(n as u32 - 8, true),
        16..=231 => {
            let n = n - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let v = |x: u8| if x == 0 { 0u8 } else { x * 40 + 55 };
            (v(r), v(g), v(b))
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mRed Text\x1b[0m"), "Red Text");
    }

    #[test]
    fn test_parse_spans_basic() {
        let spans = parse_ansi_spans("\x1b[32mgreen\x1b[0m plain");
        assert_eq!(spans.len(), 1);
        let line = &spans[0];
        assert_eq!(line[0].text, "green");
        assert!(line[0].rgb.is_some());
        assert_eq!(line[1].text, " plain");
        assert!(line[1].rgb.is_none());
    }

    #[test]
    fn test_parse_spans_multiline() {
        let spans = parse_ansi_spans("line1\nline2\n");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0][0].text, "line1");
        assert_eq!(spans[1][0].text, "line2");
    }
}
