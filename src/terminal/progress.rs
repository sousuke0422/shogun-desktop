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

use std::sync::atomic::{AtomicU8, Ordering};

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

/// Longest payload we accept: `9;4;` + state + `;` + 3-digit percent.
/// Anything longer is not an OSC 9;4 and the scanner abandons it.
const MAX_PAYLOAD: usize = 12;
const PREFIX: &[u8] = b"9;4;";

enum ScanState {
    Ground,
    /// Saw ESC — next byte decides (`]` starts an OSC).
    Esc,
    /// Inside an OSC, accumulating while the payload still matches `9;4;…`.
    Collect,
    /// Saw ESC inside a collected OSC — `\` (ST) terminates it.
    CollectEsc,
}

/// Incremental scanner for OSC 9;4 sequences in a raw PTY byte stream.
///
/// Purely an observer: every byte is also fed to the real VTE parser by the
/// caller, so a mis-detection here can at worst miss a progress update —
/// never corrupt the terminal state.
pub struct Osc94Scanner {
    state: ScanState,
    payload: Vec<u8>,
}

impl Osc94Scanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            payload: Vec::with_capacity(MAX_PAYLOAD),
        }
    }

    /// Feed one byte; returns a parsed update when a full OSC 9;4 terminates.
    pub fn advance(&mut self, byte: u8) -> Option<ProgressUpdate> {
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
                    // Abandon as soon as the payload can no longer be an
                    // OSC 9;4 — this is some other OSC (title, color, 52…).
                    let n = self.payload.len().min(PREFIX.len());
                    if self.payload[..n] != PREFIX[..n] || self.payload.len() > MAX_PAYLOAD {
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

/// Parse `9;4;<state>[;<progress>]` — payload without terminator.
fn parse_payload(payload: &[u8]) -> Option<ProgressUpdate> {
    let rest = payload.strip_prefix(PREFIX)?;
    let mut parts = rest.split(|&b| b == b';');
    let state = parse_num(parts.next()?)?;
    // Percent clamps to 100 (apps occasionally emit 100+ during rounding).
    let percent = parts.next().and_then(parse_num).map(|p| p.min(100) as u8);
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

    fn scan(bytes: &[u8]) -> Vec<ProgressUpdate> {
        let mut s = Osc94Scanner::new();
        bytes.iter().filter_map(|&b| s.advance(b)).collect()
    }

    #[test]
    fn set_progress_bel() {
        assert_eq!(scan(b"\x1b]9;4;1;42\x07"), vec![ProgressUpdate::Set(42)]);
    }

    #[test]
    fn set_progress_st() {
        assert_eq!(
            scan(b"\x1b]9;4;1;100\x1b\\"),
            vec![ProgressUpdate::Set(100)]
        );
    }

    #[test]
    fn remove() {
        assert_eq!(scan(b"\x1b]9;4;0\x07"), vec![ProgressUpdate::Remove]);
        assert_eq!(scan(b"\x1b]9;4;0;0\x07"), vec![ProgressUpdate::Remove]);
    }

    #[test]
    fn error_with_and_without_percent() {
        assert_eq!(scan(b"\x1b]9;4;2\x07"), vec![ProgressUpdate::Error(None)]);
        assert_eq!(scan(b"\x1b]9;4;2;0\x07"), vec![ProgressUpdate::Error(None)]);
        assert_eq!(
            scan(b"\x1b]9;4;2;55\x07"),
            vec![ProgressUpdate::Error(Some(55))]
        );
    }

    #[test]
    fn indeterminate_and_warning() {
        assert_eq!(scan(b"\x1b]9;4;3\x07"), vec![ProgressUpdate::Indeterminate]);
        assert_eq!(
            scan(b"\x1b]9;4;4;10\x07"),
            vec![ProgressUpdate::Warning(Some(10))]
        );
    }

    #[test]
    fn percent_clamped_to_100() {
        assert_eq!(scan(b"\x1b]9;4;1;999\x07"), vec![ProgressUpdate::Set(100)]);
    }

    #[test]
    fn split_across_chunks() {
        let mut s = Osc94Scanner::new();
        let mut got = vec![];
        for chunk in [&b"\x1b]9;"[..], &b"4;1;7"[..], &b"3\x1b"[..], &b"\\"[..]] {
            for &b in chunk {
                if let Some(u) = s.advance(b) {
                    got.push(u);
                }
            }
        }
        assert_eq!(got, vec![ProgressUpdate::Set(73)]);
    }

    #[test]
    fn other_osc_ignored() {
        assert_eq!(scan(b"\x1b]0;window title\x07"), vec![]);
        assert_eq!(scan(b"\x1b]52;c;aGVsbG8=\x07"), vec![]);
        // OSC 9;1 (ConEmu notification) — not 9;4.
        assert_eq!(scan(b"\x1b]9;1;done\x07"), vec![]);
    }

    #[test]
    fn scanner_recovers_after_abandoned_osc() {
        assert_eq!(
            scan(b"\x1b]0;title\x07\x1b]9;4;1;5\x07"),
            vec![ProgressUpdate::Set(5)]
        );
    }

    #[test]
    fn invalid_state_or_garbage_ignored() {
        assert_eq!(scan(b"\x1b]9;4;9\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;4;x;10\x07"), vec![]);
        assert_eq!(scan(b"plain text \x1b[31mred\x1b[0m"), vec![]);
    }

    #[test]
    fn oversized_payload_abandoned() {
        assert_eq!(scan(b"\x1b]9;4;1;100000000\x07"), vec![]);
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
