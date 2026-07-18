//! Session assembly over an *inherited* ConPTY — the Windows default-terminal
//! handoff (`ITerminalHandoff3`) and, later, cross-window tab moves.
//!
//! Unlike [`crate::pty_session::build_terminal_session`]'s usual callers, the
//! embedder spawns nothing here: the console session already exists, and this
//! module merely wraps the handles it was given —
//!
//! - `input`: our write end of the console's input pipe (keystrokes go in),
//! - `output`: our read end of the console's VT output pipe,
//! - `signal`: the ConPTY signal pipe; resizes are written to it using
//!   winconpty's wire format (see [`resize_signal_packet`]),
//! - `keepalive`: the server/client *process* handles from the handoff, held
//!   for later bookkeeping (exit codes, client names); process handles keep
//!   nothing alive. They ride in the resizer, whose `Arc` drops with the
//!   [`TerminalSession`]. The ConDrv *reference* handle must NOT ride here:
//!   as long as it exists conhost keeps serving even after its last client
//!   left (winconpty.h), so an exited shell would never break our output
//!   pipe and the tab would linger forever. Upstream drops it the moment the
//!   connection starts (`ConptyConnection::Start` →
//!   `ConptyReleasePseudoConsole`); callers here do the same.
//!
//! Who created the handles is the caller's business (the monarch pulls them
//! from the handoff shim with `DuplicateHandle`); by the time they reach this
//! module they are plain owned handles in this process.
#![cfg(windows)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::OwnedHandle;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::FairMutex;

use crate::{PtyResizer, TerminalSession};

/// The handles a default-terminal handoff (or a tab move) delivers, already
/// owned by this process. `keepalive` is closed when the session drops.
pub struct HandoffPty {
    /// Write end: terminal → console input.
    pub input: OwnedHandle,
    /// Read end: console VT output → terminal.
    pub output: OwnedHandle,
    /// ConPTY signal pipe (write end). `None` = resize requests are dropped.
    pub signal: Option<OwnedHandle>,
    /// Server/client process handles held for the session's lifetime —
    /// never the ConDrv reference handle (see module docs).
    pub keepalive: Vec<OwnedHandle>,
}

/// Birth-time duplicates of a session's ConPTY handle set, stocked on
/// `TerminalSession::transfer` so a later cross-process tab move has clean
/// handles to send. Duplicated at assembly because the live set is
/// unreachable afterwards (the reader thread owns its `File` as
/// `Box<dyn Read>`), and because the receiver's `DUPLICATE_CLOSE_SOURCE`
/// pull consumes what it is given — consuming an independent duplicate
/// leaves the session's own handles valid for the sender's teardown.
pub struct TransferKit {
    pub input: OwnedHandle,
    pub output: OwnedHandle,
    pub signal: Option<OwnedHandle>,
    /// Same order as `HandoffPty::keepalive` (conhost/server first, then
    /// client) — the wire's `server`/`client` slots are filled from it.
    pub keepalive: Vec<OwnedHandle>,
}

impl TransferKit {
    fn stock(pty: &HandoffPty) -> std::io::Result<TransferKit> {
        Ok(TransferKit {
            input: pty.input.try_clone()?,
            output: pty.output.try_clone()?,
            signal: pty.signal.as_ref().map(|h| h.try_clone()).transpose()?,
            keepalive: pty
                .keepalive
                .iter()
                .map(|h| h.try_clone())
                .collect::<std::io::Result<_>>()?,
        })
    }
}

