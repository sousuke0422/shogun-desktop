use gpui::{
    canvas, div, fill, point, px, rgba, size, AnyElement, App, Bounds, FontWeight, IntoElement,
    ParentElement, Rgba, Styled, Window,
};
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
/// Used as a **static fallback** when `TextSystem` is not available (tests, bench).
/// At runtime, [`measure_cell_metrics`] supersedes this via `TextSystem::ch_advance`.
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

/// Returns `true` for Unicode code-points rendered as geometry (box drawing + block
/// elements).  Characters in this range are drawn as filled quads by
/// [`paint_box_char`], eliminating any font-metric dependency.
///
/// Covered:
///   U+2500-U+257F  Box Drawing (─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼ ═ ║ ╔ ╗ ╚ ╝ …)
///   U+2580-U+259F  Block Elements (▀ ▄ █ ▌ ▐ ░ ▒ ▓ …)
pub(crate) fn is_geom_box_char(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 0x2500..=0x259F)
}

/// Returns `true` for EAW=Ambiguous code-points that need the MS Gothic font
/// override (full-width glyph) but are **not** drawn as geometry.
///
/// Covered:
///   U+2190-U+21FF  Arrows (→ ← ↑ ↓ ⇒ …)
///   U+25A0-U+25FF  Geometric Shapes (◆ ■ ▶ …)
pub(crate) fn is_box_drawing(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x2190..=0x21FF   // Arrows
        | 0x25A0..=0x25FF // Geometric Shapes (non-block)
    )
}

/// A styled run of consecutive terminal cells with identical visual properties.
///
/// Adjacent cells that share the same fg, bg, bold, and underline flags are merged
/// into a single `Run` for efficient GPUI rendering.  The cursor cell is always
/// its own run so colours can be inverted independently.
///
/// Runs are also split at geometry-box / font-box / plain-text boundaries.
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
    /// True when every char in this run is a geometry-rendered box char (U+2500-U+259F).
    /// The renderer uses `canvas` + `paint_quad` for these.
    pub use_geom: bool,
    /// True when every char needs the MS Gothic font override (arrows, geometric shapes).
    pub use_box_font: bool,
    /// Per-char display widths for geometry runs (parallel to `text.chars()`).
    /// Empty for non-geom runs.
    pub geom_char_widths: Vec<u8>,
}

/// Map a resolved terminal color to a GPUI `Rgba`.
pub fn color_to_rgba(color: ResolvedColor) -> Rgba {
    match color {
        ResolvedColor::Rgb(r, g, b) => rgba(u32::from_be_bytes([r, g, b, 0xff])),
        ResolvedColor::Default => Colors::zouge(),
    }
}

/// Resolve the display fg/bg for a [`Run`], applying block-cursor inversion.
///
/// Returns `(fg, bg_opt)`.  `None` bg means transparent.
fn resolve_run_colors(run: &Run) -> (Rgba, Option<Rgba>) {
    let resolved_fg = color_to_rgba(run.fg);
    let resolved_bg = match run.bg {
        ResolvedColor::Rgb(r, g, b) => Some(rgba(u32::from_be_bytes([r, g, b, 0xff]))),
        ResolvedColor::Default => None,
    };
    if run.is_cursor {
        let cursor_bg = resolved_fg;
        let cursor_fg = resolved_bg.unwrap_or_else(|| rgba(0x1e1e1eff));
        (cursor_fg, Some(cursor_bg))
    } else {
        (resolved_fg, resolved_bg)
    }
}

/// Font used for EAW=Ambiguous override (arrows U+2190-U+21FF, geometric shapes
/// U+25A0-U+25FF).  MS Gothic has full-width glyphs for → ◆ ▶ etc. in EAW=A=2.
const BOX_FONT: &str = "MS Gothic";

// ─────────────────────────────────────────────────────────────────────────────
// Geometry renderer for U+2500-U+259F
// ─────────────────────────────────────────────────────────────────────────────

