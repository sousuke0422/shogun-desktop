//! OSC 9;4 progress reporting (ConEmu extension, adopted by Windows Terminal).
//!
//! Sequence: `ESC ] 9 ; 4 ; <state> [; <progress>] (BEL | ESC \)`
//!
//! | state | meaning                                | progress          |
//! |-------|----------------------------------------|-------------------|
//! | 0     | remove progress                        | ignored           |
//! | 1     | normal (e.g. taskbar green)            | 0–100             |
//! | 2     | error (red)                            | optional — keep last when absent/0 |
//! | 3     | indeterminate (pulsing)                | ignored           |
//! | 4     | warning / paused (yellow)              | optional — keep last when absent/0 |
//!
//! The vendored alacritty_terminal / vte stack silently drops OSC 9;4, so a
//! passive byte scanner in the PTY reader thread observes the stream *before*
//! `Processor::advance` and never interferes with the real parser. The scanner
//! must survive sequences split across `read()` chunk boundaries, hence the
//! explicit state machine instead of a regex over each buffer.
//!
//! The same scanner also extracts OSC 9 / OSC 777 desktop notifications
//! (also dropped by vte) — see [`super::notify`] for the parsing rules.

use std::sync::atomic::{AtomicU8, Ordering};

use super::notify::{self, TermNotification};

/// Terminal-reported progress, shared between the PTY reader thread (writer)
/// and the UI (reader). Two `AtomicU8`s instead of a mutex: the reader thread
/// must never block on the render path.
#[derive(Default)]
pub struct Progress {
    /// Encoded [`ProgressState`] (0 = none/removed).
    state: AtomicU8,
    /// Last explicit percentage (0–100). Retained across error/warning
    /// transitions that omit the value, matching Windows Terminal.
    percent: AtomicU8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}

impl Progress {
    /// Current progress, or `None` when no progress is being reported.
    pub fn get(&self) -> Option<(ProgressState, u8)> {
        let state = match self.state.load(Ordering::Relaxed) {
            1 => ProgressState::Normal,
            2 => ProgressState::Error,
            3 => ProgressState::Indeterminate,
            4 => ProgressState::Warning,
            _ => return None,
        };
        Some((state, self.percent.load(Ordering::Relaxed)))
    }

    /// Apply a parsed OSC 9;4 update (called from the PTY reader thread).
    pub fn apply(&self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Remove => self.state.store(0, Ordering::Relaxed),
            ProgressUpdate::Set(pct) => {
                self.percent.store(pct, Ordering::Relaxed);
                self.state.store(1, Ordering::Relaxed);
            }
            ProgressUpdate::Error(pct) => {
                if let Some(p) = pct {
                    self.percent.store(p, Ordering::Relaxed);
                }
                self.state.store(2, Ordering::Relaxed);
            }
            ProgressUpdate::Indeterminate => self.state.store(3, Ordering::Relaxed),
            ProgressUpdate::Warning(pct) => {
                if let Some(p) = pct {
                    self.percent.store(p, Ordering::Relaxed);
                }
                self.state.store(4, Ordering::Relaxed);
            }
        }
    }
}

/// One parsed OSC 9;4 payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgressUpdate {
    Remove,
    /// state 1 — explicit percentage.
    Set(u8),
    /// state 2 — percentage optional (absent or 0 keeps the previous value).
    Error(Option<u8>),
    /// state 3.
    Indeterminate,
    /// state 4 — percentage optional, like `Error`.
    Warning(Option<u8>),
}

/// A semantic-prompt mark (OSC 133, FinalTerm/ghostty shell integration).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptMark {
    /// `133;A` — prompt starts here (the jump-to-prompt anchor).
    PromptStart,
    /// `133;B` — prompt ends, user command line starts.
    CommandStart,
    /// `133;C` — command output starts.
    OutputStart,
    /// `133;D[;exit]` — command finished.
    Finished,
}

