use gpui::{div, px, rgba, IntoElement, ParentElement, Rgba, Styled};
use gpui_component::v_flex;
use unicode_width::UnicodeWidthChar;

use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::{GridSnapshot, ResolvedColor};
use crate::theme::Colors;

/// Fixed cell width in pixels (tune against tmux box-drawing alignment).
pub const CELL_W: f32 = 7.8;
/// Fixed cell height in pixels.
pub const CELL_H: f32 = 20.0;

/// Terminal column width for a single character (1 = half-width, 2 = full-width).
pub fn cell_width(c: char) -> usize {
    match c.width() {
        Some(0) | None => 0,
        Some(w) => w,
    }
}

/// Map a resolved terminal color to a GPUI `Rgba` for text/background styling.
pub fn color_to_rgba(color: ResolvedColor) -> Rgba {
    match color {
        ResolvedColor::Rgb(r, g, b) => rgba(u32::from_be_bytes([r, g, b, 0xff])),
        ResolvedColor::Default => Colors::zouge(),
    }
}

pub fn render_grid(snap: &GridSnapshot) -> impl IntoElement {
    v_flex()
        .font_family(MONO_FONT)
        .text_size(px(13.))
        .children(snap.cells.iter().map(|row| {
            div()
                .flex()
                .flex_row()
                .h(px(CELL_H))
                .children(coalesce_runs(row).map(|(text, fg, bg, cells)| {
                    let mut el = div()
                        .child(text)
                        .w(px(CELL_W * cells as f32))
                        .overflow_hidden()
                        .text_color(color_to_rgba(fg));
                    if let ResolvedColor::Rgb(_, _, _) = bg {
                        el = el.bg(color_to_rgba(bg));
                    }
                    el
                }))
        }))
}

pub(crate) fn coalesce_runs(
    cells: &[crate::terminal::SnapshotCell],
) -> impl Iterator<Item = (String, ResolvedColor, ResolvedColor, usize)> {
    let mut runs: Vec<(String, ResolvedColor, ResolvedColor, usize)> = Vec::new();
    for cell in cells {
        let w = cell_width(cell.c);
        if let Some(last) = runs.last_mut() {
            if last.1 == cell.fg && last.2 == cell.bg {
                last.0.push(cell.c);
                last.3 += w;
                continue;
            }
        }
        runs.push((cell.c.to_string(), cell.fg, cell.bg, w));
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
            bold: false,
            underline: false,
        }
    }

    fn cell_rgb(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Rgb(r, g, b),
            bg: ResolvedColor::Default,
            bold: false,
            underline: false,
        }
    }

    #[test]
    fn cell_width_ascii_is_one() {
        assert_eq!(cell_width('a'), 1);
        assert_eq!(cell_width(' '), 1);
    }

    #[test]
    fn cell_width_cjk_is_two() {
        assert_eq!(cell_width('あ'), 2);
        assert_eq!(cell_width('漢'), 2);
    }

    #[test]
    fn empty_slice_yields_no_runs() {
        let runs: Vec<_> = coalesce_runs(&[]).collect();
        assert!(runs.is_empty());
    }

    #[test]
    fn same_color_cells_coalesce_into_one_run() {
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "abc");
        assert_eq!(runs[0].3, 3);
        assert_eq!(runs[0].1, ResolvedColor::Default);
        assert_eq!(runs[0].2, ResolvedColor::Default);
    }

    #[test]
    fn different_color_cells_split_into_runs() {
        let cells = [cell('a'), cell_rgb('b', 255, 0, 0)];
        let runs: Vec<_> = coalesce_runs(&cells).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, "a");
        assert_eq!(runs[0].3, 1);
        assert_eq!(runs[1].0, "b");
        assert_eq!(runs[1].1, ResolvedColor::Rgb(255, 0, 0));
    }

    #[test]
    fn adjacent_same_color_runs_merge_before_color_change() {
        let cells = [cell('a'), cell('b'), cell_rgb('c', 0, 255, 0)];
        let runs: Vec<_> = coalesce_runs(&cells).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, "ab");
        assert_eq!(runs[0].3, 2);
        assert_eq!(runs[1].0, "c");
        assert_eq!(runs[1].1, ResolvedColor::Rgb(0, 255, 0));
    }

    #[test]
    fn mixed_width_cells_sum_cell_count() {
        let cells = [cell('a'), cell('あ')];
        let runs: Vec<_> = coalesce_runs(&cells).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "aあ");
        assert_eq!(runs[0].3, 3);
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

    fn cell_bg(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Rgb(r, g, b),
            bold: false,
            underline: false,
        }
    }

    #[test]
    fn different_background_splits_runs() {
        let cells = [cell('a'), cell_bg('b', 0, 0, 255)];
        let runs: Vec<_> = coalesce_runs(&cells).collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1].2, ResolvedColor::Rgb(0, 0, 255));
    }
}