/// Draw one box-drawing / block-element character as filled quads.
///
/// `ox`, `oy`  — top-left origin of this character's cell (raw f32 pixels).
/// `cw`        — width of this character's display cell (1-cell-px × display_width).
/// `ch`        — cell height (CELL_H).
///
/// Returns `false` for unhandled characters (caller may fall back to font).
#[allow(clippy::too_many_arguments)]
fn paint_box_char(
    c: char,
    ox: f32,
    oy: f32,
    cw: f32,
    ch: f32,
    fg: Rgba,
    window: &mut Window,
) -> bool {
    // Cell boundary pixels
    let x1 = ox + cw;
    let y1 = oy + ch;
    let xm = ox + cw * 0.5;
    let ym = oy + ch * 0.5;

    // Light line weight: 1/8 cell height, min 1 px
    let lw = (ch / 8.0).max(1.0);
    // Heavy line weight: 1/4 cell height, min 2 px
    let hw = (ch / 4.0).max(2.0);

    // Helper: build a Bounds<Pixels> from raw f32 corners
    macro_rules! rect {
        ($ax:expr, $ay:expr, $bx:expr, $by:expr) => {
            Bounds {
                origin: point(px($ax), px($ay)),
                size: size(px(($bx - $ax).max(0.0)), px(($by - $ay).max(0.0))),
            }
        };
    }

    // Sub-pixel tolerance: extend lines at open ends to prevent 1-device-pixel
    // gaps caused by GPUI rounding logical→physical coords per-quad.
    // At 1.25× DPI: 0.5 logical px = 0.625 device px → rounds to 1 full overlap pixel.
    const TOL: f32 = 0.5;

    // Segment helpers (returns Bounds<Pixels>)
    // Open ends are extended by TOL so adjacent segments overlap rather than gap.
    // Interior endpoints (at xm/ym) are NOT extended — keeps corner geometry clean.
    macro_rules! h_full { ($yc:expr, $lw:expr) => { rect!(ox-TOL,  $yc-$lw/2.0, x1+TOL, $yc+$lw/2.0) }; }
    macro_rules! h_left { ($yc:expr, $lw:expr) => { rect!(ox-TOL,  $yc-$lw/2.0, xm,     $yc+$lw/2.0) }; }
    macro_rules! h_right{ ($yc:expr, $lw:expr) => { rect!(xm,      $yc-$lw/2.0, x1+TOL, $yc+$lw/2.0) }; }
    // Vertical full / top-half / bottom-half at given x-center
    macro_rules! v_full { ($xc:expr, $lw:expr) => { rect!($xc-$lw/2.0, oy-TOL, $xc+$lw/2.0, y1+TOL) }; }
    macro_rules! v_top  { ($xc:expr, $lw:expr) => { rect!($xc-$lw/2.0, oy-TOL, $xc+$lw/2.0, ym)     }; }
    macro_rules! v_bot  { ($xc:expr, $lw:expr) => { rect!($xc-$lw/2.0, ym,     $xc+$lw/2.0, y1+TOL) }; }

    // Paint a filled quad
    macro_rules! q { ($b:expr) => { window.paint_quad(fill($b, fg)) }; }

    // Double-line offsets (40% and 60% of cell)
    let d1x = ox + cw * 0.4;
    let d2x = ox + cw * 0.6;
    let d1y = oy + ch * 0.4;
    let d2y = oy + ch * 0.6;

    match c {
        // ── Light & heavy horizontal / vertical ──────────────────────────────
        '─' => q!(h_full!(ym, lw)),
        '━' => q!(h_full!(ym, hw)),
        '│' => q!(v_full!(xm, lw)),
        '┃' => q!(v_full!(xm, hw)),
        // dashed lines → solid approximation
        '┄' | '╌' | '┈' => q!(h_full!(ym, lw)),
        '┅' | '╍' | '┉' => q!(h_full!(ym, hw)),
        '┆' | '╎' | '┊' => q!(v_full!(xm, lw)),
        '┇' | '╏' | '┋' => q!(v_full!(xm, hw)),

        // ── Light corners ─────────────────────────────────────────────────────
        '┌' => { q!(h_right!(ym, lw)); q!(v_bot!(xm, lw)); }
        '┐' => { q!(h_left!(ym, lw));  q!(v_bot!(xm, lw)); }
        '└' => { q!(h_right!(ym, lw)); q!(v_top!(xm, lw)); }
        '┘' => { q!(h_left!(ym, lw));  q!(v_top!(xm, lw)); }

        // light + heavy corner variants
        '┍' => { q!(h_right!(ym, hw)); q!(v_bot!(xm, lw)); }
        '┎' => { q!(h_right!(ym, lw)); q!(v_bot!(xm, hw)); }
        '┏' => { q!(h_right!(ym, hw)); q!(v_bot!(xm, hw)); }
        '┑' => { q!(h_left!(ym, hw));  q!(v_bot!(xm, lw)); }
        '┒' => { q!(h_left!(ym, lw));  q!(v_bot!(xm, hw)); }
        '┓' => { q!(h_left!(ym, hw));  q!(v_bot!(xm, hw)); }
        '┕' => { q!(h_right!(ym, hw)); q!(v_top!(xm, lw)); }
        '┖' => { q!(h_right!(ym, lw)); q!(v_top!(xm, hw)); }
        '┗' => { q!(h_right!(ym, hw)); q!(v_top!(xm, hw)); }
        '┙' => { q!(h_left!(ym, hw));  q!(v_top!(xm, lw)); }
        '┚' => { q!(h_left!(ym, lw));  q!(v_top!(xm, hw)); }
        '┛' => { q!(h_left!(ym, hw));  q!(v_top!(xm, hw)); }

        // ── T-junctions ───────────────────────────────────────────────────────
        '├' => { q!(h_right!(ym, lw)); q!(v_full!(xm, lw)); }
        '┤' => { q!(h_left!(ym, lw));  q!(v_full!(xm, lw)); }
        '┬' => { q!(h_full!(ym, lw));  q!(v_bot!(xm, lw));  }
        '┴' => { q!(h_full!(ym, lw));  q!(v_top!(xm, lw));  }
        '┼' => { q!(h_full!(ym, lw));  q!(v_full!(xm, lw)); }

        // heavy T-junctions
        '┣' => { q!(h_right!(ym, hw)); q!(v_full!(xm, hw)); }
        '┫' => { q!(h_left!(ym, hw));  q!(v_full!(xm, hw)); }
        '┳' => { q!(h_full!(ym, hw));  q!(v_bot!(xm, hw));  }
        '┻' => { q!(h_full!(ym, hw));  q!(v_top!(xm, hw));  }
        '╋' => { q!(h_full!(ym, hw));  q!(v_full!(xm, hw)); }

        // mixed T-junctions → approximate with light
        '┝'..='┞' | '┟'..='┠' | '┡'..='┢' | '┦'..='┧'
        | '┨'..='┩' | '┪' | '┭'..='┯'
        | '┰'..='┲' | '┵'..='┷' | '┸'..='┺' | '┽'..='┿'
        | '╀'..='╉' | '╊' => {
            q!(h_full!(ym, lw)); q!(v_full!(xm, lw));
        }

        // ── Half-lines ────────────────────────────────────────────────────────
        '╴' => q!(h_left!(ym, lw)),
        '╵' => q!(v_top!(xm, lw)),
        '╶' => q!(h_right!(ym, lw)),
        '╷' => q!(v_bot!(xm, lw)),
        '╸' => q!(h_left!(ym, hw)),
        '╹' => q!(v_top!(xm, hw)),
        '╺' => q!(h_right!(ym, hw)),
        '╻' => q!(v_bot!(xm, hw)),
        '╼' => { q!(h_left!(ym, lw)); q!(h_right!(ym, hw)); }
        '╽' => { q!(v_top!(xm, lw)); q!(v_bot!(xm, hw)); }
        '╾' => { q!(h_left!(ym, hw)); q!(h_right!(ym, lw)); }
        '╿' => { q!(v_top!(xm, hw)); q!(v_bot!(xm, lw)); }

        // ── Double lines ──────────────────────────────────────────────────────
        '═' => { q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw)); }
        '║' => { q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw)); }

        // Double corners (top-left)
        '╔' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_bot!(d1x, lw));   q!(v_bot!(d2x, lw));
        }
        '╓' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╒' => {
            q!(h_right!(ym, lw));
            q!(v_bot!(d1x, lw)); q!(v_bot!(d2x, lw));
        }

        // top-right
        '╗' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_bot!(d1x, lw));  q!(v_bot!(d2x, lw));
        }
        '╖' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╕' => {
            q!(h_left!(ym, lw));
            q!(v_bot!(d1x, lw)); q!(v_bot!(d2x, lw));
        }

        // bottom-left
        '╚' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_top!(d1x, lw));   q!(v_top!(d2x, lw));
        }
        '╙' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╘' => {
            q!(h_right!(ym, lw));
            q!(v_top!(d1x, lw)); q!(v_top!(d2x, lw));
        }

        // bottom-right
        '╝' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_top!(d1x, lw));  q!(v_top!(d2x, lw));
        }
        '╜' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╛' => {
            q!(h_left!(ym, lw));
            q!(v_top!(d1x, lw)); q!(v_top!(d2x, lw));
        }

        // Double T-junctions
        '╠' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_full!(d1x, lw));  q!(v_full!(d2x, lw));
        }
        '╟' => {
            q!(h_right!(d1y, lw)); q!(h_right!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╞' => {
            q!(h_right!(ym, lw));
            q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw));
        }
        '╣' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw));
        }
        '╢' => {
            q!(h_left!(d1y, lw)); q!(h_left!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╡' => {
            q!(h_left!(ym, lw));
            q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw));
        }
        '╦' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_bot!(d1x, lw));  q!(v_bot!(d2x, lw));
        }
        '╥' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╤' => {
            q!(h_full!(ym, lw));
            q!(v_bot!(d1x, lw)); q!(v_bot!(d2x, lw));
        }
        '╩' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_top!(d1x, lw));  q!(v_top!(d2x, lw));
        }
        '╨' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╧' => {
            q!(h_full!(ym, lw));
            q!(v_top!(d1x, lw)); q!(v_top!(d2x, lw));
        }
        '╬' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw));
        }
        '╫' => {
            q!(h_full!(d1y, lw)); q!(h_full!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╪' => {
            q!(h_full!(ym, lw));
            q!(v_full!(d1x, lw)); q!(v_full!(d2x, lw));
        }

        // ── Block elements U+2580-U+259F ──────────────────────────────────────
        '▀' => q!(rect!(ox, oy,             x1, ym)),           // upper half
        '▁' => q!(rect!(ox, oy+ch*7.0/8.0, x1, y1)),           // lower 1/8
        '▂' => q!(rect!(ox, oy+ch*6.0/8.0, x1, y1)),
        '▃' => q!(rect!(ox, oy+ch*5.0/8.0, x1, y1)),
        '▄' => q!(rect!(ox, ym,             x1, y1)),           // lower half
        '▅' => q!(rect!(ox, oy+ch*3.0/8.0, x1, y1)),
        '▆' => q!(rect!(ox, oy+ch*2.0/8.0, x1, y1)),
        '▇' => q!(rect!(ox, oy+ch*1.0/8.0, x1, y1)),
        '█' => q!(rect!(ox, oy,             x1, y1)),           // full block
        '▉' => q!(rect!(ox, oy, ox+cw*7.0/8.0, y1)),
        '▊' => q!(rect!(ox, oy, ox+cw*6.0/8.0, y1)),
        '▋' => q!(rect!(ox, oy, ox+cw*5.0/8.0, y1)),
        '▌' => q!(rect!(ox, oy, xm,             y1)),           // left half
        '▍' => q!(rect!(ox, oy, ox+cw*3.0/8.0, y1)),
        '▎' => q!(rect!(ox, oy, ox+cw*2.0/8.0, y1)),
        '▏' => q!(rect!(ox, oy, ox+cw*1.0/8.0, y1)),
        '▐' => q!(rect!(xm, oy, x1,             y1)),           // right half
        // Shades: approximate with dot patterns
        '░' => {
            let dw = (cw * 0.15).max(1.0);
            let dh = (ch * 0.15).max(1.0);
            for row in 0..4_i32 {
                for col in 0..4_i32 {
                    if (row + col) % 4 == 0 {
                        let qx = ox + cw * (col as f32 / 4.0 + 0.05);
                        let qy = oy + ch * (row as f32 / 4.0 + 0.05);
                        q!(Bounds {
                            origin: point(px(qx), px(qy)),
                            size: size(px(dw), px(dh)),
                        });
                    }
                }
            }
        }
        '▒' => {
            let dw = (cw / 4.0).max(1.0);
            let dh = (ch / 4.0).max(1.0);
            for row in 0..4_i32 {
                for col in 0..4_i32 {
                    if (row + col) % 2 == 0 {
                        let qx = ox + cw * col as f32 / 4.0;
                        let qy = oy + ch * row as f32 / 4.0;
                        q!(Bounds {
                            origin: point(px(qx), px(qy)),
                            size: size(px(dw), px(dh)),
                        });
                    }
                }
            }
        }
        '▓' => {
            // 75% — draw the majority cells (3 out of 4)
            let dw = (cw / 4.0).max(1.0);
            let dh = (ch / 4.0).max(1.0);
            for row in 0..4_i32 {
                for col in 0..4_i32 {
                    if (row + col) % 2 == 0 || (row * 4 + col) % 4 != 3 {
                        let qx = ox + cw * col as f32 / 4.0;
                        let qy = oy + ch * row as f32 / 4.0;
                        q!(Bounds {
                            origin: point(px(qx), px(qy)),
                            size: size(px(dw), px(dh)),
                        });
                    }
                }
            }
        }
        '▔' => q!(rect!(ox, oy, x1, oy+ch/8.0)),               // upper 1/8
        '▕' => q!(rect!(ox+cw*7.0/8.0, oy, x1, y1)),           // right 1/8
        '▖' => q!(rect!(ox, ym, xm, y1)),                       // lower-left quad
        '▗' => q!(rect!(xm, ym, x1, y1)),                       // lower-right quad
        '▘' => q!(rect!(ox, oy, xm, ym)),                       // upper-left quad
        '▙' => { q!(rect!(ox, oy, xm, y1));  q!(rect!(xm, ym, x1, y1)); }
        '▚' => { q!(rect!(ox, oy, xm, ym));  q!(rect!(xm, ym, x1, y1)); }
        '▛' => { q!(rect!(ox, oy, x1, ym));  q!(rect!(ox, ym, xm, y1)); }
        '▜' => { q!(rect!(ox, oy, x1, ym));  q!(rect!(xm, ym, x1, y1)); }
        '▝' => q!(rect!(xm, oy, x1, ym)),                       // upper-right quad
        '▞' => { q!(rect!(xm, oy, x1, ym));  q!(rect!(ox, ym, xm, y1)); }
        '▟' => { q!(rect!(xm, oy, x1, ym));  q!(rect!(ox, ym, x1, y1)); }

        _ => return false,
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────────

/// Render the terminal grid.
///
/// `cw` and `ch` are the logical-pixel cell dimensions measured at runtime from
/// the active font via [`crate::window::measure_cell_metrics`] (wt-style
/// `ch_advance` + `ascent + descent`).  Fall back to [`CELL_W`]/[`CELL_H`] when
/// `TextSystem` is unavailable (e.g. unit tests).
pub fn render_grid(snap: &GridSnapshot, font: &str, cw: f32, ch: f32) -> impl IntoElement {
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
                .h(px(ch))
                .children(coalesce_runs(row, cur_col).map(move |run| -> AnyElement {
                    let (fg_rgba, bg_opt) = resolve_run_colors(&run);
                    let total_w = px(cw * run.width as f32);

                    if run.use_geom {
                        // ── Geometry canvas for box drawing / block elements ───
                        let chars: Vec<(char, u8)> = run
                            .text
                            .chars()
                            .zip(run.geom_char_widths.iter().copied())
                            .map(|(c, w)| (c, w))
                            .collect();
                        let fg_cap = fg_rgba;
                        let bg_cap = bg_opt;
                        let cw_cap = cw;

                        return canvas(
                            |_bounds, _window, _cx| (),
                            move |bounds, (), window, _cx: &mut App| {
                                if let Some(bg) = bg_cap {
                                    window.paint_quad(fill(bounds, bg));
                                }
                                let mut x_off = f32::from(bounds.origin.x);
                                let y_off = f32::from(bounds.origin.y);
                                // Use actual painted cell height from canvas bounds
                                // so geometry always fills the row regardless of font size.
                                let cell_h = f32::from(bounds.size.height);
                                for &(c, dw) in &chars {
                                    let char_cw = cw_cap * dw as f32;
                                    paint_box_char(c, x_off, y_off, char_cw, cell_h, fg_cap, window);
                                    x_off += char_cw;
                                }
                            },
                        )
                        .w(total_w)
                        .h(px(ch))
                        .into_any_element();
                    }

                    // ── Font-rendered text ─────────────────────────────────────
                    let effective_font = if run.use_box_font && font != BOX_FONT {
                        BOX_FONT.to_string()
                    } else {
                        font.to_string()
                    };
                    let mut el = div()
                        .child(run.text)
                        .w(total_w)
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
                    el.into_any_element()
                }))
        }))
}