/// One event extracted by the scanner from the raw PTY stream.
#[derive(Clone, PartialEq, Debug)]
pub enum OscEvent {
    Progress(ProgressUpdate),
    Notify(TermNotification),
    /// OSC 133 semantic prompt mark.
    Prompt(PromptMark),
    /// Working directory reported by the shell — OSC 9;9 (ConEmu/wt, a
    /// plain Windows path) or OSC 7 (a `file://` URL), already normalized
    /// to a local path.
    Cwd(String),
}

/// Longest payload we collect. Notification bodies are capped at 255 bytes by
/// [`notify`], so anything past this is either garbage or some other OSC the
/// prefix check failed to reject early — abandon it.
const MAX_PAYLOAD: usize = 1024;

/// A collected payload stays alive while it could still become `9;…` or
/// `777;…`; everything else (title, color, OSC 52…) is abandoned on the first
/// mismatching byte.
fn plausible(payload: &[u8]) -> bool {
    let matches = |p: &[u8]| {
        let n = payload.len().min(p.len());
        payload[..n] == p[..n]
    };
    matches(b"9;") || matches(b"777;") || matches(b"133;") || matches(b"7;")
}

enum ScanState {
    Ground,
    /// Saw ESC — next byte decides (`]` starts an OSC).
    Esc,
    /// Inside an OSC, accumulating while the payload still matches `9;4;…`.
    Collect,
    /// Saw ESC inside a collected OSC — `\` (ST) terminates it.
    CollectEsc,
}

/// Incremental scanner for OSC 9 / 777 sequences in a raw PTY byte stream.
///
/// Purely an observer: every byte is also fed to the real VTE parser by the
/// caller, so a mis-detection here can at worst miss a progress update or a
/// notification — never corrupt the terminal state.
pub struct OscScanner {
    state: ScanState,
    payload: Vec<u8>,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            payload: Vec::with_capacity(64),
        }
    }

    /// Feed one byte; returns an event when a matching OSC terminates.
    pub fn advance(&mut self, byte: u8) -> Option<OscEvent> {
        match self.state {
            ScanState::Ground => {
                if byte == 0x1b {
                    self.state = ScanState::Esc;
                }
                None
            }
            ScanState::Esc => {
                self.state = match byte {
                    b']' => {
                        self.payload.clear();
                        ScanState::Collect
                    }
                    0x1b => ScanState::Esc,
                    _ => ScanState::Ground,
                };
                None
            }
            ScanState::Collect => match byte {
                0x07 => {
                    // BEL terminator.
                    self.state = ScanState::Ground;
                    parse_payload(&self.payload)
                }
                0x1b => {
                    self.state = ScanState::CollectEsc;
                    None
                }
                _ => {
                    self.payload.push(byte);
                    if !plausible(&self.payload) || self.payload.len() > MAX_PAYLOAD {
                        self.state = ScanState::Ground;
                        self.payload.clear();
                    }
                    None
                }
            },
            ScanState::CollectEsc => match byte {
                b'\\' => {
                    // ESC \ (ST) terminator.
                    self.state = ScanState::Ground;
                    parse_payload(&self.payload)
                }
                b']' => {
                    // ESC inside the OSC actually started a new one.
                    self.payload.clear();
                    self.state = ScanState::Collect;
                    None
                }
                0x1b => {
                    self.state = ScanState::Esc;
                    None
                }
                _ => {
                    self.state = ScanState::Ground;
                    None
                }
            },
        }
    }
}

