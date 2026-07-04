use gpui::{
    App, Bounds, FontWeight, IntoElement, ParentElement, Rgba, Styled, Window, canvas, fill, point,
    px, rgba, size,
};
use gpui_component::v_flex;

use crate::terminal::{GridSnapshot, ResolvedColor, SnapshotCell};
use crate::theme::Colors;

/// Fixed cell width in pixels for the default font (Moralerspace Neon HW @ 13pt).
/// Moralerspace HW ASCII advance = 525/1000 × 13 = 6.825px; we use 7.8 for
/// comfortable inter-char spacing that was empirically validated in cmd_185.
pub const CELL_W: f32 = 7.8;

/// Return the cell width (in logical pixels) appropriate for the selected font at
/// `text_size = 13pt`.
///
/// Used as a **static fallback** when `TextSystem` is not available (tests, bench).
/// At runtime, [`crate::window::measure_cell_metrics`] supersedes this via `TextSystem::ch_advance`.
///
/// Measured advances (HAdvanceWidth / UPM × 13):
///   Moralerspace Neon HW : ASCII 6.825 px  → use 7.8 (empirical, adds breathing room)
///   Cica                  : ASCII 6.500 px  → use 6.5 (exact fit)
pub fn cell_width_for_font(font: &str) -> f32 {
    match font {
        "Cica" => 6.5,
        _ => CELL_W,
    }
}

/// Family name of the bundled color emoji font (see `main.rs` / CREDITS).
pub const EMOJI_FONT: &str = "Twemoji Mozilla";

/// Build the terminal font with the bundled emoji fallback attached, so emoji
/// resolve to the same (embedded) glyphs on every OS instead of the platform
/// emoji font. gpui inserts user fallbacks ahead of the system fallback chain
/// on both DirectWrite and CoreText.
pub fn terminal_font(family: &str) -> gpui::Font {
    let mut f = gpui::font(family.to_string());
    f.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![EMOJI_FONT.into()]));
    f
}

/// Attach the bundled emoji fallback to an element's inherited text style, so
/// UI chrome (buttons, status bars, tab labels) renders emoji with the same
/// embedded glyphs as the terminal grid. Only the fallback list is touched —
/// family/size/weight keep cascading as before.
pub fn with_emoji_fallback<E: gpui::Styled>(mut el: E) -> E {
    el.text_style()
        .get_or_insert_with(Default::default)
        .font_fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![EMOJI_FONT.into()]));
    el
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

/// A styled run of consecutive terminal cells with identical visual properties.
///
/// Adjacent cells that share the same fg, bg, bold, and underline flags are merged
/// into a single `Run` for efficient GPUI rendering.  The cursor cell is always
/// its own run so colours can be inverted independently.
///
/// Runs are split at geometry-box / plain-text boundaries.
///
/// Note: EAW=Ambiguous characters (arrows U+2190-U+21FF, geometric shapes
/// U+25A0-U+25FF) are rendered with the primary font at 1-cell (narrow) width,
/// following Windows Terminal's de facto standard (PR #2928 — wcswidth=1 for EAW=A).
/// A previous implementation switched to MS Gothic for these; that was wrong because
/// alacritty_terminal already assigns display_width=1 to EAW=A chars, so a full-width
/// MS Gothic glyph would overflow the 1-cell container.
pub(crate) struct Run {
    pub text: String,
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    /// Total display-column width of this run (sum of each cell's `display_width`).
    pub width: usize,
    /// SGR attributes shared by every cell in this run.
    pub style: crate::terminal::CellStyle,
    /// True for the single run that sits at the cursor position.
    pub is_cursor: bool,
    /// True when every char in this run is a geometry-rendered box char (U+2500-U+259F).
    /// The renderer uses `canvas` + `paint_quad` for these.
    pub use_geom: bool,
    /// Per-char display widths (parallel to `text.chars()`), for all runs.
    /// Geometry runs use them for quad sizing; font runs use them to place each
    /// glyph at its exact grid column (Σ width × cw).
    pub char_widths: Vec<u8>,
}

/// Map a resolved terminal color to a GPUI `Rgba`.
pub fn color_to_rgba(color: ResolvedColor) -> Rgba {
    match color {
        ResolvedColor::Rgb(r, g, b) => rgba(u32::from_be_bytes([r, g, b, 0xff])),
        ResolvedColor::Default => Colors::zouge(),
    }
}

