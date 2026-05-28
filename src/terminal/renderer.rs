use gpui::{div, px, rgba, FontWeight, IntoElement, ParentElement, Rgba, Styled};
use gpui_component::v_flex;

use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::{GridSnapshot, ResolvedColor, SnapshotCell};
use crate::theme::Colors;

/// Fixed cell width in pixels (tune against tmux box-drawing alignment).
pub const CELL_W: f32 = 7.8;
/// Fixed cell height in pixels.
pub const CELL_H: f32 = 20.0;

/// A styled run of consecutive terminal cells with identical visual properties.
///
/// Adjacent cells that share the same fg, bg, bold, and underline flags are merged
/// into a single `Run` for efficient GPUI rendering. The cursor cell is always its
/// own run (never merged with neighbours) so colours can be inverted independently.
pub(crate) struct Run {
    pub text: String,
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    /// Total display-column width of this run (sum of each cell's `display_width`).
    pub width: usize,
    pub bold: bool,
    pub underline: bool,
    /// True for the single run that sits at the cursor position.
    pub is_cursor: bool,
}

/// Map a resolved terminal color to a GPUI `Rgba` for text/background styling.
pub fn color_to_rgba(color: ResolvedColor) -> Rgba {
    match color {
        ResolvedColor::Rgb(r, g, b) => rgba(u32::from_be_bytes([r, g, b, 0xff])),
        ResolvedColor::Default => Colors::zouge(),
    }
}

/// Resolve the display fg/bg for a `Run`, applying block-cursor inversion when needed.
///
/// Returns `(fg: Rgba, bg: Option<Rgba>)`.
/// `None` bg means transparent — the terminal background shows through.
fn resolve_run_colors(run: &Run) -> (Rgba, Option<Rgba>) {
    let resolved_fg = color_to_rgba(run.fg);
    let resolved_bg = match run.bg {
        ResolvedColor::Rgb(r, g, b) => Some(rgba(u32::from_be_bytes([r, g, b, 0xff]))),
        ResolvedColor::Default => None,
    };

    if run.is_cursor {
        // Block cursor: swap fg ↔ bg.
        // When bg is Default (transparent), fall back to a dark terminal-background tone
        // so the cursor block is always visible.
        let cursor_bg = resolved_fg;
        let cursor_fg = resolved_bg.unwrap_or_else(|| rgba(0x1e1e1eff));
        (cursor_fg, Some(cursor_bg))
    } else {
        (resolved_fg, resolved_bg)
    }
}

pub fn render_grid(snap: &GridSnapshot) -> impl IntoElement {
    let (cursor_row, cursor_col) = snap.cursor;
    v_flex()
        .font_family(MONO_FONT)
        .text_size(px(13.))
        .children(snap.cells.iter().enumerate().map(|(row_idx, row)| {
            let cur_col = if row_idx == cursor_row {
                Some(cursor_col)
            } else {
                None
            };
            div()
                .flex()
                .flex_row()
                .h(px(CELL_H))
                .children(coalesce_runs(row, cur_col).map(|run| {
                    let (fg_rgba, bg_opt) = resolve_run_colors(&run);
                    let mut el = div()
                        .child(run.text)
                        .w(px(CELL_W * run.width as f32))
                        .overflow_hidden()
                        .text_color(fg_rgba);
                    if let Some(bg_rgba) = bg_opt {
                        el = el.bg(bg_rgba);
                    }
                    if run.bold {
                        el = el.font_weight(FontWeight::BOLD);
                    }
                    if run.underline {
                        el = el.underline().text_decoration_1();
                    }
                    el
                }))
        }))
}