/// Serialize a quiesced session's LIVE screen as a VT byte stream — the
/// `state` payload of a tab move, replayed into the receiver's fresh parser
/// as a reader preface. The sender's grid matched conhost's buffer (the
/// settler lane guarantees it), so reproducing it verbatim keeps every
/// later absolute cursor position conhost emits landing where it expects.
///
/// Covered: the scrollback (printed as flowing text so it lands in the
/// receiver's history, wrapped logical lines staying joined; a 4 MiB budget
/// drops the OLDEST rows first), every visible cell (chars + zerowidth
/// combiners + full SGR, colors resolved against the live palette; default
/// fg/bg stay symbolic so the receiver's default-color handling is
/// preserved), cursor position / visibility / DECSCUSR shape, and the
/// wire-visible mode bits (alt screen, app cursor/keypad, wrap, bracketed
/// paste, focus reporting, the mouse suite, kitty keyboard flags).
/// While the alt screen is active, BOTH screens travel: the hidden
/// primary (with its scrollback) replays first, ?1049h saves its parked
/// cursor, then the alt content paints on top — leaving the alt screen
/// after the move shows exactly what the sender's ?1049l would have.
/// Deliberately dropped: the VISIBLE rows' WRAPLINE continuity (they are
/// painted via absolute CUP; the visible result is identical, only
/// selection line-joins differ until the next repaint) and DECSTBM
/// (ConPTY's renderer emits absolute positions, never scroll regions).
///
/// Call AFTER `quiesce_for_transfer`: the reader is dead, so the Term can
/// no longer change under the serialization.
///
/// `shipped_images`: ids whose image data travels alongside this replay
/// (see [`image_payloads`]) — their Unicode placeholder cells are re-emitted
/// verbatim so the receiver's renderer can place the carried images; every
/// other placeholder blanks, exactly the pre-transport behavior.
pub fn replay_bytes(
    session: &TerminalSession,
    shipped_images: &std::collections::HashSet<u32>,
) -> Vec<u8> {
    use std::fmt::Write as _;

    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

    let term = session.term.lock();
    let mode = *term.mode();
    let mut out = String::new();

    // Modes first (?1049 is NOT here: entering the alt screen happens
    // between the two screen replays below, so the primary paints first).
    if mode.contains(TermMode::APP_CURSOR) {
        out.push_str("\x1b[?1h");
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    if !mode.contains(TermMode::LINE_WRAP) {
        out.push_str("\x1b[?7l");
    }
    for (bit, seq) in [
        (TermMode::BRACKETED_PASTE, "\x1b[?2004h"),
        (TermMode::FOCUS_IN_OUT, "\x1b[?1004h"),
        (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h"),
        (TermMode::MOUSE_DRAG, "\x1b[?1002h"),
        (TermMode::MOUSE_MOTION, "\x1b[?1003h"),
        (TermMode::UTF8_MOUSE, "\x1b[?1005h"),
        (TermMode::SGR_MOUSE, "\x1b[?1006h"),
        (TermMode::ALTERNATE_SCROLL, "\x1b[?1007h"),
    ] {
        if mode.contains(bit) {
            out.push_str(seq);
        }
    }
    let kitty = (mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) as u8)
        | ((mode.contains(TermMode::REPORT_EVENT_TYPES) as u8) << 1)
        | ((mode.contains(TermMode::REPORT_ALTERNATE_KEYS) as u8) << 2)
        | ((mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) as u8) << 3)
        | ((mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) as u8) << 4);
    if kitty != 0 {
        let _ = write!(out, "\x1b[={kitty};1u");
    }

    // One SGR parameter list per cell run. Colors resolve through the live
    // palette (OSC 4 redefinitions included); Foreground/Background stay
    // symbolic so `default bg is unpainted`-style renderer behavior holds.
    let color_params = |s: &mut String, color: Color, is_fg: bool| {
        let base = if is_fg { 38 } else { 48 };
        let resolved = match color {
            Color::Named(NamedColor::Foreground) if is_fg => return,
            Color::Named(NamedColor::Background) if !is_fg => return,
            Color::Spec(rgb) => Some(rgb),
            Color::Named(n) => crate::query_color_rgb(term.colors(), n as usize),
            Color::Indexed(i) => crate::query_color_rgb(term.colors(), i as usize),
        };
        if let Some(rgb) = resolved {
            let _ = write!(s, ";{base};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    };
    let sgr_of = |cell: &alacritty_terminal::term::cell::Cell| -> String {
        let mut s = String::new();
        let f = cell.flags;
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "4:2"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::BLINK, "5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if f.contains(flag) {
                s.push(';');
                s.push_str(code);
            }
        }
        color_params(&mut s, cell.fg, true);
        color_params(&mut s, cell.bg, false);
        s
    };
    // One cell → text+SGR emitter, shared by the history and screen loops.
    // `last_link` carries the open OSC 8 hyperlink: transitions re-emit the
    // link (same id/uri, so the receiver's parser coalesces the spans back
    // into one logical hyperlink) and `None` closes it — without this a
    // moved tab's links degrade to plain text.
    let emit_cell = |out: &mut String,
                     last: &mut Option<String>,
                     last_link: &mut Option<alacritty_terminal::term::cell::Hyperlink>,
                     cell: &alacritty_terminal::term::cell::Cell| {
        // The wide char itself advances the parser two columns; its spacer
        // must not emit anything or the row would overflow.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            return;
        }
        let link = cell.hyperlink();
        if *last_link != link {
            match &link {
                Some(l) => {
                    let _ = write!(out, "\x1b]8;id={};{}\x1b\\", l.id(), l.uri());
                }
                None => out.push_str("\x1b]8;;\x1b\\"),
            }
            *last_link = link;
        }
        let sgr = sgr_of(cell);
        if last.as_ref() != Some(&sgr) {
            let _ = write!(out, "\x1b[0{sgr}m");
            *last = Some(sgr);
        }
        // A kitty/sixel placeholder carries its image id in the fg color
        // (24-bit spec or palette index — the VALUE is the id, never
        // resolved through the palette). When that image rides the move,
        // re-emit the cell verbatim under an exact id-preserving SGR so the
        // receiver's renderer can place the carried image; otherwise blank
        // it (a placeholder without its image would render as tofu).
        if cell.c == crate::kitty_graphics::PLACEHOLDER {
            let id = match cell.fg {
                Color::Spec(rgb) => {
                    Some(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32)
                }
                Color::Indexed(i) => Some(i as u32),
                _ => None,
            };
            match (id, &cell.fg) {
                (Some(id), Color::Spec(rgb)) if shipped_images.contains(&id) => {
                    let _ = write!(out, "\x1b[0;38;2;{};{};{}m", rgb.r, rgb.g, rgb.b);
                    *last = None;
                    out.push(cell.c);
                    if let Some(extra) = cell.zerowidth() {
                        out.extend(extra);
                    }
                }
                (Some(id), Color::Indexed(i)) if shipped_images.contains(&id) => {
                    let _ = write!(out, "\x1b[0;38;5;{i}m");
                    *last = None;
                    out.push(cell.c);
                    if let Some(extra) = cell.zerowidth() {
                        out.extend(extra);
                    }
                }
                _ => out.push(' '),
            }
            return;
        }
        out.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            out.extend(extra);
        }
    };

    type CellGrid = alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>;

    // ── Scrollback, oldest first, BEFORE a visible screen: each history
    // row is printed as flowing text, so it scrolls straight into the
    // receiver's history. A row whose last cell carries WRAPLINE omits its
    // newline — the next row joins it, reproducing wrapped logical lines.
    // A byte budget keeps the payload under the IPC frame cap; the OLDEST
    // rows fall off first, like any scrollback. Each row opens with its
    // own SGR reset so dropping whole rows at the front can never tear an
    // escape run. Trailing padding newlines push the tail of the flow out
    // of the viewport — the visible screen is painted OVER the viewport
    // next, and a history row still inside it would be overwritten instead
    // of scrolled into history (exactly the printed rows scroll out; the
    // padding itself never reaches history).
    let flow_history = |out: &mut String, grid: &CellGrid| {
        let history = grid.history_size();
        if history == 0 {
            return;
        }
        use std::collections::VecDeque;
        const HISTORY_BUDGET: usize = 4 * 1024 * 1024;
        let mut rows_vt: VecDeque<String> = VecDeque::new();
        let mut total = 0usize;
        for i in (1..=history).rev() {
            let line = Line(-(i as i32));
            let mut row = String::new();
            let mut last: Option<String> = None;
            // Per-row link state (like the per-row SGR reset): rows can be
            // dropped from the front of the budget, so each must be
            // self-contained — a link spanning rows closes at the row edge
            // and re-opens on the next, which the parser merges by id/uri.
            let mut last_link = None;
            for col in 0..term.columns() {
                emit_cell(
                    &mut row,
                    &mut last,
                    &mut last_link,
                    &grid[line][Column(col)],
                );
            }
            if last_link.is_some() {
                row.push_str("\x1b]8;;\x1b\\");
            }
            let wrapped = grid[line][Column(term.columns() - 1)]
                .flags
                .contains(Flags::WRAPLINE);
            if !wrapped {
                row.push_str("\r\n");
            }
            total += row.len();
            rows_vt.push_back(row);
            while total > HISTORY_BUDGET {
                if let Some(dropped) = rows_vt.pop_front() {
                    total -= dropped.len();
                }
            }
        }
        for row in rows_vt {
            out.push_str(&row);
        }
        out.push_str(&"\r\n".repeat(term.screen_lines().saturating_sub(1)));
    };
    // A visible screen, absolute CUP per row.
    let paint_screen = |out: &mut String, grid: &CellGrid| {
        let mut last_sgr: Option<String> = None;
        let mut last_link = None;
        for row in 0..term.screen_lines() {
            let _ = write!(out, "\x1b[{};1H", row + 1);
            for col in 0..term.columns() {
                emit_cell(
                    out,
                    &mut last_sgr,
                    &mut last_link,
                    &grid[Line(row as i32)][Column(col)],
                );
            }
        }
        if last_link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
    };

    // While the alt screen is active, the PRIMARY screen (with its
    // scrollback) lives in the inactive grid: replay it FIRST, park its
    // cursor (?1049h saves it), then enter the alt screen and paint the
    // alt content on top — leaving the alt screen after the move restores
    // exactly what the sender's ?1049l would have shown.
    if mode.contains(TermMode::ALT_SCREEN) {
        let primary = term.inactive_grid();
        flow_history(&mut out, primary);
        paint_screen(&mut out, primary);
        let p = primary.cursor.point;
        let _ = write!(out, "\x1b[0m\x1b[{};{}H", p.line.0 + 1, p.column.0 + 1);
        out.push_str("\x1b[?1049h");
    }

    let grid = term.grid();
    // (While the alt screen is active it has no history of its own —
    // flow_history returns immediately there.)
    flow_history(&mut out, grid);
    paint_screen(&mut out, grid);

    // Cursor: position, DECSCUSR shape, visibility — after the paint, since
    // painting moved it.
    let point = grid.cursor.point;
    let _ = write!(
        out,
        "\x1b[0m\x1b[{};{}H",
        point.line.0 + 1,
        point.column.0 + 1
    );
    let style = term.cursor_style();
    let decscusr = match (style.shape, style.blinking) {
        (CursorShape::Block, true) => 1,
        (CursorShape::Block, false) => 2,
        (CursorShape::Underline, true) => 3,
        (CursorShape::Underline, false) => 4,
        (CursorShape::Beam, true) => 5,
        (CursorShape::Beam, false) => 6,
        // Hidden rides ?25l below; HollowBlock is a focus artifact.
        _ => 2,
    };
    let _ = write!(out, "\x1b[{decscusr} q");
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25l");
    }
    out.into_bytes()
}

