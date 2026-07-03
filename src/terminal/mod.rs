pub mod ime;
pub mod keys;
pub mod pty_session;
pub mod renderer;
pub mod selection;

use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::{Term, event::EventListener};
use parking_lot::FairMutex;

// ── OSC 52 clipboard listener ─────────────────────────────────────────────────

/// Events forwarded from the VTE parser to the clipboard handler thread.
pub enum ClipboardEvent {
    /// OSC 52 write: application wants to store text in the host clipboard.
    Store(String),
    /// OSC 52 read: application wants the clipboard content back via PTY write.
    /// The callback formats the OSC 52 response string when called with the
    /// clipboard text (generated internally by alacritty_terminal).
    Load(Arc<dyn Fn(&str) -> String + Sync + Send + 'static>),
    /// Generic PTY write-back (used by OSC color queries etc.).
    PtyWrite(String),
}

/// EventListener implementation that forwards clipboard-related events to a
/// background handler thread via a bounded channel.
///
/// Using `try_send` ensures the PTY reader thread never blocks on a slow
/// clipboard operation — events are silently dropped if the buffer is full.
pub struct ClipboardListener {
    pub tx: std::sync::mpsc::SyncSender<ClipboardEvent>,
}

impl EventListener for ClipboardListener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        use alacritty_terminal::event::Event;
        match event {
            Event::ClipboardStore(_ty, text) => {
                let _ = self.tx.try_send(ClipboardEvent::Store(text));
            }
            Event::ClipboardLoad(_ty, callback) => {
                let _ = self.tx.try_send(ClipboardEvent::Load(callback));
            }
            Event::PtyWrite(text) => {
                let _ = self.tx.try_send(ClipboardEvent::PtyWrite(text));
            }
            _ => {}
        }
    }
}

/// Trait implemented by backend-specific PTY resizers.
///
/// Implementors must be `Send + Sync` so the resizer can be stored in an `Arc`
/// and called from any thread (including GPUI's render/event thread).
pub trait PtyResizer: Send + Sync {
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;
}

/// No-op resizer used as a fallback when the backend provides no resize channel.
#[allow(dead_code)]
pub struct NoopResizer;

impl PtyResizer for NoopResizer {
    fn resize(&self, _cols: u16, _rows: u16) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct TerminalSession {
    #[allow(dead_code)]
    pub term: Arc<FairMutex<Term<ClipboardListener>>>,
    pub writer: Arc<FairMutex<Box<dyn std::io::Write + Send>>>,
    pub snapshot: Arc<FairMutex<GridSnapshot>>,
    pub connected: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    /// Signalled by the reader thread whenever `generation` advances (and on
    /// disconnect). UI refresh tasks park on this instead of polling, so an
    /// idle terminal causes zero wakeups.
    pub notify: Arc<tokio::sync::Notify>,
    #[allow(dead_code)]
    pub error: Arc<FairMutex<Option<String>>>,
    /// Current terminal width in columns (updated by `resize`).
    pub cols: AtomicU16,
    /// Current terminal height in rows (updated by `resize`).
    pub rows: AtomicU16,
    /// Backend-specific mechanism for propagating resize to the PTY / SSH channel.
    pub resizer: Arc<dyn PtyResizer>,
}

impl TerminalSession {
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn send_bytes(&self, bytes: &[u8]) {
        let _ = self.writer.lock().write_all(bytes);
    }

    /// Route a wheel event to the PTY when the running application asked for
    /// it. Returns `false` when the wheel should scroll the local scrollback
    /// instead (plain primary-screen shell). See [`wheel_pty_bytes`].
    pub fn wheel_to_pty(&self, lines: i32, col: usize, row: usize) -> bool {
        let mode = *self.term.lock().mode();
        match wheel_pty_bytes(mode, lines, col, row) {
            Some(buf) => {
                if !buf.is_empty() {
                    self.send_bytes(&buf);
                }
                true
            }
            None => false,
        }
    }

    /// Paste text into the terminal, honoring bracketed-paste mode. Snaps the
    /// view back to the live bottom first, like typing does.
    pub fn paste(&self, text: &str) {
        let mode = *self.term.lock().mode();
        self.scroll_display_to_bottom();
        self.send_bytes(&paste_pty_bytes(mode, text));
    }