/// Resolve the display fg/bg for a [`Run`], applying SGR inverse/dim and
/// block-cursor inversion (in that order — the cursor inverts whatever the
/// cell already displays).
///
/// Returns `(fg, bg_opt)`.  `None` bg means transparent.
fn resolve_run_colors(run: &Run) -> (Rgba, Option<Rgba>) {
    let mut fg = color_to_rgba(run.fg);
    let mut bg = match run.bg {
        ResolvedColor::Rgb(r, g, b) => Some(rgba(u32::from_be_bytes([r, g, b, 0xff]))),
        ResolvedColor::Default => None,
    };
    if run.style.inverse {
        // SGR 7: swap fg/bg. A transparent default bg swaps in as the pane's
        // base color so the inverted text stays legible.
        let new_bg = Some(fg);
        fg = bg.unwrap_or_else(Colors::shikkoku);
        bg = new_bg;
    }
    if run.style.dim {
        // SGR 2 (faint): scale the ink toward black, leaving bg untouched.
        fg = Rgba {
            r: fg.r * 0.6,
            g: fg.g * 0.6,
            b: fg.b * 0.6,
            a: fg.a,
        };
    }
    if run.is_cursor {
        let cursor_bg = fg;
        let cursor_fg = bg.unwrap_or_else(|| rgba(0x1e1e1eff));
        (cursor_fg, Some(cursor_bg))
    } else {
        (fg, bg)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Geometry renderer for U+2500-U+259F
// ─────────────────────────────────────────────────────────────────────────────

/// Draw one box-drawing / block-element character as filled quads.
///
/// `ox`, `oy`  — top-left origin of this character's cell (raw f32 pixels).
/// `cw`        — width of this character's display cell (1-cell-px × display_width).
/// `ch`        — cell height (logical px, = font_size × 1.5 at runtime).
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
    macro_rules! h_full {
        ($yc:expr, $lw:expr) => {
            rect!(ox - TOL, $yc - $lw / 2.0, x1 + TOL, $yc + $lw / 2.0)
        };
    }
    macro_rules! h_left {
        ($yc:expr, $lw:expr) => {
            rect!(ox - TOL, $yc - $lw / 2.0, xm, $yc + $lw / 2.0)
        };
    }
    macro_rules! h_right {
        ($yc:expr, $lw:expr) => {
            rect!(xm, $yc - $lw / 2.0, x1 + TOL, $yc + $lw / 2.0)
        };
    }
    // Vertical full / top-half / bottom-half at given x-center
    macro_rules! v_full {
        ($xc:expr, $lw:expr) => {
            rect!($xc - $lw / 2.0, oy - TOL, $xc + $lw / 2.0, y1 + TOL)
        };
    }
    macro_rules! v_top {
        ($xc:expr, $lw:expr) => {
            rect!($xc - $lw / 2.0, oy - TOL, $xc + $lw / 2.0, ym)
        };
    }
    macro_rules! v_bot {
        ($xc:expr, $lw:expr) => {
            rect!($xc - $lw / 2.0, ym, $xc + $lw / 2.0, y1 + TOL)
        };
    }

    // Paint a filled quad
    macro_rules! q {
        ($b:expr) => {
            window.paint_quad(fill($b, fg))
        };
    }

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
        '┌' => {
            q!(h_right!(ym, lw));
            q!(v_bot!(xm, lw));
        }
        '┐' => {
            q!(h_left!(ym, lw));
            q!(v_bot!(xm, lw));
        }
        '└' => {
            q!(h_right!(ym, lw));
            q!(v_top!(xm, lw));
        }
        '┘' => {
            q!(h_left!(ym, lw));
            q!(v_top!(xm, lw));
        }

        // light + heavy corner variants
        '┍' => {
            q!(h_right!(ym, hw));
            q!(v_bot!(xm, lw));
        }
        '┎' => {
            q!(h_right!(ym, lw));
            q!(v_bot!(xm, hw));
        }
        '┏' => {
            q!(h_right!(ym, hw));
            q!(v_bot!(xm, hw));
        }
        '┑' => {
            q!(h_left!(ym, hw));
            q!(v_bot!(xm, lw));
        }
        '┒' => {
            q!(h_left!(ym, lw));
            q!(v_bot!(xm, hw));
        }
        '┓' => {
            q!(h_left!(ym, hw));
            q!(v_bot!(xm, hw));
        }
        '┕' => {
            q!(h_right!(ym, hw));
            q!(v_top!(xm, lw));
        }
        '┖' => {
            q!(h_right!(ym, lw));
            q!(v_top!(xm, hw));
        }
        '┗' => {
            q!(h_right!(ym, hw));
            q!(v_top!(xm, hw));
        }
        '┙' => {
            q!(h_left!(ym, hw));
            q!(v_top!(xm, lw));
        }
        '┚' => {
            q!(h_left!(ym, lw));
            q!(v_top!(xm, hw));
        }
        '┛' => {
            q!(h_left!(ym, hw));
            q!(v_top!(xm, hw));
        }

        // ── T-junctions ───────────────────────────────────────────────────────
        '├' => {
            q!(h_right!(ym, lw));
            q!(v_full!(xm, lw));
        }
        '┤' => {
            q!(h_left!(ym, lw));
            q!(v_full!(xm, lw));
        }
        '┬' => {
            q!(h_full!(ym, lw));
            q!(v_bot!(xm, lw));
        }
        '┴' => {
            q!(h_full!(ym, lw));
            q!(v_top!(xm, lw));
        }
        '┼' => {
            q!(h_full!(ym, lw));
            q!(v_full!(xm, lw));
        }

        // heavy T-junctions
        '┣' => {
            q!(h_right!(ym, hw));
            q!(v_full!(xm, hw));
        }
        '┫' => {
            q!(h_left!(ym, hw));
            q!(v_full!(xm, hw));
        }
        '┳' => {
            q!(h_full!(ym, hw));
            q!(v_bot!(xm, hw));
        }
        '┻' => {
            q!(h_full!(ym, hw));
            q!(v_top!(xm, hw));
        }
        '╋' => {
            q!(h_full!(ym, hw));
            q!(v_full!(xm, hw));
        }

        // mixed T-junctions → approximate with light
        '┝'..='┞'
        | '┟'..='┠'
        | '┡'..='┢'
        | '┦'..='┧'
        | '┨'..='┩'
        | '┪'
        | '┭'..='┯'
        | '┰'..='┲'
        | '┵'..='┷'
        | '┸'..='┺'
        | '┽'..='┿'
        | '╀'..='╉'
        | '╊' => {
            q!(h_full!(ym, lw));
            q!(v_full!(xm, lw));
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
        '╼' => {
            q!(h_left!(ym, lw));
            q!(h_right!(ym, hw));
        }
        '╽' => {
            q!(v_top!(xm, lw));
            q!(v_bot!(xm, hw));
        }
        '╾' => {
            q!(h_left!(ym, hw));
            q!(h_right!(ym, lw));
        }
        '╿' => {
            q!(v_top!(xm, hw));
            q!(v_bot!(xm, lw));
        }

        // ── Double lines ──────────────────────────────────────────────────────
        '═' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
        }
        '║' => {
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }

        // Double corners (top-left)
        '╔' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }
        '╓' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╒' => {
            q!(h_right!(ym, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }

        // top-right
        '╗' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }
        '╖' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╕' => {
            q!(h_left!(ym, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }

        // bottom-left
        '╚' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }
        '╙' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╘' => {
            q!(h_right!(ym, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }

        // bottom-right
        '╝' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }
        '╜' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╛' => {
            q!(h_left!(ym, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }

        // Double T-junctions
        '╠' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }
        '╟' => {
            q!(h_right!(d1y, lw));
            q!(h_right!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╞' => {
            q!(h_right!(ym, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }
        '╣' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }
        '╢' => {
            q!(h_left!(d1y, lw));
            q!(h_left!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╡' => {
            q!(h_left!(ym, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }
        '╦' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }
        '╥' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_bot!(xm, lw));
        }
        '╤' => {
            q!(h_full!(ym, lw));
            q!(v_bot!(d1x, lw));
            q!(v_bot!(d2x, lw));
        }
        '╩' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }
        '╨' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_top!(xm, lw));
        }
        '╧' => {
            q!(h_full!(ym, lw));
            q!(v_top!(d1x, lw));
            q!(v_top!(d2x, lw));
        }
        '╬' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }
        '╫' => {
            q!(h_full!(d1y, lw));
            q!(h_full!(d2y, lw));
            q!(v_full!(xm, lw));
        }
        '╪' => {
            q!(h_full!(ym, lw));
            q!(v_full!(d1x, lw));
            q!(v_full!(d2x, lw));
        }

        // ── Block elements U+2580-U+259F ──────────────────────────────────────
        '▀' => q!(rect!(ox, oy, x1, ym)), // upper half
        '▁' => q!(rect!(ox, oy + ch * 7.0 / 8.0, x1, y1)), // lower 1/8
        '▂' => q!(rect!(ox, oy + ch * 6.0 / 8.0, x1, y1)),
        '▃' => q!(rect!(ox, oy + ch * 5.0 / 8.0, x1, y1)),
        '▄' => q!(rect!(ox, ym, x1, y1)), // lower half
        '▅' => q!(rect!(ox, oy + ch * 3.0 / 8.0, x1, y1)),
        '▆' => q!(rect!(ox, oy + ch * 2.0 / 8.0, x1, y1)),
        '▇' => q!(rect!(ox, oy + ch * 1.0 / 8.0, x1, y1)),
        '█' => q!(rect!(ox, oy, x1, y1)), // full block
        '▉' => q!(rect!(ox, oy, ox + cw * 7.0 / 8.0, y1)),
        '▊' => q!(rect!(ox, oy, ox + cw * 6.0 / 8.0, y1)),
        '▋' => q!(rect!(ox, oy, ox + cw * 5.0 / 8.0, y1)),
        '▌' => q!(rect!(ox, oy, xm, y1)), // left half
        '▍' => q!(rect!(ox, oy, ox + cw * 3.0 / 8.0, y1)),
        '▎' => q!(rect!(ox, oy, ox + cw * 2.0 / 8.0, y1)),
        '▏' => q!(rect!(ox, oy, ox + cw * 1.0 / 8.0, y1)),
        '▐' => q!(rect!(xm, oy, x1, y1)), // right half
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
        '▔' => q!(rect!(ox, oy, x1, oy + ch / 8.0)), // upper 1/8
        '▕' => q!(rect!(ox + cw * 7.0 / 8.0, oy, x1, y1)), // right 1/8
        '▖' => q!(rect!(ox, ym, xm, y1)),            // lower-left quad
        '▗' => q!(rect!(xm, ym, x1, y1)),            // lower-right quad
        '▘' => q!(rect!(ox, oy, xm, ym)),            // upper-left quad
        '▙' => {
            q!(rect!(ox, oy, xm, y1));
            q!(rect!(xm, ym, x1, y1));
        }
        '▚' => {
            q!(rect!(ox, oy, xm, ym));
            q!(rect!(xm, ym, x1, y1));
        }
        '▛' => {
            q!(rect!(ox, oy, x1, ym));
            q!(rect!(ox, ym, xm, y1));
        }
        '▜' => {
            q!(rect!(ox, oy, x1, ym));
            q!(rect!(xm, ym, x1, y1));
        }
        '▝' => q!(rect!(xm, oy, x1, ym)), // upper-right quad
        '▞' => {
            q!(rect!(xm, oy, x1, ym));
            q!(rect!(ox, ym, xm, y1));
        }
        '▟' => {
            q!(rect!(xm, oy, x1, ym));
            q!(rect!(ox, ym, x1, y1));
        }

        _ => return false,
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────────

