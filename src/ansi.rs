use regex::Regex;
use std::sync::LazyLock;

static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("valid ansi regex"));

/// Strip ANSI escape sequences (Phase 1: plain text only).
pub fn strip_ansi(s: &str) -> String {
    ANSI_RE.replace_all(s, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn test_strip_ansi_basic() {
        let input = "\x1b[31mRed Text\x1b[0m";
        assert_eq!(strip_ansi(input), "Red Text");
    }

    #[test]
    fn test_strip_ansi_multiple() {
        let input = "\x1b[1;32mBold Green\x1b[0m Normal \x1b[4;33mUnderline Yellow\x1b[0m";
        assert_eq!(
            strip_ansi(input),
            "Bold Green Normal Underline Yellow"
        );
    }

    #[test]
    fn test_strip_ansi_no_codes() {
        let input = "Plain text without codes";
        assert_eq!(strip_ansi(input), "Plain text without codes");
    }

    #[test]
    fn test_strip_ansi_tmux_sample() {
        let input = "2026-05-25 \x1b[1;32m[shogun]\x1b[0m READY";
        assert_eq!(strip_ansi(input), "2026-05-25 [shogun] READY");
    }

    #[test]
    fn test_strip_ansi_cursor_and_clear() {
        let input = "line\x1b[2K\x1b[1G";
        assert_eq!(strip_ansi(input), "line");
    }
}