    /// Scroll the emulator's display window into the scrollback history
    /// (positive = older content) and refresh the snapshot immediately —
    /// the reader thread only refreshes it on PTY output.
    pub fn scroll_display(&self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        if delta == 0 {
            return;
        }
        let mut t = self.term.lock();
        let before = t.grid().display_offset();
        t.scroll_display(Scroll::Delta(delta));
        if t.grid().display_offset() == before {
            return; // already clamped at top/bottom — nothing to repaint
        }
        *self.snapshot.lock() = take_snapshot(&t);
        drop(t);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Snap the display window back to the live view (bottom).
    pub fn scroll_display_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        let mut t = self.term.lock();
        if t.grid().display_offset() == 0 {
            return;
        }
        t.scroll_display(Scroll::Bottom);
        *self.snapshot.lock() = take_snapshot(&t);
        drop(t);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Resize the terminal to the given dimensions.
    ///
    /// This updates the internal `alacritty_terminal::Term` geometry **and**
    /// notifies the backing PTY / SSH channel so that remote applications
    /// (e.g. tmux) can reflow their layout accordingly.
    pub fn resize(&self, cols: u16, rows: u16) {
        use alacritty_terminal::term::test::TermSize;
        // 1. Resize the in-process term emulator.
        {
            let mut t = self.term.lock();
            t.resize(TermSize::new(cols as usize, rows as usize));
        }
        // 2. Persist the new size so callers can detect when the session is
        //    already at the right dimensions without re-sending the resize.
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        // 3. Tell the OS PTY / SSH channel.
        let _ = self.resizer.resize(cols, rows);
    }
}

#[derive(Clone)]
pub struct GridSnapshot {
    #[allow(dead_code)]
    pub cols: usize,
    #[allow(dead_code)]
    pub rows: usize,
    pub cells: Vec<Vec<SnapshotCell>>,
    #[allow(dead_code)]
    pub cursor: (usize, usize),
    /// Lines scrolled back into history (0 = live view at the bottom).
    pub display_offset: usize,
}

impl GridSnapshot {
    pub fn blank(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![SnapshotCell::blank(); cols]; rows],
            cursor: (0, 0),
            display_offset: 0,
        }
    }
}

#[derive(Clone)]
pub struct SnapshotCell {
    pub c: char,
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    /// 0 = skip render (wide spacer), 1 = half-width, 2 = wide (Flags::WIDE_CHAR).
    pub display_width: u8,
    pub bold: bool,
    pub underline: bool,
}

