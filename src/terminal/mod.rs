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
        Color::Named(named) => {
            if let Some(rgb) = table[named] {
                ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b)
            } else {
                ResolvedColor::Default
            }
        }
        Color::Indexed(idx) => {
            if let Some(rgb) = table[idx as usize] {
                ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b)
            } else {
                ResolvedColor::Default
            }
        }
        Color::Spec(rgb) => ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
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
        parser.advance(term, bytes);
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
    fn initial_cursor_is_at_origin() {
        let term = make_term(10, 5);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cursor, (0, 0));
    }
}