/// Render the terminal grid.
///
/// `cw` and `ch` are the logical-pixel cell dimensions measured at runtime from
/// the active font via [`crate::window::measure_cell_metrics`]
/// (`cw` = `ch_advance`; `ch` = `font_size × 1.5`).
/// Fall back to [`CELL_W`] / hardcoded `20.0` when `TextSystem` is unavailable.
/// Selection highlight color: translucent steel blue painted over the row's
/// ink, so the selected text stays legible underneath.
const SELECTION_RGBA: u32 = 0x3465a480;

/// Compute the selected column span of one row for an inclusive, linear
/// (reading-order) selection. Rows strictly between the endpoints are selected
/// full-width. Returns `None` when the row is outside the selection.
///
/// The span is snapped to whole characters: wide glyphs (CJK, emoji) occupy a
/// base cell plus a `display_width == 0` spacer, and a pointer landing on
/// either half must select the entire glyph. Without the snap the highlight
/// starts or ends mid-glyph and disagrees with the copied text.
pub(crate) fn selection_cols_for_row(
    selection: Option<((usize, usize), (usize, usize))>,
    row: usize,
    grid_cols: usize,
    cells: &[crate::terminal::SnapshotCell],
) -> Option<(usize, usize)> {
    let (start, end) = selection?;
    if row < start.0 || row > end.0 {
        return None;
    }
    let mut c0 = if row == start.0 { start.1 } else { 0 };
    let mut c1 = if row == end.0 {
        (end.1 + 1).min(grid_cols)
    } else {
        grid_cols
    };
    // Start on a spacer → pull back to the wide glyph's base cell.
    while c0 > 0 && cells.get(c0).is_some_and(|c| c.display_width == 0) {
        c0 -= 1;
    }
    // Cell just past the end is a spacer → the end cell is a wide base whose
    // right half would be left unhighlighted; extend over the spacer.
    while c1 < grid_cols && cells.get(c1).is_some_and(|c| c.display_width == 0) {
        c1 += 1;
    }
    (c0 < c1).then_some((c0, c1))
}