/// PNG-encode the session's image store for a tab move: `(id, cols, rows,
/// png)` per image, insertion order. Newest images win under a byte budget
/// (encoded size) so the state frame stays inside the IPC cap alongside the
/// 4 MiB history budget; what does not fit simply drops — its placeholders
/// blank in the replay, exactly the pre-transport behavior.
pub fn image_payloads(session: &TerminalSession) -> Vec<(u32, u16, u16, Vec<u8>)> {
    const IMAGE_BUDGET: usize = 1536 * 1024;
    let mut picked = Vec::new();
    let mut total = 0usize;
    // Walk newest-first so the budget keeps what the user saw last.
    for (id, stored) in session.images.snapshot_all().into_iter().rev() {
        let Some(bytes) = stored.image.as_bytes(0) else {
            continue;
        };
        let size = stored.image.size(0);
        let (w, h) = (size.width.0 as u32, size.height.0 as u32);
        if w == 0 || h == 0 {
            continue;
        }
        // BGRA (gpui atlas order) back to RGBA for the encoder.
        let mut rgba = bytes.to_vec();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let Some(buf) = image::RgbaImage::from_raw(w, h, rgba) else {
            continue;
        };
        let mut png = Vec::new();
        if image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_err()
        {
            continue;
        }
        if total + png.len() > IMAGE_BUDGET {
            continue;
        }
        total += png.len();
        picked.push((id, stored.cols, stored.rows, png));
    }
    picked.reverse(); // back to insertion order
    picked
}

