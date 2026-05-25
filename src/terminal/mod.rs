pub mod keys;
pub mod pty_session;
pub mod renderer;

use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::{event::VoidListener, Term};

pub struct TerminalSession {
    #[allow(dead_code)]
    pub term: Arc<Mutex<Term<VoidListener>>>,
    pub writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    pub snapshot: Arc<Mutex<GridSnapshot>>,
    pub connected: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    #[allow(dead_code)]
    pub error: Arc<Mutex<Option<String>>>,
}

impl TerminalSession {
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn send_bytes(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
        }
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
}

impl GridSnapshot {
    pub fn blank(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![SnapshotCell::blank(); cols]; rows],
            cursor: (0, 0),
        }
    }
}

#[derive(Clone)]
pub struct SnapshotCell {
    pub c: char,
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    #[allow(dead_code)]
    pub bold: bool,
    #[allow(dead_code)]
    pub underline: bool,
}

impl SnapshotCell {
    pub fn blank() -> Self {
        Self {
            c: ' ',
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
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

/// alacritty_terminal の Cell/Color → SnapshotCell に変換
pub fn take_snapshot(term: &Term<VoidListener>) -> GridSnapshot {
    use alacritty_terminal::term::cell::Flags;

    let content = term.renderable_content();
    let cols = term.columns();
    let rows = term.screen_lines();
    let mut cells = vec![vec![SnapshotCell::blank(); cols]; rows];

    for indexed in content.display_iter {
        let row = indexed.point.line.0 as usize;
        let col = indexed.point.column.0;
        if row < rows && col < cols {
            cells[row][col] = SnapshotCell {
                c: indexed.c,
                fg: resolve_color(indexed.fg, content.colors),
                bg: resolve_color(indexed.bg, content.colors),
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
            let ResolvedColor::Rgb(r, g, b) = base else { return None };
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
            let ResolvedColor::Rgb(r, g, b) = base else { return None };
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
        event::VoidListener,
        term::{test::TermSize, Config},
        vte::ansi::{Processor, StdSyncHandler},
        Term,
    };

    fn make_term(cols: usize, rows: usize) -> Term<VoidListener> {
        Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            VoidListener,
        )
    }

    fn advance_bytes(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser = Processor::<StdSyncHandler>::new();
        for &byte in bytes {
            parser.advance(term, byte);
        }
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