impl SnapshotCell {
    pub fn blank() -> Self {
        Self {
            c: ' ',
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 1,
            bold: false,
            underline: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ResolvedColor {
    Default,
    Rgb(u8, u8, u8),
}

/// Encode pasted text for the PTY.
///
/// - Bracketed-paste mode (`?2004`, requested by claude code / opencode /
///   modern shells): wrap in `ESC[200~ … ESC[201~` so the app can tell a
///   paste from typing (multiline text stops auto-executing per line). Any
///   embedded end marker is stripped — otherwise clipboard content could
///   break out of the bracket and inject keystrokes.
/// - Plain mode: newlines are normalized to CR, which is what a terminal
///   sends for the Enter key.
pub fn paste_pty_bytes(mode: alacritty_terminal::term::TermMode, text: &str) -> Vec<u8> {
    use alacritty_terminal::term::TermMode;
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let sanitized = text.replace("\x1b[201~", "");
        let mut buf = Vec::with_capacity(sanitized.len() + 12);
        buf.extend_from_slice(b"\x1b[200~");
        buf.extend_from_slice(sanitized.as_bytes());
        buf.extend_from_slice(b"\x1b[201~");
        buf
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Encode a wheel event for the PTY according to the terminal's modes, or
/// `None` when the wheel should scroll the local scrollback instead.
///
/// - Mouse-reporting apps (btop, htop, tmux `mouse on`): mouse buttons 64/65
///   at the pointed-at cell, SGR (`?1006`) or X10 flavor.
/// - Alt-screen apps with alternate-scroll (`?1007`): arrow keys, honoring
///   DECCKM application-cursor mode (less, vim without mouse).
///
/// `lines > 0` = wheel up. `col`/`row` are 0-based grid coordinates.
pub fn wheel_pty_bytes(
    mode: alacritty_terminal::term::TermMode,
    lines: i32,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let btn: u32 = if lines > 0 { 64 } else { 65 };
        let mut buf = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            if mode.contains(TermMode::SGR_MOUSE) {
                buf.extend_from_slice(format!("\x1b[<{btn};{};{}M", col + 1, row + 1).as_bytes());
            } else {
                // X10 encoding caps coordinates at 223 (255 - 32).
                buf.extend_from_slice(&[
                    0x1b,
                    b'[',
                    b'M',
                    (32 + btn) as u8,
                    (32 + (col + 1).min(223)) as u8,
                    (32 + (row + 1).min(223)) as u8,
                ]);
            }
        }
        Some(buf)
    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        let mut buf = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            buf.extend_from_slice(seq);
        }
        Some(buf)
    } else {
        None
    }
}

/// alacritty_terminal の Cell/Color → SnapshotCell に変換
///
/// Generic over any `EventListener` so tests can use `VoidListener` while
/// production code uses `ClipboardListener`.
pub fn take_snapshot<L: EventListener>(term: &Term<L>) -> GridSnapshot {
    use alacritty_terminal::term::cell::Flags;

    let content = term.renderable_content();
    let cols = term.columns();
    let rows = term.screen_lines();
    let mut cells = vec![vec![SnapshotCell::blank(); cols]; rows];

    for indexed in content.display_iter {
        let row = indexed.point.line.0 as usize;
        let col = indexed.point.column.0;
        if row < rows && col < cols {
            let is_spacer = indexed
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let display_width = if is_spacer {
                0
            } else if indexed.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            cells[row][col] = SnapshotCell {
                c: indexed.c,
                fg: resolve_color(indexed.fg, content.colors),
                bg: resolve_color(indexed.bg, content.colors),
                display_width,
                bold: indexed.flags.contains(Flags::BOLD),
                underline: indexed.flags.contains(Flags::UNDERLINE),
            };
        }
    }

    let cur = content.cursor.point;
    GridSnapshot {
        cols,
        rows,
        cells,
        cursor: (cur.line.0 as usize, cur.column.0),
        display_offset: term.grid().display_offset(),
    }
}

fn resolve_color(
    color: alacritty_terminal::vte::ansi::Color,
    table: &alacritty_terminal::term::color::Colors,
) -> ResolvedColor {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    match color {
        Color::Named(NamedColor::Foreground) | Color::Named(NamedColor::Background) => {
            ResolvedColor::Default
        }
        Color::Named(named) => table[named]
            .map(|rgb| ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b))
            .or_else(|| fallback_named_color(named))
            .unwrap_or(ResolvedColor::Default),
        Color::Indexed(idx) => table[idx as usize]
            .map(|rgb| ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b))
            .or_else(|| fallback_indexed_color(idx))
            .unwrap_or(ResolvedColor::Default),
        Color::Spec(rgb) => ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// xterm palette fallback when the term color table has no entry yet.
fn fallback_named_color(named: alacritty_terminal::vte::ansi::NamedColor) -> Option<ResolvedColor> {
    use alacritty_terminal::vte::ansi::NamedColor;
    let (r, g, b) = match named {
        NamedColor::Black => (0x1e, 0x1e, 0x1e),
        NamedColor::Red => (0xcc, 0x00, 0x00),
        NamedColor::Green => (0x4e, 0x9a, 0x06),
        NamedColor::Yellow => (0xc4, 0xa0, 0x00),
        NamedColor::Blue => (0x34, 0x65, 0xa4),
        NamedColor::Magenta => (0x75, 0x50, 0x7b),
        NamedColor::Cyan => (0x06, 0x98, 0x9a),
        NamedColor::White => (0xd3, 0xd7, 0xcf),
        NamedColor::BrightBlack => (0x55, 0x57, 0x53),
        NamedColor::BrightRed => (0xef, 0x29, 0x29),
        NamedColor::BrightGreen => (0x8a, 0xe2, 0x34),
        NamedColor::BrightYellow => (0xfc, 0xe9, 0x4f),
        NamedColor::BrightBlue => (0x72, 0x9f, 0xcf),
        NamedColor::BrightMagenta => (0xad, 0x7f, 0xa8),
        NamedColor::BrightCyan => (0x34, 0xe2, 0xe2),
        NamedColor::BrightWhite => (0xee, 0xee, 0xec),
        _ => return None,
    };
    Some(ResolvedColor::Rgb(r, g, b))
}

fn fallback_indexed_color(idx: u8) -> Option<ResolvedColor> {
    let (r, g, b) = match idx {
        0..=7 => {
            let base = fallback_named_color(match idx {
                0 => alacritty_terminal::vte::ansi::NamedColor::Black,
                1 => alacritty_terminal::vte::ansi::NamedColor::Red,
                2 => alacritty_terminal::vte::ansi::NamedColor::Green,
                3 => alacritty_terminal::vte::ansi::NamedColor::Yellow,
                4 => alacritty_terminal::vte::ansi::NamedColor::Blue,
                5 => alacritty_terminal::vte::ansi::NamedColor::Magenta,
                6 => alacritty_terminal::vte::ansi::NamedColor::Cyan,
                _ => alacritty_terminal::vte::ansi::NamedColor::White,
            })?;
            let ResolvedColor::Rgb(r, g, b) = base else {
                return None;
            };
            (r, g, b)
        }
        8..=15 => {
            let base = fallback_named_color(match idx - 8 {
                0 => alacritty_terminal::vte::ansi::NamedColor::BrightBlack,
                1 => alacritty_terminal::vte::ansi::NamedColor::BrightRed,
                2 => alacritty_terminal::vte::ansi::NamedColor::BrightGreen,
                3 => alacritty_terminal::vte::ansi::NamedColor::BrightYellow,
                4 => alacritty_terminal::vte::ansi::NamedColor::BrightBlue,
                5 => alacritty_terminal::vte::ansi::NamedColor::BrightMagenta,
                6 => alacritty_terminal::vte::ansi::NamedColor::BrightCyan,
                _ => alacritty_terminal::vte::ansi::NamedColor::BrightWhite,
            })?;
            let ResolvedColor::Rgb(r, g, b) = base else {
                return None;
            };
            (r, g, b)
        }
        16..=231 => {
            let n = idx - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let v = |x: u8| if x == 0 { 0u8 } else { x * 40 + 55 };
            (v(r), v(g), v(b))
        }
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            (v, v, v)
        }
    };
    Some(ResolvedColor::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::{Processor, StdSyncHandler},
    };