/// Merge adjacent cells with identical styling into [`Run`]s.
///
/// Runs are split on any difference in fg, bg, bold, or underline.
/// Wide-char spacer cells (`display_width == 0`) are silently skipped.
/// The cell at `cursor_col` (when `Some`) is always isolated into its own run
/// with `is_cursor = true`, regardless of surrounding styles.
pub(crate) fn coalesce_runs(
    cells: &[SnapshotCell],
    cursor_col: Option<usize>,
) -> impl Iterator<Item = Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, cell) in cells.iter().enumerate() {
        let w = usize::from(cell.display_width);
        if w == 0 {
            continue; // wide-char spacer — skip without emitting a run
        }
        let is_cursor = cursor_col == Some(col);
        if let Some(last) = runs.last_mut() {
            if !is_cursor
                && !last.is_cursor
                && last.fg == cell.fg
                && last.bg == cell.bg
                && last.bold == cell.bold
                && last.underline == cell.underline
            {
                last.text.push(cell.c);
                last.width += w;
                continue;
            }
        }
        runs.push(Run {
            text: cell.c.to_string(),
            fg: cell.fg,
            bg: cell.bg,
            width: w,
            bold: cell.bold,
            underline: cell.underline,
            is_cursor,
        });
    }
    runs.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{ResolvedColor, SnapshotCell};

    fn cell(c: char) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 1,
            bold: false,
            underline: false,
        }
    }

    fn cell_wide(c: char) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 2,
            bold: false,
            underline: false,
        }
    }

    fn cell_spacer() -> SnapshotCell {
        SnapshotCell {
            c: ' ',
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 0,
            bold: false,
            underline: false,
        }
    }

    fn cell_rgb(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Rgb(r, g, b),
            bg: ResolvedColor::Default,
            display_width: 1,
            bold: false,
            underline: false,
        }
    }

    fn cell_bg(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Rgb(r, g, b),
            display_width: 1,
            bold: false,
            underline: false,
        }
    }

    fn cell_bold(c: char) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 1,
            bold: true,
            underline: false,
        }
    }

    // ── existing tests (ported to struct-field access) ────────────────────────

    #[test]
    fn wide_char_spacer_cells_are_skipped() {
        let cells = [cell_wide('あ'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "あ");
        assert_eq!(runs[0].width, 2);
    }

    #[test]
    fn empty_slice_yields_no_runs() {
        let runs: Vec<_> = coalesce_runs(&[], None).collect();
        assert!(runs.is_empty());
    }

    #[test]
    fn same_color_cells_coalesce_into_one_run() {
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "abc");
        assert_eq!(runs[0].width, 3);
        assert_eq!(runs[0].fg, ResolvedColor::Default);
        assert_eq!(runs[0].bg, ResolvedColor::Default);
    }

    #[test]
    fn different_color_cells_split_into_runs() {
        let cells = [cell('a'), cell_rgb('b', 255, 0, 0)];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "a");
        assert_eq!(runs[0].width, 1);
        assert_eq!(runs[1].text, "b");
        assert_eq!(runs[1].fg, ResolvedColor::Rgb(255, 0, 0));
    }

    #[test]
    fn adjacent_same_color_runs_merge_before_color_change() {
        let cells = [cell('a'), cell('b'), cell_rgb('c', 0, 255, 0)];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[0].width, 2);
        assert_eq!(runs[1].text, "c");
        assert_eq!(runs[1].fg, ResolvedColor::Rgb(0, 255, 0));
    }

    #[test]
    fn mixed_width_cells_sum_display_width() {
        let cells = [cell('a'), cell_wide('あ')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "aあ");
        assert_eq!(runs[0].width, 3);
    }

    #[test]
    fn color_to_rgba_default_matches_zouge() {
        assert_eq!(color_to_rgba(ResolvedColor::Default), Colors::zouge());
    }

    #[test]
    fn color_to_rgba_rgb_packs_bytes() {
        let c = color_to_rgba(ResolvedColor::Rgb(0x12, 0x34, 0x56));
        assert!((c.r - 18.0 / 255.0).abs() < 0.01);
        assert!((c.g - 52.0 / 255.0).abs() < 0.01);
        assert!((c.b - 86.0 / 255.0).abs() < 0.01);
    }

    #[test]
    fn different_background_splits_runs() {
        let cells = [cell('a'), cell_bg('b', 0, 0, 255)];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1].bg, ResolvedColor::Rgb(0, 0, 255));
    }

    // ── new tests: bold ───────────────────────────────────────────────────────

    #[test]
    fn bold_cells_split_from_normal_cells() {
        let cells = [cell('a'), cell_bold('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].bold);
        assert!(runs[1].bold);
        assert_eq!(runs[1].text, "b");
        assert!(!runs[2].bold);
    }

    #[test]
    fn adjacent_bold_cells_coalesce() {
        let cells = [cell_bold('x'), cell_bold('y')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "xy");
        assert!(runs[0].bold);
    }

    // ── new tests: cursor isolation ───────────────────────────────────────────

    #[test]
    fn cursor_col_isolates_cursor_cell() {
        // 'a' | cursor:'b' | 'c'  →  three runs
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(1)).collect();
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].is_cursor);
        assert!(runs[1].is_cursor);
        assert_eq!(runs[1].text, "b");
        assert_eq!(runs[1].width, 1);
        assert!(!runs[2].is_cursor);
    }

    #[test]
    fn cursor_at_col_zero() {
        let cells = [cell('a'), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(0)).collect();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].is_cursor);
        assert_eq!(runs[0].text, "a");
        assert!(!runs[1].is_cursor);
    }

    #[test]
    fn no_cursor_col_leaves_all_is_cursor_false() {
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert!(runs.iter().all(|r| !r.is_cursor));
    }

    #[test]
    fn cursor_past_end_produces_no_cursor_run() {
        let cells = [cell('a'), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(99)).collect();
        assert!(runs.iter().all(|r| !r.is_cursor));
    }
}