pub fn render_grid(
    snap: &GridSnapshot,
    font: &str,
    cw: f32,
    ch: f32,
    // Normalized inclusive (start, end) cell range of the mouse selection.
    selection: Option<((usize, usize), (usize, usize))>,
    // IME composition (preedit) text, drawn inline at the terminal cursor.
    // Painted here — in the cursor row's canvas — because paint calls issued
    // from the absolute viewport-overlay canvas never reach the screen
    // (empirically verified 2026-07-03).
    ime_preedit: Option<String>,
) -> impl IntoElement {
    let (cursor_row, cursor_col) = snap.cursor;
    let grid_cols = snap.cols;
    let font_name = font.to_string();
    v_flex()
        .font_family(font.to_string())
        .text_size(px(13.))
        .children(snap.cells.iter().enumerate().map(move |(row_idx, row)| {
            let cur_col = if row_idx == cursor_row {
                Some(cursor_col)
            } else {
                None
            };
            // One canvas per row: every run is painted at its exact column offset
            // `col × cw` from the row origin. Laying runs out as separate flex
            // children lets GPUI quantize each child width to device pixels, and
            // the rounding error accumulates left-to-right (measured +4 px by
            // column 114 at 150% DPI), so vertical borders in adjacent rows no
            // longer line up. Absolute per-run placement inside a single canvas
            // has no such accumulation.
            let runs: Vec<Run> = coalesce_runs(row, cur_col).collect();
            let total_cols: usize = runs.iter().map(|r| r.width).sum();
            let sel_cols = selection_cols_for_row(selection, row_idx, grid_cols, row);
            let preedit = if row_idx == cursor_row {
                ime_preedit.clone().filter(|s| !s.is_empty())
            } else {
                None
            };
            let font_name = font_name.clone();

            canvas(
                |_bounds, _window, _cx| (),
                move |bounds, (), window, cx: &mut App| {
                    let ox = f32::from(bounds.origin.x);
                    let oy = f32::from(bounds.origin.y);
                    let font_size = px(13.);
                    let line_height = px(ch);
                    let mut col = 0usize;

                    for run in runs {
                        let x = ox + col as f32 * cw;
                        col += run.width;
                        let (fg_rgba, bg_opt) = resolve_run_colors(&run);

                        if let Some(bg) = bg_opt {
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(px(x), px(oy)),
                                    size: size(px(cw * run.width as f32), px(ch)),
                                },
                                bg,
                            ));
                        }

                        if run.use_geom {
                            // ── Box drawing / block elements as filled quads ───
                            let mut x_off = x;
                            for (c, dw) in run.text.chars().zip(run.char_widths.iter().copied()) {
                                let char_cw = cw * dw as f32;
                                paint_box_char(c, x_off, oy, char_cw, ch, fg_rgba, window);
                                x_off += char_cw;
                            }
                            continue;
                        }

                        // SGR 8 (hidden): bg is painted, ink is not.
                        if run.style.hidden {
                            continue;
                        }

                        // Blank runs (all spaces, no decoration) have no ink at
                        // all — bg is already painted above. Most of a terminal
                        // grid is blank, so skipping the shape+paint here is the
                        // single biggest per-frame saving.
                        if !run.style.underline
                            && !run.style.undercurl
                            && !run.style.strikeout
                            && run.text.bytes().all(|b| b == b' ')
                        {
                            continue;
                        }

                        // ── Font-rendered text: grid-exact glyph placement ─────
                        //
                        // Flowing text through a div lets the shaper advance each
                        // glyph by its natural font advance, which drifts off the
                        // `cw`-snapped grid. Instead, paint via shape_line:
                        //   - all-narrow runs: one shape with force_width = cw,
                        //     which re-pins every glyph to `n × cw`.
                        //   - mixed-width runs: split into maximal segments of
                        //     uniform display width and shape each segment once
                        //     with force_width = width × cw (glyph n of an
                        //     all-wide segment belongs at n × 2cw).
                        let mut run_font = terminal_font(&font_name);
                        if run.style.bold {
                            run_font.weight = FontWeight::BOLD;
                        }
                        if run.style.italic {
                            // The bundled mono font has no italic face; the
                            // platform synthesizes an oblique where it can.
                            run_font.style = gpui::FontStyle::Italic;
                        }
                        let fg_hsla: gpui::Hsla = fg_rgba.into();
                        let underline_style = (run.style.underline || run.style.undercurl)
                            .then_some(gpui::UnderlineStyle {
                                color: Some(fg_hsla),
                                thickness: px(1.),
                                wavy: run.style.undercurl,
                            });
                        let strikethrough_style =
                            run.style.strikeout.then_some(gpui::StrikethroughStyle {
                                color: Some(fg_hsla),
                                thickness: px(1.),
                            });

                        let all_narrow = run.char_widths.iter().all(|&w| w == 1);
                        if all_narrow {
                            let text_run = gpui::TextRun {
                                len: run.text.len(),
                                font: run_font,
                                color: fg_hsla,
                                background_color: None,
                                underline: underline_style,
                                strikethrough: strikethrough_style,
                            };
                            let line = window.text_system().shape_line(
                                run.text.into(),
                                font_size,
                                &[text_run],
                                Some(px(cw)),
                            );
                            let _ = line.paint(point(px(x), px(oy)), line_height, window, cx);
                        } else {
                            // Maximal segments of uniform display width, one
                            // shape_line each: glyph n of an all-wide segment
                            // belongs at n × 2cw, so force_width still applies.
                            let mut x_off = x;
                            let mut seg = String::new();
                            let mut seg_w = 0u8;
                            let flush = |seg: &mut String,
                                         seg_w: u8,
                                         x_off: &mut f32,
                                         window: &mut Window,
                                         cx: &mut App| {
                                if seg.is_empty() {
                                    return;
                                }
                                let cells = seg.chars().count() as f32 * seg_w as f32;
                                let text_run = gpui::TextRun {
                                    len: seg.len(),
                                    font: run_font.clone(),
                                    color: fg_hsla,
                                    background_color: None,
                                    underline: underline_style,
                                    strikethrough: strikethrough_style,
                                };
                                let line = window.text_system().shape_line(
                                    std::mem::take(seg).into(),
                                    font_size,
                                    &[text_run],
                                    Some(px(cw * seg_w as f32)),
                                );
                                let _ =
                                    line.paint(point(px(*x_off), px(oy)), line_height, window, cx);
                                *x_off += cw * cells;
                            };
                            for (c, w) in run.text.chars().zip(run.char_widths.iter().copied()) {
                                if w != seg_w {
                                    flush(&mut seg, seg_w, &mut x_off, window, cx);
                                    seg_w = w;
                                }
                                seg.push(c);
                            }
                            flush(&mut seg, seg_w, &mut x_off, window, cx);
                        }
                    }

                    // Mouse-selection highlight: painted last (over this row's
                    // ink) with a translucent color, so it can never be hidden
                    // by cell backgrounds and the text stays readable. Painted
                    // here rather than in the viewport overlay because the row
                    // canvas already lives in grid coordinates.
                    if let Some((c0, c1)) = sel_cols {
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(px(ox + c0 as f32 * cw), px(oy)),
                                size: size(px((c1 - c0) as f32 * cw), px(ch)),
                            },
                            rgba(SELECTION_RGBA),
                        ));
                    }

                    // IME preedit: drawn over the cursor row starting at the
                    // cursor column (dark-blue background + underline), so
                    // composition text is visible before it is committed.
                    if let Some(pre) = &preedit {
                        let fg: gpui::Hsla = rgba(0xffffffff).into();
                        let text_run = gpui::TextRun {
                            len: pre.len(),
                            font: terminal_font(&font_name),
                            color: fg,
                            background_color: Some(rgba(0x1e3a5fff).into()),
                            underline: Some(gpui::UnderlineStyle {
                                color: Some(fg),
                                thickness: px(1.),
                                wavy: false,
                            }),
                            strikethrough: None,
                        };
                        let line = window.text_system().shape_line(
                            pre.clone().into(),
                            font_size,
                            &[text_run],
                            None,
                        );
                        let origin = point(px(ox + cursor_col as f32 * cw), px(oy));
                        let _ = line.paint_background(origin, line_height, window, cx);
                        let _ = line.paint(origin, line_height, window, cx);
                    }
                },
            )
            .w(px(cw * total_cols.max(1) as f32))
            .h(px(ch))
        }))
}