/// winconpty's `PTY_SIGNAL_RESIZE_WINDOW` message: three little-endian u16s —
/// the signal id (8), then the new column and row counts. ConPTY's signal
/// pipe carries no pixel sizes.
pub fn resize_signal_packet(cols: u16, rows: u16) -> [u8; 6] {
    const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;
    let mut buf = [0u8; 6];
    buf[0..2].copy_from_slice(&PTY_SIGNAL_RESIZE_WINDOW.to_le_bytes());
    buf[2..4].copy_from_slice(&cols.to_le_bytes());
    buf[4..6].copy_from_slice(&rows.to_le_bytes());
    buf
}

/// Resizer over the ConPTY signal pipe. Doubles as the keepalive anchor (see
/// module docs): the handoff's lifetime handles drop with the session.
struct SignalResizer {
    signal: Option<FairMutex<File>>,
    _keepalive: Vec<OwnedHandle>,
}

impl PtyResizer for SignalResizer {
    fn resize(&self, cols: u16, rows: u16, _pw: u16, _ph: u16) -> Result<()> {
        if let Some(signal) = &self.signal {
            let mut f = signal.lock();
            f.write_all(&resize_signal_packet(cols, rows))?;
            f.flush()?;
        }
        Ok(())
    }
}

/// Build a live [`TerminalSession`] over an inherited ConPTY. The console
/// closing its output end reads as EOF and flips the session disconnected,
/// exactly like a spawned shell exiting.
pub fn build_handoff_session(
    cols: u16,
    rows: u16,
    pty: HandoffPty,
    xtversion_identity: &str,
) -> Result<TerminalSession> {
    build_handoff_session_with_preface(cols, rows, pty, Vec::new(), xtversion_identity)
}

/// [`build_handoff_session`] with `preface` bytes applied by the parse
/// thread as one atomic first chunk, before anything from the pipe and
/// before any reflow — a tab move's [`replay_bytes`] lands here, so the
/// receiver's grid starts as a copy of the sender's instead of blank.
pub fn build_handoff_session_with_preface(
    cols: u16,
    rows: u16,
    pty: HandoffPty,
    preface: Vec<u8>,
    xtversion_identity: &str,
) -> Result<TerminalSession> {
    // Stock the transfer duplicates before the handles disappear into File
    // boxes and worker threads — this is the only moment the full set is
    // still in one hand.
    let transfer = TransferKit::stock(&pty)?;
    let reader: Box<dyn Read + Send> = Box::new(File::from(pty.output));
    let writer: Box<dyn Write + Send> = Box::new(File::from(pty.input));
    let resizer: Arc<dyn PtyResizer> = Arc::new(SignalResizer {
        signal: pty.signal.map(|h| FairMutex::new(File::from(h))),
        _keepalive: pty.keepalive,
    });
    let session = crate::pty_session::build_terminal_session_with_preface(
        cols,
        rows,
        reader,
        Arc::new(FairMutex::new(writer)),
        resizer,
        xtversion_identity,
        preface,
    )?;
    // This session IS a ConPTY: reflow like conhost or drift, and don't
    // advertise kitty keyboard (see mark_conpty's docs in lib.rs).
    session.mark_conpty();
    *session.transfer.lock() = Some(transfer);
    Ok(session)
}

#[cfg(test)]
mod transfer_tests {
    use std::io::{Read as _, Write as _};
    use std::os::windows::io::OwnedHandle;

    use super::*;