/// Dispatch a terminated OSC payload (without terminator) to the progress or
/// notification parser.
fn parse_payload(payload: &[u8]) -> Option<OscEvent> {
    if let Some(rest) = payload.strip_prefix(b"9;4;") {
        // Malformed 9;4 payloads are dropped, not shown as notifications
        // (deliberate Ghostty deviation — see notify.rs module docs).
        return parse_progress(rest).map(OscEvent::Progress);
    }
    if let Some(rest) = payload.strip_prefix(b"9;9;") {
        // ConEmu/wt cwd report — checked before the OSC 9 notification so a
        // path never surfaces as a toast. Value may be double-quoted.
        return parse_cwd_windows(rest).map(OscEvent::Cwd);
    }
    if let Some(rest) = payload.strip_prefix(b"9;") {
        return notify::parse_osc9(rest).map(OscEvent::Notify);
    }
    if let Some(rest) = payload.strip_prefix(b"777;") {
        return notify::parse_osc777(rest).map(OscEvent::Notify);
    }
    if let Some(rest) = payload.strip_prefix(b"133;") {
        let mark = match rest.first()? {
            b'A' => PromptMark::PromptStart,
            b'B' => PromptMark::CommandStart,
            b'C' => PromptMark::OutputStart,
            b'D' => PromptMark::Finished,
            _ => return None,
        };
        return Some(OscEvent::Prompt(mark));
    }
    if let Some(rest) = payload.strip_prefix(b"7;") {
        return cwd_from_file_url(rest).map(OscEvent::Cwd);
    }
    None
}

/// `9;9;<path>` — the path as the shell printed it, minus optional quotes.
fn parse_cwd_windows(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    (!s.is_empty()).then(|| s.to_string())
}

/// `7;file://host/path` → a local path. Only an empty host / `localhost` is
/// accepted (a foreign host's path means nothing to this machine); percent
/// escapes decode, and a `/C:/…` drive form loses its leading slash so the
/// result is a usable Windows path. Other absolute paths pass through as-is —
/// the consumer existence-checks before using them.
fn cwd_from_file_url(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?.trim();
    let after = s.strip_prefix("file://")?;
    let slash = after.find('/')?;
    let (host, path) = after.split_at(slash);
    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        return None;
    }
    // Percent-decode.
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let mut path = String::from_utf8(out).ok()?;
    // `/C:/work` (URL form of a Windows path) → `C:/work`.
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path.remove(0);
    }
    (!path.is_empty()).then_some(path)
}

/// Parse `<state>[;<progress>]` — the part after `9;4;`.
fn parse_progress(rest: &[u8]) -> Option<ProgressUpdate> {
    let mut parts = rest.split(|&b| b == b';');
    let state = parse_num(parts.next()?)?;
    // Percent clamps to 100 (apps occasionally emit 100+ during rounding).
    // A present-but-unparseable percent makes the whole payload malformed —
    // except an *empty* field, which is a trailing `;` with no value. Claude
    // Code emits exactly that for stateful updates: `ESC]9;4;3;BEL`
    // (indeterminate) and `ESC]9;4;0;BEL` (remove) carry an empty 4th arg, and
    // the ConEmu spec leaves the progress optional for those states. Treating
    // the empty field as absent (not malformed) is what kept Claude's
    // indeterminate bar from ever showing.
    let percent = match parts.next() {
        None => None,
        Some(f) if f.is_empty() => None,
        Some(f) => Some(parse_num(f)?.min(100) as u8),
    };
    match state {
        0 => Some(ProgressUpdate::Remove),
        1 => Some(ProgressUpdate::Set(percent.unwrap_or(0))),
        2 => Some(ProgressUpdate::Error(percent.filter(|&p| p > 0))),
        3 => Some(ProgressUpdate::Indeterminate),
        4 => Some(ProgressUpdate::Warning(percent.filter(|&p| p > 0))),
        _ => None,
    }
}

