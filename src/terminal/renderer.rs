use gpui::{div, px, rgba, FontWeight, IntoElement, ParentElement, Rgba, Styled};
use gpui_component::v_flex;

use crate::terminal::{GridSnapshot, ResolvedColor, SnapshotCell};
use crate::theme::Colors;

/// Fixed cell width in pixels for the default font (Moralerspace Neon HW @ 13pt).
/// Moralerspace HW ASCII advance = 525/1000 × 13 = 6.825px; we use 7.8 for
/// comfortable inter-char spacing that was empirically validated in cmd_185.
pub const CELL_W: f32 = 7.8;
/// Fixed cell height in pixels.
pub const CELL_H: f32 = 20.0;

/// Return the cell width (in logical pixels) appropriate for the selected font at
/// `text_size = 13pt`.
///
/// Measured advances (HAdvanceWidth / UPM × 13):
///   Moralerspace Neon HW : ASCII 6.825 px  → use 7.8 (empirical, adds breathing room)
///   Cica                  : ASCII 6.500 px  → use 6.5 (exact fit)
///   MS Gothic             : ASCII 6.500 px  → use 6.5 (exact fit; all EAW=A are full-width)
pub fn cell_width_for_font(font: &str) -> f32 {
    match font {
        "Cica" | "MS Gothic" => 6.5,
        _ => CELL_W,
    }
}

/// Returns `true` for Unicode code-points that are EAW=Ambiguous and rendered as
/// **full-width** by `MS Gothic` (advance = UPM = 13 px @ 13 pt) but as
/// **half-width** by Cica / Moralerspace HW (advance = UPM/2 = 6.5 px @ 13 pt).
///
/// When a non-MS-Gothic font is selected, the renderer overrides the font for
/// these runs with `MS Gothic` so that:
///   • tmux box borders render as solid continuous lines
///   • `→` / `←` arrows and `◆` diamonds appear full-width
///
/// Measured advances in MS Gothic @ 13 pt (UPM = 256):
///   U+2192 → (RIGHTWARDS ARROW)         : 256 units = 13 px  [FULL]
///   U+25C6 ◆ (BLACK DIAMOND)             : 256 units = 13 px  [FULL]
///   U+2500 ─ (BOX DRAWINGS LIGHT HORIZ)  : 256 units = 13 px  [FULL]
///   U+2502 │ (BOX DRAWINGS LIGHT VERT)   : 256 units = 13 px  [FULL]
///
/// Exception: U+25B6 ▶ is HALF-WIDTH (128 units) in MS Gothic too — no help there.
pub(crate) fn is_box_drawing(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x2190..=0x21FF  // Arrows: →, ←, ↑, ↓, ↔, ⇒, etc.
        | 0x2500..=0x25FF  // Box Drawing + Block Elements + Geometric Shapes (◆, ■, etc.)
    )
}

/// A styled run of consecutive terminal cells with identical visual properties.
///
/// Adjacent cells that share the same fg, bg, bold, and underline flags are merged
/// into a single `Run` for efficient GPUI rendering. The cursor cell is always its
/// own run (never merged with neighbours) so colours can be inverted independently.
///
/// Runs are also split at box-drawing / non-box-drawing boundaries so that the
/// renderer can apply a per-run font override for box chars (see `is_box_drawing`).
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
    /// True when every character in this run is a box-drawing / block-element char.
    /// The renderer will substitute `Moralerspace Neon HW` for these runs when the
    /// user has selected a different (non-HW) font.
    pub use_box_font: bool,
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

/// Font used for EAW=Ambiguous override (arrows, box drawing, geometric shapes).
/// MS Gothic has full-width glyphs for → ◆ ─ │ etc. in EAW=A=2 mode.
/// Falls back to "Moralerspace Neon HW" (embedded) if MS Gothic is not loaded.
const BOX_FONT: &str = "MS Gothic";