    /// ConPTY width changes must reflow exactly like conhost's
    /// `TextBuffer::Reflow`: content wraps on a shrink (blank rows
    /// preserved, cursor following logically — REFLOW_JANK counts the
    /// blank cell in front of the cursor as content) and the round trip
    /// back rejoins everything: the grow restores the original layout.
    #[test]
    fn conpty_width_reflow_matches_conhost() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // A short row, a BLANK row, then a 56-char row with the cursor
        // parked at its end — the shell-prompt shape that exposed both the
        // wrap drift and any blank-row mis-accounting.
        let mut feed = b"AAA\r\n\r\n".to_vec();
        feed.extend(std::iter::repeat_n(b'P', 56));
        out_write.write_all(&feed).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let snap = session.snapshot.lock();
                if snap.cells[2][55].c == 'P' {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "output never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        session.resize(45, 24, (8.0, 16.0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let snap = session.snapshot.lock();
                if snap.cells[0].len() == 45 {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "shrink never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let snap = session.snapshot.lock();
            let text = |r: usize| -> String {
                snap.cells[r]
                    .iter()
                    .map(|c| c.c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            };
            assert_eq!(text(0), "AAA", "short rows stay put");
            assert_eq!(text(1), "", "blank rows are preserved");
            assert_eq!(text(2), "P".repeat(45), "long rows wrap at the new width");
            assert_eq!(
                text(3),
                "P".repeat(11),
                "the remainder lands on the next row"
            );
            assert_eq!(
                snap.cursor,
                (3, 11),
                "cursor follows its logical position (row 3, 56 − 45)"
            );
        }

        session.resize(80, 24, (8.0, 16.0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let snap = session.snapshot.lock();
                if snap.cells[0].len() == 80 {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "grow never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snap = session.snapshot.lock();
        let text = |r: usize| -> String {
            snap.cells[r]
                .iter()
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert_eq!(text(0), "AAA");
        assert_eq!(text(1), "", "blank rows survive the round trip");
        assert_eq!(
            text(2),
            "P".repeat(56),
            "the grow rejoins the wrapped pair — the round trip is lossless"
        );
        assert_eq!(text(3), "", "the remainder row folded back");
        assert_eq!(snap.cursor, (2, 56), "cursor returns to its origin");
    }

    /// The sending half of a tab move, distilled: quiesce provably stops OUR
    /// reader — bytes written afterwards never reach this grid — while the
    /// pipe, including data already buffered in it, stays intact for the
    /// receiver, read here through the birth-time transfer duplicate.
    #[test]
    fn quiesce_stops_the_reader_and_the_kit_inherits_the_pipe() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // Prove the reader is alive first, so "MOVED never lands" below
        // means quiesce — not a dead pipeline.
        out_write.write_all(b"hi").unwrap();
        let row0 = || -> String {
            session.snapshot.lock().cells[0]
                .iter()
                .map(|c| c.c)
                .collect()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !row0().starts_with("hi") {
            assert!(std::time::Instant::now() < deadline, "output never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        session.quiesce_for_transfer().expect("quiesce");

        out_write.write_all(b"MOVED").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            row0().starts_with("hi") && !row0().contains("MOVED"),
            "reader must be dead after quiesce, got {:?}",
            row0()
        );

        // The receiver's view: the transfer duplicate reads the bytes our
        // stopped reader left in the pipe.
        let kit = session
            .transfer
            .lock()
            .take()
            .expect("kit stocked at birth");
        let mut peer = File::from(kit.output);
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"MOVED");
    }

    /// A moved tab must ARRIVE wearing the sender's screen: serialize a
    /// session showing colors, a wide char, a mode bit and a parked cursor,
    /// replay the bytes into a fresh session (the receiver's preface path),
    /// and require the two snapshots to agree cell by cell.
    #[test]
    fn replay_reproduces_screen_cursor_and_modes() {
        use alacritty_terminal::term::TermMode;

        let script = "\x1b[?2004h\x1b[1;31mRED\x1b[0m plain 字\r\nsecond\x1b[5;7H".to_string();
        let a = crate::pty_session::build_test_session_with_output(40, 10, script.into_bytes());
        let wait_eof = |s: &TerminalSession| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.is_connected() {
                assert!(std::time::Instant::now() < deadline, "no EOF");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait_eof(&a);

        let replay = replay_bytes(&a, &Default::default());
        let b = crate::pty_session::build_test_session_with_output(40, 10, replay);
        wait_eof(&b);

        let sa = a.snapshot.lock().clone();
        let sb = b.snapshot.lock().clone();
        assert_eq!(sa.cursor, sb.cursor, "cursor must land where it was");
        assert_eq!(sa.cursor_shape, sb.cursor_shape);
        for (r, (ra, rb)) in sa.cells.iter().zip(&sb.cells).enumerate() {
            for (c, (ca, cb)) in ra.iter().zip(rb).enumerate() {
                assert_eq!(ca.c, cb.c, "char at {r},{c}");
                assert_eq!(ca.fg, cb.fg, "fg at {r},{c}");
                assert_eq!(ca.bg, cb.bg, "bg at {r},{c}");
                assert_eq!(ca.style, cb.style, "style at {r},{c}");
                assert_eq!(ca.display_width, cb.display_width, "width at {r},{c}");
            }
        }
        assert!(
            b.term.lock().mode().contains(TermMode::BRACKETED_PASTE),
            "mode bits must survive the move"
        );
    }

    /// Images survive the move: the store PNG-encodes losslessly, and the
    /// replay re-emits placeholder cells whose image made the payload while
    /// blanking the rest (the pre-transport behavior).
    #[test]
    fn image_store_and_placeholders_survive_the_move() {
        let script = crate::sixel::placeholder_bytes(7, 3, 1, 0);
        let a = crate::pty_session::build_test_session_with_output(40, 5, script);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while a.is_connected() {
            assert!(std::time::Instant::now() < deadline, "no EOF");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        #[rustfmt::skip]
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255,   0, 255, 0, 255,
            0, 0, 255, 255,   255, 255, 255, 255,
        ];
        assert!(a.images.insert_rgba(7, 2, 2, rgba.clone(), 3, 1));

        // The payload PNG decodes back to the exact pixels + placement.
        let payloads = image_payloads(&a);
        assert_eq!(payloads.len(), 1);
        let (id, c, r, png) = &payloads[0];
        assert_eq!((*id, *c, *r), (7, 3, 1));
        let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap();
        let out = img.to_rgba8();
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.into_raw(), rgba);

        // Shipped id → placeholders re-emitted; not shipped → blanked.
        let shipped: std::collections::HashSet<u32> = [7].into_iter().collect();
        let with = String::from_utf8(replay_bytes(&a, &shipped)).unwrap();
        assert!(with.contains(crate::kitty_graphics::PLACEHOLDER));
        let without = String::from_utf8(replay_bytes(&a, &Default::default())).unwrap();
        assert!(!without.contains(crate::kitty_graphics::PLACEHOLDER));
    }

    /// OSC 8 hyperlinks must survive the move: the replay re-emits the link
    /// around each linked run, so the receiver's cells carry the same id/uri
    /// instead of degrading to plain text.
    #[test]
    fn replay_carries_osc8_hyperlinks() {
        use alacritty_terminal::index::{Column, Line};

        let script =
            "plain \x1b]8;id=k1;https://example.com/a\x1b\\LINK\x1b]8;;\x1b\\ tail".to_string();
        let a = crate::pty_session::build_test_session_with_output(40, 5, script.into_bytes());
        let wait_eof = |s: &TerminalSession| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.is_connected() {
                assert!(std::time::Instant::now() < deadline, "no EOF");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait_eof(&a);

        let replay = String::from_utf8(replay_bytes(&a, &Default::default())).unwrap();
        assert!(
            replay.contains("\x1b]8;id=k1;https://example.com/a\x1b\\"),
            "replay must re-open the hyperlink: {replay:?}"
        );
        let b = crate::pty_session::build_test_session_with_output(40, 5, replay.into_bytes());
        wait_eof(&b);

        let term = b.term.lock();
        let grid = term.grid();
        // Row 0 = "plain LINK tail": cols 6..=9 carry the link, others none.
        let link = grid[Line(0)][Column(6)]
            .hyperlink()
            .expect("LINK cell must carry a hyperlink after the move");
        assert_eq!(link.uri(), "https://example.com/a");
        assert_eq!(link.id(), "k1");
        assert!(grid[Line(0)][Column(0)].hyperlink().is_none());
        assert!(grid[Line(0)][Column(11)].hyperlink().is_none());
    }

    /// Bytes the reader consumed but the parser has not yet applied — an
    /// open ?2026 sync buffer is the deterministic way to create that
    /// state — must reach the Term BEFORE quiesce returns: the pipe no
    /// longer holds them for the receiver, so a replay serialized without
    /// them loses them for good. Quiesce joins the parser after the
    /// reader; the parser's EOF path force-flushes the open sync buffer.
    #[test]
    fn quiesce_drains_sync_buffered_bytes_into_the_replay() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            40,
            5,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // BSU with no ESU: the parser buffers TRAPPED without applying it.
        // 50ms lets the reader consume the bytes (well under the 150ms
        // sync deadline, so the parser still holds them when we quiesce —
        // and if timing drifts past it, the deadline flush makes the
        // assertion pass identically).
        out_write.write_all(b"\x1b[?2026hTRAPPED").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));

        session.quiesce_for_transfer().expect("quiesce");
        let replay = String::from_utf8(replay_bytes(&session, &Default::default())).unwrap();
        assert!(
            replay.contains("TRAPPED"),
            "bytes in flight at quiesce were lost by the move"
        );
    }

    /// A resize racing the preface must lose: the replay's absolute
    /// positions assume the birth geometry, so the settler holds every
    /// reflow until the parse thread has applied the preface as one atomic
    /// chunk. Before this ordering, the reader chunked the preface and an
    /// adopt-time reflow could slice in, scattering the remainder across
    /// the new grid — the "input jumps to the bottom after merging into a
    /// differently-sized window" field report.
    #[test]
    fn resize_waits_for_the_preface() {
        use std::fmt::Write as _;

        // A 24-row screen painted via absolute CUP + a parked cursor —
        // the shape replay_bytes emits.
        let mut preface = String::new();
        for r in 1..=24 {
            let _ = write!(preface, "\x1b[{r};1HROW-{r}");
        }
        preface.push_str("\x1b[24;9H");

        let (out_read, _out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session_with_preface(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            preface.into_bytes(),
            "test-identity",
        )
        .expect("session with preface");

        // Race the adopt-time fit: resize immediately after construction.
        session.resize(80, 40, (8.0, 16.0));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let snap = session.snapshot.lock();
                if snap.cells.len() == 40 {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "resize never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snap = session.snapshot.lock().clone();
        let row_text = |r: usize| -> String {
            snap.cells[r]
                .iter()
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        // Top-anchored ConPTY growth over a FULLY-applied preface: every
        // row where the replay put it, blanks below, cursor still parked.
        assert_eq!(row_text(0), "ROW-1");
        assert_eq!(row_text(23), "ROW-24");
        for r in 24..40 {
            assert_eq!(row_text(r), "", "grown rows must be blank (row {r})");
        }
        assert_eq!(snap.cursor, (23, 8), "cursor must stay parked");
    }

    /// While the alt screen is active BOTH screens must travel: the
    /// receiver shows the alt content, and leaving the alt screen
    /// (?1049l — what a fullscreen app emits on exit) reveals the primary
    /// screen, its cursor and its scrollback, exactly as on the sender.
    #[test]
    fn replay_carries_the_primary_hidden_under_the_alt_screen() {
        use alacritty_terminal::grid::Dimensions as _;
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::TermMode;

        // Primary: 12 numbered rows through a 40x5 viewport (history!),
        // then enter the alt screen and paint fullscreen-app content.
        let mut script = String::new();
        for i in 1..=12 {
            script.push_str(&format!("P{i}\r\n"));
        }
        script.push_str("\x1b[?1049h\x1b[2J\x1b[1;1HALT-CONTENT");
        let a = crate::pty_session::build_test_session_with_output(40, 5, script.into_bytes());
        let wait_eof = |s: &TerminalSession| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.is_connected() {
                assert!(std::time::Instant::now() < deadline, "no EOF");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait_eof(&a);
        assert!(a.term.lock().mode().contains(TermMode::ALT_SCREEN));

        let replay = replay_bytes(&a, &Default::default());

        // The receiver as adopted: alt screen active, alt content visible.
        let b = crate::pty_session::build_test_session_with_output(40, 5, replay.clone());
        wait_eof(&b);
        assert!(b.term.lock().mode().contains(TermMode::ALT_SCREEN));
        let row0: String = b.snapshot.lock().cells[0].iter().map(|c| c.c).collect();
        assert!(row0.starts_with("ALT-CONTENT"), "got {row0:?}");

        // The receiver after the app exits: ?1049l must reveal the moved
        // primary — compare against the sender's hidden (inactive) grid.
        let mut with_exit = replay;
        with_exit.extend_from_slice(b"\x1b[?1049l");
        let c = crate::pty_session::build_test_session_with_output(40, 5, with_exit);
        wait_eof(&c);
        let ta = a.term.lock();
        let tc = c.term.lock();
        let (pa, pc) = (ta.inactive_grid(), tc.grid());
        assert!(
            pa.history_size() > 0,
            "test setup must overflow the viewport"
        );
        assert_eq!(pa.history_size(), pc.history_size(), "primary history");
        assert_eq!(pa.cursor.point, pc.cursor.point, "primary cursor");
        let text = |g: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
                    line: i32|
         -> String {
            (0..ta.columns())
                .map(|col| g[Line(line)][Column(col)].c)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        for line in -(pa.history_size() as i32)..(ta.screen_lines() as i32) {
            assert_eq!(text(pa, line), text(pc, line), "primary row {line}");
        }
    }

    /// Scrollback rides the move: rows that scrolled out of the sender's
    /// viewport reappear in the receiver's HISTORY (same count — wrapped
    /// physical rows included, since WRAPLINE rows omit their newline in
    /// the replay), with the oldest row intact.
    #[test]
    fn replay_carries_scrollback_history() {
        use alacritty_terminal::grid::Dimensions as _;
        use alacritty_terminal::index::{Column, Line};

        // 20 numbered lines + one 60-char wrapping line through a 40x5
        // viewport: plenty lands in history, including a wrapped pair.
        let mut script = String::new();
        script.push_str(&"W".repeat(60));
        script.push_str("\r\n");
        for i in 1..=20 {
            script.push_str(&format!("H{i}\r\n"));
        }
        let a = crate::pty_session::build_test_session_with_output(40, 5, script.into_bytes());
        let wait_eof = |s: &TerminalSession| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.is_connected() {
                assert!(std::time::Instant::now() < deadline, "no EOF");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait_eof(&a);

        let replay = replay_bytes(&a, &Default::default());
        let b = crate::pty_session::build_test_session_with_output(40, 5, replay);
        wait_eof(&b);

        let history = |s: &TerminalSession| s.term.lock().grid().history_size();
        assert!(history(&a) > 10, "test setup must overflow the viewport");
        assert_eq!(
            history(&a),
            history(&b),
            "every history row must survive the move"
        );
        let row_text = |s: &TerminalSession, line: i32| -> String {
            let t = s.term.lock();
            (0..t.columns())
                .map(|c| t.grid()[Line(line)][Column(c)].c)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let oldest = -(history(&a) as i32);
        assert_eq!(row_text(&a, oldest), "W".repeat(40), "oldest row intact");
        for line in oldest..0 {
            assert_eq!(
                row_text(&a, line),
                row_text(&b, line),
                "history row {line} must match"
            );
        }
    }

    /// After quiesce the settler must go silent: no resize — not even the
    /// pending-flush on session drop — may reach the signal pipe, or it
    /// would fight the receiver's geometry (ConPTY never repaints).
    #[test]
    fn quiesce_seals_resizes_off_the_signal_pipe() {
        let (out_read, _out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let (mut sig_read, sig_write) = std::io::pipe().expect("signal pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: Some(OwnedHandle::from(sig_write)),
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // A live settler first (leading edge applies immediately)…
        session.resize(100, 30, (8.0, 16.0));
        let mut first = [0u8; 6];
        sig_read.read_exact(&mut first).unwrap();
        assert_eq!(first, resize_signal_packet(100, 30));

        // …then seal, resize again, and drain to EOF: nothing may follow.
        session.quiesce_for_transfer().expect("quiesce");
        session.resize(120, 40, (8.0, 16.0));
        drop(session.transfer.lock().take()); // the kit's signal dup too
        drop(session);
        let mut rest = Vec::new();
        sig_read.read_to_end(&mut rest).unwrap();
        assert!(
            rest.is_empty(),
            "sealed session leaked a resize onto the signal pipe: {rest:?}"
        );
    }
}

#[cfg(test)]
mod resize_snapshot_tests {
    use std::io::Write as _;
    use std::os::windows::io::OwnedHandle;

    use super::*;

    /// Resize must republish the snapshot itself: the reader thread only
    /// refreshes it on PTY output, so a quiet screen would otherwise keep
    /// the old geometry — the prompt sits clipped out of view until the
    /// next byte arrives (the "hidden until Enter" bug).
    #[test]
    fn resize_republishes_the_snapshot_without_output() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // Land one output burst, then go quiet (the write end stays open, so
        // the reader blocks — any later snapshot change is resize's doing).
        out_write.write_all(b"hi").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let row0: String = session.snapshot.lock().cells[0]
                .iter()
                .map(|c| c.c)
                .collect();
            if row0.starts_with("hi") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "output never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        session.resize(100, 30, (8.0, 16.0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let snap = session.snapshot.lock();
                if (snap.cells[0].len(), snap.cells.len()) == (100, 30) {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "snapshot must pick up the new geometry with no output"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// ConPTY sessions must grow like conhost: content pinned to the top,
    /// blank rows appended below, nothing pulled back from scrollback.
    /// ConPTY emits nothing on resize and computes every later absolute
    /// cursor position against that layout — Alacritty's native
    /// bottom-anchored growth makes typed input land mid-screen after a
    /// shrink+grow storm.
    #[test]
    fn conpty_growth_is_top_anchored() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        // 40 numbered lines: 24 visible, the rest already in scrollback.
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("L{i}\r\n"));
        }
        out_write.write_all(feed.as_bytes()).unwrap();
        let row_text = |r: usize| -> String {
            session.snapshot.lock().cells[r]
                .iter()
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while row_text(22) != "L40" {
            assert!(
                std::time::Instant::now() < deadline,
                "L40 never landed (row 22 = {:?})",
                row_text(22)
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let top_before = row_text(0);
        session.resize(80, 30, (8.0, 16.0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.snapshot.lock().cells.len() != 30 {
            assert!(std::time::Instant::now() < deadline, "resize never applied");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            row_text(0),
            top_before,
            "growth must not pull scrollback down (top row changed)"
        );
        assert_eq!(row_text(22), "L40", "content must stay put");
        for r in 24..30 {
            assert_eq!(row_text(r), "", "grown rows must be blank (row {r})");
        }
    }

    /// A resize burst must reach the PTY as the leading + settled sizes
    /// only. The transient widths of a window drag would wrap and unwrap
    /// long rows in conhost's buffer with row accounting different from
    /// ours — and ConPTY never repaints — so every intermediate size the
    /// PTY never sees is drift avoided.
    #[test]
    fn pty_resize_bursts_coalesce_to_leading_and_settled() {
        use std::io::Read as _;

        let (out_read, _out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let (mut sig_read, sig_write) = std::io::pipe().expect("signal pipe");
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: Some(OwnedHandle::from(sig_write)),
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("session over pipes");

        for c in [78u16, 75, 70, 66, 60, 55, 50, 45, 60, 75, 100] {
            session.resize(c, 30, (8.0, 16.0));
        }
        // Dropping the session disconnects the settler, which applies the
        // trailing edge and exits — closing the last signal write end.
        drop(session);

        let mut bytes = Vec::new();
        sig_read.read_to_end(&mut bytes).expect("drain signal pipe");
        assert_eq!(bytes.len() % 6, 0, "whole resize packets only");
        let packets: Vec<(u16, u16)> = bytes
            .chunks(6)
            .map(|p| {
                assert_eq!(u16::from_le_bytes([p[0], p[1]]), 8, "resize signal id");
                (
                    u16::from_le_bytes([p[2], p[3]]),
                    u16::from_le_bytes([p[4], p[5]]),
                )
            })
            .collect();
        assert!(
            packets.len() <= 3,
            "burst must coalesce, PTY saw {packets:?}"
        );
        assert_eq!(packets.first(), Some(&(78, 30)), "leading edge is instant");
        assert_eq!(packets.last(), Some(&(100, 30)), "settled size lands last");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_eof(session: &TerminalSession) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader thread must reach EOF");
    }

    #[test]
    fn resize_packet_is_winconpty_wire_format() {
        assert_eq!(
            resize_signal_packet(120, 40),
            [8, 0, 120, 0, 40, 0],
            "u16 LE triple: signal id 8, cols, rows"
        );
        assert_eq!(resize_signal_packet(0x1234, 0x00FF)[2..4], [0x34, 0x12]);
    }

    /// Console-side output flows into the grid; terminal-side input lands on
    /// the console's end of the input pipe; EOF disconnects — the same
    /// lifecycle a spawned PTY has, but over inherited pipe handles.
    #[test]
    fn handoff_session_round_trips_both_pipes() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (mut in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            10,
            2,
            HandoffPty {
                input: in_write.into(),
                output: out_read.into(),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-terminal 0.0",
        )
        .expect("session over inherited pipes");

        out_write.write_all(b"hi").unwrap();
        drop(out_write);
        wait_for_eof(&session);
        let row0: String = session.snapshot.lock().cells[0]
            .iter()
            .map(|c| c.c)
            .collect();
        assert!(row0.starts_with("hi"), "got {row0:?}");

        session.send_bytes(b"ls\r");
        let mut buf = [0u8; 3];
        in_read.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ls\r");
    }

    /// `TerminalSession::resize` reaches the signal pipe as a winconpty
    /// resize packet; a session without a signal pipe swallows resizes.
    #[test]
    fn resize_goes_out_over_the_signal_pipe() {
        let (out_read, out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let (mut sig_read, sig_write) = std::io::pipe().expect("signal pipe");
        let session = build_handoff_session(
            10,
            2,
            HandoffPty {
                input: in_write.into(),
                output: out_read.into(),
                signal: Some(sig_write.into()),
                keepalive: Vec::new(),
            },
            "test-terminal 0.0",
        )
        .expect("session with signal pipe");

        session.resize(120, 40, (8.0, 16.0));
        let mut buf = [0u8; 6];
        sig_read.read_exact(&mut buf).unwrap();
        assert_eq!(buf, resize_signal_packet(120, 40));
        drop(out_write);
    }
}