    fn make_term(cols: usize, rows: usize) -> Term<VoidListener> {
        Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener)
    }

    fn advance_bytes(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser = Processor::<StdSyncHandler>::new();
        for &byte in bytes {
            parser.advance(term, byte);
        }
    }

    #[test]
    fn paste_plain_normalizes_newlines_to_cr() {
        let term = make_term(80, 24);
        assert_eq!(
            paste_pty_bytes(*term.mode(), "a\r\nb\nc"),
            b"a\rb\rc".to_vec()
        );
    }

    #[test]
    fn paste_bracketed_wraps_and_strips_end_marker() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?2004h");
        // Newlines pass through untouched; an embedded end marker is stripped
        // so clipboard content cannot break out of the bracket.
        assert_eq!(
            paste_pty_bytes(*term.mode(), "a\nb\x1b[201~echo pwned\n"),
            b"\x1b[200~a\nbecho pwned\n\x1b[201~".to_vec()
        );
    }

    #[test]
    fn wheel_plain_shell_scrolls_local_history() {
        let term = make_term(80, 24);
        assert_eq!(wheel_pty_bytes(*term.mode(), 1, 0, 0), None);
    }

    #[test]
    fn wheel_sgr_mouse_reporting_encodes_buttons_at_cell() {
        let mut term = make_term(80, 24);
        // ?1002h = mouse drag reporting, ?1006h = SGR encoding (btop's setup).
        advance_bytes(&mut term, b"\x1b[?1002h\x1b[?1006h");
        let up = wheel_pty_bytes(*term.mode(), 2, 5, 3).expect("routed to pty");
        assert_eq!(up, b"\x1b[<64;6;4M\x1b[<64;6;4M");
        let down = wheel_pty_bytes(*term.mode(), -1, 0, 0).expect("routed to pty");
        assert_eq!(down, b"\x1b[<65;1;1M");
    }

    #[test]
    fn wheel_x10_mouse_reporting_without_sgr() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h");
        let up = wheel_pty_bytes(*term.mode(), 1, 5, 3).expect("routed to pty");
        assert_eq!(up, &[0x1b, b'[', b'M', 32 + 64, 32 + 6, 32 + 4]);
    }

    #[test]
    fn wheel_alternate_scroll_sends_arrows() {
        let mut term = make_term(80, 24);
        // ?1049h = alt screen, ?1007h = alternate scroll (less-style).
        advance_bytes(&mut term, b"\x1b[?1049h\x1b[?1007h");
        let up = wheel_pty_bytes(*term.mode(), 2, 0, 0).expect("routed to pty");
        assert_eq!(up, b"\x1b[A\x1b[A");
        // DECCKM application cursor mode switches to SS3 arrows.
        advance_bytes(&mut term, b"\x1b[?1h");
        let down = wheel_pty_bytes(*term.mode(), -1, 0, 0).expect("routed to pty");
        assert_eq!(down, b"\x1bOB");
    }

    #[test]
    fn wheel_alt_screen_defaults_to_arrows_and_1007l_disables() {
        let mut term = make_term(80, 24);
        // ALTERNATE_SCROLL is ON by default in alacritty, so entering the
        // alt screen alone already routes the wheel as arrow keys…
        advance_bytes(&mut term, b"\x1b[?1049h");
        assert_eq!(
            wheel_pty_bytes(*term.mode(), 1, 0, 0),
            Some(b"\x1b[A".to_vec())
        );
        // …until the app opts out with ?1007l.
        advance_bytes(&mut term, b"\x1b[?1007l");
        assert_eq!(wheel_pty_bytes(*term.mode(), 1, 0, 0), None);
    }

    #[test]
    fn empty_terminal_snapshot_dimensions_and_blank_cells() {
        let term = make_term(10, 5);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.rows, 5);
        assert!(snap.cells.iter().flatten().all(|c| c.c == ' '));
    }

    #[test]
    fn written_text_appears_in_snapshot() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"H");
        let snap = take_snapshot(&term);
        assert!(snap.cells.iter().flatten().any(|c| c.c == 'H'));
    }

    #[test]
    fn ansi_rgb_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[38;2;255;0;0mR");
        let snap = take_snapshot(&term);
        assert!(
            snap.cells
                .iter()
                .flatten()
                .any(|c| c.c == 'R' && c.fg == ResolvedColor::Rgb(255, 0, 0))
        );
    }

    #[test]
    fn ansi_named_red_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[31mR\x1b[0m");
        let snap = take_snapshot(&term);
        let r_cell = snap
            .cells
            .iter()
            .flatten()
            .find(|c| c.c == 'R')
            .expect("R cell");
        assert!(
            matches!(r_cell.fg, ResolvedColor::Rgb(r, _, _) if r > 0),
            "named red should resolve to non-zero RGB, got {:?}",
            r_cell.fg
        );
    }

    #[test]
    fn ansi_indexed_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[38;5;46mG");
        let snap = take_snapshot(&term);
        assert!(
            snap.cells
                .iter()
                .flatten()
                .any(|c| c.c == 'G' && matches!(c.fg, ResolvedColor::Rgb(_, _, _)))
        );
    }

    #[test]
    fn ansi_background_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[42mB\x1b[0m");
        let snap = take_snapshot(&term);
        let b_cell = snap
            .cells
            .iter()
            .flatten()
            .find(|c| c.c == 'B')
            .expect("B cell");
        assert!(
            matches!(b_cell.bg, ResolvedColor::Rgb(_, _, _)),
            "green background should resolve to RGB, got {:?}",
            b_cell.bg
        );
    }

    #[test]
    fn initial_cursor_is_at_origin() {
        let term = make_term(10, 5);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cursor, (0, 0));
    }
}
