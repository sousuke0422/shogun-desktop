//! OSC 9 / OSC 777 desktop notifications (Ghostty-compatible behavior).
//!
//! Sequences:
//! - `ESC ] 9 ; <body> (BEL | ESC \)` — iTerm2-style, body only (Claude Code
//!   sends exactly this; it does not use OSC 777)
//! - `ESC ] 777 ; notify ; <title> ; <body> (BEL | ESC \)` — urxvt extension
//!
//! Ghostty parity notes (src/terminal/osc.zig + apprt handlers):
//! - OSC 9 payloads whose first field is a ConEmu subcommand number are NOT
//!   notifications (9;4 = progress, handled by [`super::progress`]; 9;1–9;10
//!   others are ConEmu commands we ignore). Anything else after `9;` is the
//!   notification body verbatim, semicolons included.
//! - Title is truncated to 63 bytes and body to 255 bytes (Ghostty's fixed
//!   apprt buffers), on UTF-8 character boundaries.
//! - Deviation: Ghostty falls back to a notification when a ConEmu payload is
//!   malformed; we drop those instead — progress emitters are machine
//!   generated and a toast per malformed update would be noise.
//!
//! Focus suppression (the "Ghostty っぽい挙動" the Lord asked for) lives in the
//! UI watchers: a notification is shown only when its surface is not focused
//! (window inactive, or the session's tab not selected).

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

/// Ghostty's fixed title buffer size (bytes).
pub const TITLE_MAX: usize = 63;
/// Ghostty's fixed body buffer size (bytes).
pub const BODY_MAX: usize = 255;

/// Bound on queued-but-undelivered notifications per session. The UI watcher
/// normally drains within a frame; the cap only matters if a window is wedged.
const QUEUE_MAX: usize = 8;

/// One desktop notification reported by the running application.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TermNotification {
    /// From OSC 777 only; OSC 9 has no title field (Ghostty sets "").
    pub title: Option<String>,
    pub body: String,
}

/// Notification queue shared between the PTY reader thread (producer) and the
/// UI watcher task (consumer, via [`take_notifications`]).
pub type NotificationQueue = Arc<Mutex<VecDeque<TermNotification>>>;

/// Push a notification, dropping the oldest beyond [`QUEUE_MAX`].
pub fn push(queue: &NotificationQueue, n: TermNotification) {
    let mut q = queue.lock();
    if q.len() >= QUEUE_MAX {
        q.pop_front();
    }
    q.push_back(n);
}

/// Drain all pending notifications (UI thread).
pub fn take_notifications(queue: &NotificationQueue) -> Vec<TermNotification> {
    queue.lock().drain(..).collect()
}

/// Truncate to at most `max` bytes on a UTF-8 character boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Parse an OSC 9 payload *after* the `9;` prefix. Returns `None` when the
/// payload is a ConEmu subcommand (first field all digits, value 0–10) —
/// including `9;4` progress, which the caller routes separately.
pub fn parse_osc9(rest: &[u8]) -> Option<TermNotification> {
    let first = rest.split(|&b| b == b';').next().unwrap_or(rest);
    let is_conemu = !first.is_empty()
        && first.len() <= 2
        && first.iter().all(u8::is_ascii_digit)
        && std::str::from_utf8(first)
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .is_some_and(|n| n <= 10);
    if is_conemu {
        return None;
    }
    let body = String::from_utf8_lossy(rest);
    let body = truncate_utf8(&body, BODY_MAX).to_string();
    if body.is_empty() {
        return None;
    }
    Some(TermNotification { title: None, body })
}

/// Parse an OSC 777 payload *after* the `777;` prefix:
/// `notify;<title>;<body>` (body keeps any further semicolons).
pub fn parse_osc777(rest: &[u8]) -> Option<TermNotification> {
    let rest = rest.strip_prefix(b"notify;")?;
    let sep = rest.iter().position(|&b| b == b';');
    let (title, body) = match sep {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &rest[rest.len()..]),
    };
    let title = String::from_utf8_lossy(title);
    let title = truncate_utf8(&title, TITLE_MAX).to_string();
    let body = String::from_utf8_lossy(body);
    let body = truncate_utf8(&body, BODY_MAX).to_string();
    if title.is_empty() && body.is_empty() {
        return None;
    }
    Some(TermNotification {
        title: (!title.is_empty()).then_some(title),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_plain_body() {
        assert_eq!(
            parse_osc9(b"This is test"),
            Some(TermNotification {
                title: None,
                body: "This is test".into()
            })
        );
    }

    #[test]
    fn osc9_body_keeps_semicolons() {
        assert_eq!(
            parse_osc9(b"done; 3 tasks; 0 errors").unwrap().body,
            "done; 3 tasks; 0 errors"
        );
    }

    #[test]
    fn osc9_conemu_subcommands_are_not_notifications() {
        for p in [&b"1;500"[..], b"2;msg", b"3;title", b"4;1;50", b"10;#x1"] {
            assert_eq!(parse_osc9(p), None, "payload {:?}", p);
        }
        // Bare numbers too (9;1 sleep with default duration etc.).
        assert_eq!(parse_osc9(b"4"), None);
    }

    #[test]
    fn osc9_numeric_looking_body_over_10_is_a_notification() {
        assert_eq!(parse_osc9(b"42; done").unwrap().body, "42; done");
        assert_eq!(
            parse_osc9(b"1 task finished").unwrap().body,
            "1 task finished"
        );
    }

    #[test]
    fn osc9_empty_dropped() {
        assert_eq!(parse_osc9(b""), None);
    }

    #[test]
    fn osc777_title_and_body() {
        assert_eq!(
            parse_osc777(b"notify;Build;finished; all green"),
            Some(TermNotification {
                title: Some("Build".into()),
                body: "finished; all green".into()
            })
        );
    }

    #[test]
    fn osc777_requires_notify_verb() {
        assert_eq!(parse_osc777(b"other;Build;x"), None);
    }

    #[test]
    fn osc777_title_only() {
        let n = parse_osc777(b"notify;Just title").unwrap();
        assert_eq!(n.title.as_deref(), Some("Just title"));
        assert_eq!(n.body, "");
    }

    #[test]
    fn truncation_ghostty_limits_on_char_boundary() {
        // 100 × 'あ' = 300 bytes; body cap 255 → 85 chars (255 bytes exactly).
        let long = "あ".repeat(100);
        let n = parse_osc9(long.as_bytes()).unwrap();
        assert_eq!(n.body.chars().count(), 85);
        assert!(n.body.len() <= BODY_MAX);

        let payload = format!("notify;{};x", "あ".repeat(30));
        let n = parse_osc777(payload.as_bytes()).unwrap();
        let t = n.title.unwrap();
        assert!(t.len() <= TITLE_MAX);
        assert_eq!(t.chars().count(), 21); // 63 / 3
    }

    #[test]
    fn queue_caps_at_max() {
        let q: NotificationQueue = Default::default();
        for i in 0..20 {
            push(
                &q,
                TermNotification {
                    title: None,
                    body: format!("n{i}"),
                },
            );
        }
        let drained = take_notifications(&q);
        assert_eq!(drained.len(), 8);
        assert_eq!(drained[0].body, "n12");
        assert_eq!(drained[7].body, "n19");
        assert!(take_notifications(&q).is_empty());
    }
}