pub fn render_grid(snap: &GridSnapshot, font: &str) -> impl IntoElement {
    let cw = cell_width_for_font(font);
    let (cursor_row, cursor_col) = snap.cursor;
    v_flex()
        .font_family(font.to_string())
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
                    // For box-drawing runs, override with the embedded HW font so that
                    // tmux borders are full-width regardless of the user's font choice.
                    let effective_font = if run.use_box_font && font != BOX_FONT {
                        BOX_FONT.to_string()
                    } else {
                        font.to_string()
                    };
                    let mut el = div()
                        .child(run.text)
                        .w(px(cw * run.width as f32))
                        .overflow_hidden()
                        .font_family(effective_font)
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
/// Runs are split on any difference in fg, bg, bold, underline, or box-drawing
/// category (see [`is_box_drawing`]).  Wide-char spacer cells
/// (`display_width == 0`) are silently skipped.
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
        let use_box_font = is_box_drawing(cell.c);
        if let Some(last) = runs.last_mut() {
            if !is_cursor
                && !last.is_cursor
                && last.fg == cell.fg
                && last.bg == cell.bg
                && last.bold == cell.bold
                && last.underline == cell.underline
                && last.use_box_font == use_box_font
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
            use_box_font,
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

    // ── new tests: box-drawing font split ─────────────────────────────────────

    fn cell_box(c: char) -> SnapshotCell {
        // Box drawing chars must be in the U+2500-U+259F range.
        SnapshotCell {
            c,
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 2, // EAW=A → 2-wide in CJK mode
            bold: false,
            underline: false,
        }
    }

    #[test]
    fn is_box_drawing_detects_ranges() {
        // Arrows (U+2190-U+21FF)
        assert!(is_box_drawing('\u{2192}')); // →
        assert!(is_box_drawing('\u{2190}')); // ←
        assert!(is_box_drawing('\u{21FF}')); // end of arrows
        // Box Drawing + Block (U+2500-U+259F)
        assert!(is_box_drawing('\u{2500}')); // ─
        assert!(is_box_drawing('\u{2502}')); // │
        assert!(is_box_drawing('\u{2580}')); // upper-half block
        assert!(is_box_drawing('\u{259F}')); // lower-right quadrant
        // Geometric Shapes (U+25A0-U+25FF)
        assert!(is_box_drawing('\u{25C6}')); // ◆
        assert!(is_box_drawing('\u{25B6}')); // ▶
        assert!(is_box_drawing('\u{25FF}')); // end of geometric shapes
        // Outside ranges
        assert!(!is_box_drawing('a'));
        assert!(!is_box_drawing('\u{218F}')); // just below arrows
        assert!(!is_box_drawing('\u{2200}')); // just above arrows, below box
        assert!(!is_box_drawing('\u{24FF}')); // Enclosed Alphanumerics (not in range)
    }

    #[test]
    fn box_drawing_split_from_normal_chars() {
        // ASCII 'a', then U+2500 '─', then ASCII 'b'  →  3 runs
        let cells = [cell('a'), cell_box('\u{2500}'), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].use_box_font);
        assert!(runs[1].use_box_font);
        assert_eq!(runs[1].text, "\u{2500}");
        assert!(!runs[2].use_box_font);
    }

    #[test]
    fn arrow_chars_are_override_font() {
        // → (U+2192) and ◆ (U+25C6) trigger font override
        let arrow_cell = SnapshotCell {
            c: '\u{2192}', // →
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 2, // EAW=A in CJK mode
            bold: false,
            underline: false,
        };
        let diamond_cell = SnapshotCell {
            c: '\u{25C6}', // ◆
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 2,
            bold: false,
            underline: false,
        };
        let cells = [arrow_cell, diamond_cell];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        // Both are in override range → should coalesce into 1 run
        assert_eq!(runs.len(), 1);
        assert!(runs[0].use_box_font);
        assert_eq!(runs[0].text, "\u{2192}\u{25C6}");
        assert_eq!(runs[0].width, 4); // 2+2
    }

    #[test]
    fn adjacent_box_drawing_chars_coalesce() {
        let cells = [cell_box('\u{2500}'), cell_box('\u{2500}')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].use_box_font);
        assert_eq!(runs[0].text, "\u{2500}\u{2500}");
        assert_eq!(runs[0].width, 4); // 2+2 display-width
    }

    #[test]
    fn cell_width_for_cica_is_6_5() {
        assert!((cell_width_for_font("Cica") - 6.5).abs() < 0.01);
    }

    #[test]
    fn cell_width_for_moralerspace_is_default() {
        assert!((cell_width_for_font("Moralerspace Neon HW") - CELL_W).abs() < 0.01);
        assert!((cell_width_for_font("unknown font") - CELL_W).abs() < 0.01);
    }
}