fn parse_num(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || digits.len() > 3 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Vec<OscEvent> {
        let mut s = OscScanner::new();
        bytes.iter().filter_map(|&b| s.advance(b)).collect()
    }

    fn progress(u: ProgressUpdate) -> OscEvent {
        OscEvent::Progress(u)
    }

    #[test]
    fn set_progress_bel() {
        assert_eq!(
            scan(b"\x1b]9;4;1;42\x07"),
            vec![progress(ProgressUpdate::Set(42))]
        );
    }

    #[test]
    fn prompt_marks_parse() {
        assert_eq!(
            scan(b"\x1b]133;A\x07"),
            vec![OscEvent::Prompt(PromptMark::PromptStart)]
        );
        assert_eq!(
            scan(b"\x1b]133;D;0\x1b\\"),
            vec![OscEvent::Prompt(PromptMark::Finished)]
        );
        assert_eq!(scan(b"\x1b]133;Z\x07"), vec![]);
    }

    #[test]
    fn cwd_reports_parse() {
        // ConEmu/wt form, quoted and bare — and never as a notification.
        assert_eq!(
            scan(b"\x1b]9;9;\"C:\\work\\x\"\x07"),
            vec![OscEvent::Cwd("C:\\work\\x".into())]
        );
        assert_eq!(
            scan(b"\x1b]9;9;C:\\work\x07"),
            vec![OscEvent::Cwd("C:\\work".into())]
        );
        // OSC 7 file URL: localhost + percent decoding + drive form.
        assert_eq!(
            scan(b"\x1b]7;file://localhost/C:/a%20b\x07"),
            vec![OscEvent::Cwd("C:/a b".into())]
        );
        assert_eq!(
            scan(b"\x1b]7;file:///home/aki\x07"),
            vec![OscEvent::Cwd("/home/aki".into())]
        );
        // A foreign host's path means nothing here.
        assert_eq!(scan(b"\x1b]7;file://elsewhere/home/aki\x07"), vec![]);
    }

    #[test]
    fn set_progress_st() {
        assert_eq!(
            scan(b"\x1b]9;4;1;100\x1b\\"),
            vec![progress(ProgressUpdate::Set(100))]
        );
    }

    #[test]
    fn remove() {
        assert_eq!(
            scan(b"\x1b]9;4;0\x07"),
            vec![progress(ProgressUpdate::Remove)]
        );
        assert_eq!(
            scan(b"\x1b]9;4;0;0\x07"),
            vec![progress(ProgressUpdate::Remove)]
        );
    }

    #[test]
    fn error_with_and_without_percent() {
        assert_eq!(
            scan(b"\x1b]9;4;2\x07"),
            vec![progress(ProgressUpdate::Error(None))]
        );
        assert_eq!(
            scan(b"\x1b]9;4;2;0\x07"),
            vec![progress(ProgressUpdate::Error(None))]
        );
        assert_eq!(
            scan(b"\x1b]9;4;2;55\x07"),
            vec![progress(ProgressUpdate::Error(Some(55)))]
        );
    }

    #[test]
    fn indeterminate_and_warning() {
        assert_eq!(
            scan(b"\x1b]9;4;3\x07"),
            vec![progress(ProgressUpdate::Indeterminate)]
        );
        assert_eq!(
            scan(b"\x1b]9;4;4;10\x07"),
            vec![progress(ProgressUpdate::Warning(Some(10)))]
        );
    }

    #[test]
    fn trailing_empty_percent_is_tolerated() {
        // Claude Code (2.x) builds progress as `db(9, 4, <state>, "")`, i.e.
        // it always appends a trailing `;` with an empty value. That empty
        // field must read as "no percent", not as a malformed payload — this
        // is exactly why Claude's indeterminate bar never showed.
        assert_eq!(
            scan(b"\x1b]9;4;3;\x07"),
            vec![progress(ProgressUpdate::Indeterminate)]
        );
        assert_eq!(
            scan(b"\x1b]9;4;0;\x07"),
            vec![progress(ProgressUpdate::Remove)]
        );
        assert_eq!(
            scan(b"\x1b]9;4;2;\x07"),
            vec![progress(ProgressUpdate::Error(None))]
        );
        // A real value alongside a further trailing `;` still parses.
        assert_eq!(
            scan(b"\x1b]9;4;1;42;\x07"),
            vec![progress(ProgressUpdate::Set(42))]
        );
    }

    #[test]
    fn percent_clamped_to_100() {
        assert_eq!(
            scan(b"\x1b]9;4;1;999\x07"),
            vec![progress(ProgressUpdate::Set(100))]
        );
    }

    #[test]
    fn split_across_chunks() {
        let mut s = OscScanner::new();
        let mut got = vec![];
        for chunk in [&b"\x1b]9;"[..], &b"4;1;7"[..], &b"3\x1b"[..], &b"\\"[..]] {
            for &b in chunk {
                if let Some(u) = s.advance(b) {
                    got.push(u);
                }
            }
        }
        assert_eq!(got, vec![progress(ProgressUpdate::Set(73))]);
    }

    #[test]
    fn other_osc_ignored() {
        assert_eq!(scan(b"\x1b]0;window title\x07"), vec![]);
        assert_eq!(scan(b"\x1b]52;c;aGVsbG8=\x07"), vec![]);
        // OSC 9;1 (ConEmu sleep) — a ConEmu subcommand, not a notification.
        assert_eq!(scan(b"\x1b]9;1;done\x07"), vec![]);
    }

    #[test]
    fn scanner_recovers_after_abandoned_osc() {
        assert_eq!(
            scan(b"\x1b]0;title\x07\x1b]9;4;1;5\x07"),
            vec![progress(ProgressUpdate::Set(5))]
        );
    }

    #[test]
    fn invalid_state_or_garbage_ignored() {
        assert_eq!(scan(b"\x1b]9;4;9\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;4;x;10\x07"), vec![]);
        assert_eq!(scan(b"plain text \x1b[31mred\x1b[0m"), vec![]);
    }

    #[test]
    fn oversized_percent_dropped_not_notified() {
        assert_eq!(scan(b"\x1b]9;4;1;100000000\x07"), vec![]);
    }

    #[test]
    fn osc9_notification_bel_and_st() {
        let want = OscEvent::Notify(TermNotification {
            title: None,
            body: "This is test".into(),
        });
        assert_eq!(scan(b"\x1b]9;This is test\x07"), vec![want.clone()]);
        assert_eq!(scan(b"\x1b]9;This is test\x1b\\"), vec![want]);
    }

    #[test]
    fn osc777_notification() {
        assert_eq!(
            scan(b"\x1b]777;notify;Title;Body text\x07"),
            vec![OscEvent::Notify(TermNotification {
                title: Some("Title".into()),
                body: "Body text".into(),
            })]
        );
    }

    #[test]
    fn osc9_notification_split_across_chunks() {
        let mut s = OscScanner::new();
        let mut got = vec![];
        for chunk in [&b"\x1b]9;ta"[..], &b"sk done"[..], &b"\x07"[..]] {
            for &b in chunk {
                if let Some(u) = s.advance(b) {
                    got.push(u);
                }
            }
        }
        assert_eq!(
            got,
            vec![OscEvent::Notify(TermNotification {
                title: None,
                body: "task done".into(),
            })]
        );
    }

    #[test]
    fn notification_interleaved_with_progress() {
        assert_eq!(
            scan(b"\x1b]9;4;1;50\x07\x1b]9;half way\x07"),
            vec![
                progress(ProgressUpdate::Set(50)),
                OscEvent::Notify(TermNotification {
                    title: None,
                    body: "half way".into(),
                }),
            ]
        );
    }

    #[test]
    fn giant_notification_body_abandoned() {
        let mut seq = b"\x1b]9;".to_vec();
        seq.extend(std::iter::repeat_n(b'x', 2000));
        seq.push(0x07);
        assert_eq!(scan(&seq), vec![]);
    }

    #[test]
    fn progress_apply_semantics() {
        let p = Progress::default();
        assert_eq!(p.get(), None);
        p.apply(ProgressUpdate::Set(40));
        assert_eq!(p.get(), Some((ProgressState::Normal, 40)));
        // Error without percent keeps the last value.
        p.apply(ProgressUpdate::Error(None));
        assert_eq!(p.get(), Some((ProgressState::Error, 40)));
        p.apply(ProgressUpdate::Warning(Some(60)));
        assert_eq!(p.get(), Some((ProgressState::Warning, 60)));
        p.apply(ProgressUpdate::Indeterminate);
        assert_eq!(p.get(), Some((ProgressState::Indeterminate, 60)));
        p.apply(ProgressUpdate::Remove);
        assert_eq!(p.get(), None);
    }
}