/// Merge adjacent cells with identical styling into [`Run`]s.
///
/// Wide-char spacer cells (`display_width == 0`) are silently skipped.
/// The cell at `cursor_col` is always isolated into its own run.
/// Runs are split at geom / box-font / plain boundaries.
pub(crate) fn coalesce_runs(
    cells: &[SnapshotCell],
    cursor_col: Option<usize>,
) -> impl Iterator<Item = Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, cell) in cells.iter().enumerate() {
        let w = usize::from(cell.display_width);
        if w == 0 {
            continue;
        }
        let is_cursor = cursor_col == Some(col);
        let use_geom = is_geom_box_char(cell.c);
        let use_box_font = !use_geom && is_box_drawing(cell.c);

        if let Some(last) = runs.last_mut() {
            if !is_cursor
                && !last.is_cursor
                && last.fg == cell.fg
                && last.bg == cell.bg
                && last.bold == cell.bold
                && last.underline == cell.underline
                && last.use_geom == use_geom
                && last.use_box_font == use_box_font
            {
                last.text.push(cell.c);
                last.width += w;
                if use_geom {
                    last.geom_char_widths.push(w as u8);
                }
                continue;
            }
        }
        let geom_char_widths = if use_geom { vec![w as u8] } else { Vec::new() };
        runs.push(Run {
            text: cell.c.to_string(),
            fg: cell.fg,
            bg: cell.bg,
            width: w,
            bold: cell.bold,
            underline: cell.underline,
            is_cursor,
            use_geom,
            use_box_font,
            geom_char_widths,
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

    #[test]
    fn wide_char_spacer_cells_are_skipped() {
        let cells = [cell_wide('あ'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "あ");
        assert_eq!(runs[0].width, 2);
    }

    #[test]
    fn adjacent_plain_cells_merge() {
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "abc");
        assert_eq!(runs[0].width, 3);
    }

    #[test]
    fn different_fg_splits_run() {
        let cells = [cell_rgb('a', 255, 0, 0), cell_rgb('b', 0, 255, 0)];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn different_bg_splits_run() {
        let cells = [cell_bg('a', 255, 0, 0), cell_bg('b', 0, 255, 0)];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn bold_splits_run() {
        let cells = [cell('a'), cell_bold('b')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn cursor_cell_is_isolated() {
        let cells = [cell('a'), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(1)).collect();
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].is_cursor);
        assert!(runs[1].is_cursor);
        assert!(!runs[2].is_cursor);
    }

    #[test]
    fn cursor_at_start() {
        let cells = [cell('a'), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(0)).collect();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].is_cursor);
        assert_eq!(runs[0].text, "a");
        assert_eq!(runs[1].text, "b");
    }

    #[test]
    fn cursor_at_end() {
        let cells = [cell('a'), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, Some(1)).collect();
        assert_eq!(runs.len(), 2);
        assert!(!runs[0].is_cursor);
        assert!(runs[1].is_cursor);
    }

    #[test]
    fn geom_box_chars_are_flagged() {
        let cells = [cell('a'), cell_wide('─'), cell_spacer(), cell('b')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        // a | ─ | b  (spacer skipped)
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].use_geom);
        assert!(runs[1].use_geom);
        assert!(!runs[2].use_geom);
        assert_eq!(runs[1].geom_char_widths, vec![2u8]);
    }

    #[test]
    fn adjacent_geom_chars_merge() {
        let cells = [cell_wide('─'), cell_spacer(), cell_wide('─'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "──");
        assert_eq!(runs[0].width, 4);
        assert_eq!(runs[0].geom_char_widths, vec![2u8, 2u8]);
    }

    #[test]
    fn box_font_chars_are_flagged_not_geom() {
        // Arrow → is in U+2190-U+21FF → use_box_font=true, use_geom=false
        let cells = [cell_wide('→'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].use_geom);
        assert!(runs[0].use_box_font);
    }

    #[test]
    fn geom_and_box_font_split() {
        // ─ (geom) followed by → (box_font) should be separate runs
        let cells = [cell_wide('─'), cell_spacer(), cell_wide('→'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].use_geom);
        assert!(runs[1].use_box_font);
    }

    #[test]
    fn is_geom_box_char_coverage() {
        assert!(is_geom_box_char('─'));           // U+2500
        assert!(is_geom_box_char('│'));           // U+2502
        assert!(is_geom_box_char('┌'));           // U+250C
        assert!(is_geom_box_char('█'));           // U+2588
        assert!(is_geom_box_char('░'));           // U+2591
        assert!(is_geom_box_char('\u{259F}'));    // U+259F upper limit
        assert!(!is_geom_box_char('\u{25A0}'));  // Geometric shapes start
        assert!(!is_geom_box_char('→'));
        assert!(!is_geom_box_char('a'));
    }

    #[test]
    fn is_box_drawing_coverage() {
        assert!(is_box_drawing('→'));   // U+2192
        assert!(is_box_drawing('←'));   // U+2190
        assert!(is_box_drawing('◆'));   // U+25C6
        assert!(is_box_drawing('▶'));   // U+25B6
        assert!(!is_box_drawing('─'));  // geom char, not box_font
        assert!(!is_box_drawing('a'));
    }

    #[test]
    fn cell_width_for_font_variants() {
        assert_eq!(cell_width_for_font("Cica"), 6.5);
        assert_eq!(cell_width_for_font("MS Gothic"), 6.5);
        assert_eq!(cell_width_for_font("Moralerspace Neon HW"), CELL_W);
        assert_eq!(cell_width_for_font("unknown"), CELL_W);
    }
}