/// Merge adjacent cells with identical styling into [`Run`]s.
///
/// Wide-char spacer cells (`display_width == 0`) are silently skipped.
/// The cell at `cursor_col` is always isolated into its own run.
/// Runs are split at geom / plain boundaries.
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

        if let Some(last) = runs.last_mut() {
            if !is_cursor
                && !last.is_cursor
                && last.fg == cell.fg
                && last.bg == cell.bg
                && last.style == cell.style
                && last.use_geom == use_geom
            {
                last.text.push(cell.c);
                last.width += w;
                last.char_widths.push(w as u8);
                continue;
            }
        }
        runs.push(Run {
            text: cell.c.to_string(),
            fg: cell.fg,
            bg: cell.bg,
            width: w,
            style: cell.style,
            is_cursor,
            use_geom,
            char_widths: vec![w as u8],
        });
    }
    runs.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{CellStyle, ResolvedColor, SnapshotCell};

    fn cell(c: char) -> SnapshotCell {
        SnapshotCell {
            c,
            ..SnapshotCell::blank()
        }
    }

    fn cell_wide(c: char) -> SnapshotCell {
        SnapshotCell {
            c,
            display_width: 2,
            ..SnapshotCell::blank()
        }
    }

    fn cell_spacer() -> SnapshotCell {
        SnapshotCell {
            display_width: 0,
            ..SnapshotCell::blank()
        }
    }

    fn cell_rgb(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            fg: ResolvedColor::Rgb(r, g, b),
            ..SnapshotCell::blank()
        }
    }

    fn cell_bg(c: char, r: u8, g: u8, b: u8) -> SnapshotCell {
        SnapshotCell {
            c,
            bg: ResolvedColor::Rgb(r, g, b),
            ..SnapshotCell::blank()
        }
    }

    fn cell_styled(c: char, style: CellStyle) -> SnapshotCell {
        SnapshotCell {
            c,
            style,
            ..SnapshotCell::blank()
        }
    }

    fn cell_bold(c: char) -> SnapshotCell {
        cell_styled(
            c,
            CellStyle {
                bold: true,
                ..CellStyle::default()
            },
        )
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
    fn any_style_difference_splits_run() {
        let italic = CellStyle {
            italic: true,
            ..CellStyle::default()
        };
        let cells = [cell_styled('a', italic), cell('b'), cell('c')];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].style.italic);
        assert_eq!(runs[1].text, "bc");
    }

    fn styled_run(fg: ResolvedColor, bg: ResolvedColor, style: CellStyle) -> Run {
        Run {
            text: "x".into(),
            fg,
            bg,
            width: 1,
            style,
            is_cursor: false,
            use_geom: false,
            char_widths: vec![1],
        }
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let run = styled_run(
            ResolvedColor::Rgb(200, 10, 10),
            ResolvedColor::Default,
            CellStyle {
                inverse: true,
                ..CellStyle::default()
            },
        );
        let (fg, bg) = resolve_run_colors(&run);
        // fg becomes the pane base color (transparent default bg), bg the ink.
        assert_eq!(fg, Colors::shikkoku());
        assert_eq!(bg, Some(rgba(0xc80a0aff)));
    }

    #[test]
    fn dim_scales_fg_only() {
        let run = styled_run(
            ResolvedColor::Rgb(255, 0, 0),
            ResolvedColor::Rgb(0, 0, 255),
            CellStyle {
                dim: true,
                ..CellStyle::default()
            },
        );
        let (fg, bg) = resolve_run_colors(&run);
        assert!((fg.r - 0.6).abs() < 1e-5);
        assert_eq!(bg, Some(rgba(0x0000ffff)));
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
        assert_eq!(runs[1].char_widths, vec![2u8]);
    }

    #[test]
    fn adjacent_geom_chars_merge() {
        let cells = [cell_wide('─'), cell_spacer(), cell_wide('─'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "──");
        assert_eq!(runs[0].width, 4);
        assert_eq!(runs[0].char_widths, vec![2u8, 2u8]);
    }

    #[test]
    fn arrow_is_plain_narrow() {
        // Arrow → (U+2192, EAW=A) — rendered with primary font at 1-cell width.
        // No special font override; follows wt standard (EAW=A = narrow).
        let cells = [cell_wide('→'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].use_geom);
    }

    #[test]
    fn geom_and_plain_split() {
        // ─ (geom) followed by → (plain/narrow) should be separate runs
        // because use_geom differs.
        let cells = [cell_wide('─'), cell_spacer(), cell_wide('→'), cell_spacer()];
        let runs: Vec<_> = coalesce_runs(&cells, None).collect();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].use_geom);
        assert!(!runs[1].use_geom);
    }

    #[test]
    fn is_geom_box_char_coverage() {
        assert!(is_geom_box_char('─')); // U+2500
        assert!(is_geom_box_char('│')); // U+2502
        assert!(is_geom_box_char('┌')); // U+250C
        assert!(is_geom_box_char('█')); // U+2588
        assert!(is_geom_box_char('░')); // U+2591
        assert!(is_geom_box_char('\u{259F}')); // U+259F upper limit
        assert!(!is_geom_box_char('\u{25A0}')); // Geometric shapes start
        assert!(!is_geom_box_char('→'));
        assert!(!is_geom_box_char('a'));
    }

    #[test]
    fn arrow_and_diamond_are_not_geom() {
        // Arrows (U+2190-U+21FF) and geometric shapes (U+25A0-U+25FF)
        // are NOT geometry-rendered — they use the primary font at 1-cell width.
        assert!(!is_geom_box_char('→')); // U+2192
        assert!(!is_geom_box_char('←')); // U+2190
        assert!(!is_geom_box_char('◆')); // U+25C6
        assert!(!is_geom_box_char('▶')); // U+25B6
        // But box-drawing / block-elements ARE geometry:
        assert!(is_geom_box_char('─')); // U+2500
        assert!(is_geom_box_char('█')); // U+2588
    }

    fn narrow_row(cols: usize) -> Vec<crate::terminal::SnapshotCell> {
        vec![crate::terminal::SnapshotCell::blank(); cols]
    }

    /// Row where the cell at `base` holds a wide glyph (display_width 2)
    /// followed by its spacer (display_width 0).
    fn wide_row(cols: usize, base: usize) -> Vec<crate::terminal::SnapshotCell> {
        let mut row = narrow_row(cols);
        row[base].c = 'あ';
        row[base].display_width = 2;
        row[base + 1].display_width = 0;
        row
    }

    #[test]
    fn selection_cols_none_when_no_selection() {
        assert_eq!(selection_cols_for_row(None, 3, 80, &narrow_row(80)), None);
    }

    #[test]
    fn selection_cols_single_row() {
        let sel = Some(((5, 10), (5, 20)));
        assert_eq!(
            selection_cols_for_row(sel, 5, 80, &narrow_row(80)),
            Some((10, 21))
        );
        assert_eq!(selection_cols_for_row(sel, 4, 80, &narrow_row(80)), None);
        assert_eq!(selection_cols_for_row(sel, 6, 80, &narrow_row(80)), None);
    }

    #[test]
    fn selection_cols_multi_row_middle_is_full_width() {
        let sel = Some(((2, 30), (4, 10)));
        assert_eq!(
            selection_cols_for_row(sel, 2, 80, &narrow_row(80)),
            Some((30, 80))
        );
        assert_eq!(
            selection_cols_for_row(sel, 3, 80, &narrow_row(80)),
            Some((0, 80))
        );
        assert_eq!(
            selection_cols_for_row(sel, 4, 80, &narrow_row(80)),
            Some((0, 11))
        );
    }

    #[test]
    fn selection_cols_end_clamped_to_grid_width() {
        let sel = Some(((0, 0), (0, 200)));
        assert_eq!(
            selection_cols_for_row(sel, 0, 80, &narrow_row(80)),
            Some((0, 80))
        );
    }

    #[test]
    fn selection_cols_start_on_wide_spacer_snaps_to_base() {
        // Wide glyph at col 10 (spacer at 11); anchoring on the spacer must
        // highlight from the base cell.
        let sel = Some(((0, 11), (0, 20)));
        assert_eq!(
            selection_cols_for_row(sel, 0, 80, &wide_row(80, 10)),
            Some((10, 21))
        );
    }

    #[test]
    fn selection_cols_end_on_wide_base_extends_over_spacer() {
        // Selection ends ON the wide base at col 10 → the highlight must
        // cover its spacer too, or the glyph looks half-selected.
        let sel = Some(((0, 2), (0, 10)));
        assert_eq!(
            selection_cols_for_row(sel, 0, 80, &wide_row(80, 10)),
            Some((2, 12))
        );
    }

    #[test]
    fn cell_width_for_font_variants() {
        assert_eq!(cell_width_for_font("Cica"), 6.5);
        assert_eq!(cell_width_for_font("Moralerspace Neon HW"), CELL_W);
        assert_eq!(cell_width_for_font("unknown"), CELL_W);
    }
}
