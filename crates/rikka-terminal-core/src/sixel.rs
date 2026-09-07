//! Sixel graphics (DCS `q`) — the ConPTY-compatible image path.
//!
//! ConPTY strips kitty's APC but passes Sixel DCS through (Windows Terminal
//! 1.22+), so this is how local WSL shells get inline images. The design
//! deliberately converges on the kitty-graphics infrastructure instead of
//! building a second placement tracker:
//!
//! 1. [`SixelScanner`] passively watches the PTY byte stream for
//!    `ESC P .. q .. ESC \` (same pattern as the OSC/APC observers — vte
//!    swallows DCS payloads it doesn't understand, so watching is safe).
//! 2. [`decode`] turns the payload into an RGBA image.
//! 3. The caller stores the image in the shared [`KittyImageStore`] under a
//!    synthetic id and *injects Unicode placeholder cells into the parser*
//!    (see [`placeholder_bytes`]). The image then lives in ordinary grid
//!    cells: scrollback, alt-screen and resize behaviour come for free, and
//!    the renderer's existing kitty path paints it. This also reproduces
//!    sixel scrolling semantics — the cursor ends up on the line below the
//!    image, and the image scrolls away with the text around it.

use alacritty_terminal::vte::ansi::Rgb;

use super::kitty_graphics::{PLACEHOLDER, ROWCOL_DIACRITICS};

/// Ceiling on a single sixel payload (a 4096×4096 image at 1 byte per
/// pixel-column is far below this; runaway streams get dropped).
const MAX_SIXEL_BYTES: usize = 32 * 1024 * 1024;

/// Hard limits on the decoded canvas, matching common terminal caps.
const MAX_WIDTH: usize = 4096;
const MAX_HEIGHT: usize = 4096;

/// Synthetic kitty-store ids for sixel images live at the top of the 24-bit
/// space representable by an RGB-fg placeholder, away from the small integers
/// kitty clients typically pick.
pub const SIXEL_ID_BASE: u32 = 0xE0_0000;
const SIXEL_ID_SPAN: u32 = 0x1F_0000;

// ── passive DCS scanner ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScanState {
    Ground,
    Esc,
    /// Inside a DCS, parameters not yet terminated by the final byte.
    DcsParams,
    /// Inside a sixel DCS (`q` seen): accumulating payload.
    Sixel,
    /// Inside a non-sixel DCS: skipping until ST.
    OtherDcs,
    /// Saw ESC inside a DCS — next byte decides ST or payload continuation.
    SixelEsc,
    OtherDcsEsc,
}

/// Watches the raw PTY stream for sixel DCS sequences. Feed every byte;
/// returns the complete sequence (params + payload, without the `ESC P`
/// introducer, final `q`, or `ESC \` terminator... params ARE included, see
/// [`SixelSequence`]) when one terminates.
pub struct SixelScanner {
    state: ScanState,
    /// Parameter bytes between `ESC P` and the final `q` (e.g. `0;1;0`).
    params: Vec<u8>,
    /// An intermediate byte (0x20–0x2F) was seen — the DCS final can no
    /// longer be sixel (`+q` is XTGETTCAP, `$q` is DECRQSS, …).
    has_intermediate: bool,
    payload: Vec<u8>,
}

/// A complete sixel transmission: the DCS parameters and the sixel data.
pub struct SixelSequence {
    /// `P2` background-select parameter (1 = 0-bits stay transparent).
    pub transparent_background: bool,
    pub data: Vec<u8>,
}

impl Default for SixelScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SixelScanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            params: Vec::new(),
            has_intermediate: false,
            payload: Vec::new(),
        }
    }

    pub fn advance(&mut self, byte: u8) -> Option<SixelSequence> {
        match self.state {
            ScanState::Ground => {
                if byte == 0x1b {
                    self.state = ScanState::Esc;
                }
                None
            }
            ScanState::Esc => {
                if byte == b'P' {
                    self.state = ScanState::DcsParams;
                    self.params.clear();
                    self.has_intermediate = false;
                } else if byte == 0x1b {
                    // stay in Esc
                } else {
                    self.state = ScanState::Ground;
                }
                None
            }
            ScanState::DcsParams => {
                match byte {
                    b'q' if !self.has_intermediate => {
                        self.state = ScanState::Sixel;
                        self.payload.clear();
                    }
                    b'0'..=b'9' | b';' => self.params.push(byte),
                    0x20..=0x2f => self.has_intermediate = true,
                    0x1b => self.state = ScanState::Esc,
                    // Any other final byte (or `q` after an intermediate —
                    // XTGETTCAP/DECRQSS): not a sixel DCS.
                    0x40..=0x7e => self.state = ScanState::OtherDcs,
                    _ => {}
                }
                None
            }
            ScanState::Sixel => {
                if byte == 0x1b {
                    self.state = ScanState::SixelEsc;
                } else if self.payload.len() < MAX_SIXEL_BYTES {
                    self.payload.push(byte);
                } else {
                    // Runaway stream: abandon, skip to ST.
                    self.payload.clear();
                    self.state = ScanState::OtherDcs;
                }
                None
            }
            ScanState::SixelEsc => {
                if byte == b'\\' {
                    self.state = ScanState::Ground;
                    let transparent = self
                        .params
                        .split(|&b| b == b';')
                        .nth(1)
                        .is_some_and(|p| p == b"1");
                    Some(SixelSequence {
                        transparent_background: transparent,
                        data: std::mem::take(&mut self.payload),
                    })
                } else {
                    // ESC inside payload that wasn't ST — sixel data is
                    // printable-only, so treat as a broken stream.
                    self.state = if byte == 0x1b {
                        ScanState::SixelEsc
                    } else {
                        self.payload.clear();
                        ScanState::Ground
                    };
                    None
                }
            }
            ScanState::OtherDcs => {
                if byte == 0x1b {
                    self.state = ScanState::OtherDcsEsc;
                }
                None
            }
            ScanState::OtherDcsEsc => {
                self.state = match byte {
                    b'\\' => ScanState::Ground,
                    0x1b => ScanState::OtherDcsEsc,
                    _ => ScanState::OtherDcs,
                };
                None
            }
        }
    }
}

// ── decoder ───────────────────────────────────────────────────────────────────

/// The VT340 default color registers (RGB percentages from the DEC STD-070
/// palette, as adopted by libsixel and xterm).
const VT340_PALETTE: [(u16, u16, u16); 16] = [
    (0, 0, 0),
    (20, 20, 80),
    (80, 13, 13),
    (20, 80, 20),
    (80, 20, 80),
    (20, 80, 80),
    (80, 80, 20),
    (53, 53, 53),
    (26, 26, 26),
    (33, 33, 60),
    (60, 26, 26),
    (33, 60, 33),
    (60, 33, 60),
    (33, 60, 60),
    (60, 60, 33),
    (80, 80, 80),
];

fn pct(p: u16) -> u8 {
    ((p.min(100) as u32 * 255 + 50) / 100) as u8
}

/// DEC HLS → RGB. DEC's hue circle is rotated: 0° = blue (not red), so
/// shift by +240° before the standard HLS conversion.
fn hls_to_rgb(h: u16, l: u16, s: u16) -> Rgb {
    let h = f64::from((h + 240) % 360);
    let l = f64::from(l.min(100)) / 100.0;
    let s = f64::from(s.min(100)) / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f64| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb {
        r: to8(r1),
        g: to8(g1),
        b: to8(b1),
    }
}

/// A decoded sixel image (straight RGBA, row-major).
pub struct SixelImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Decode a sixel data stream (the bytes between the DCS final `q` and ST).
///
/// `transparent_background`: when false (P2 ≠ 1) untouched pixels are left
/// transparent anyway — the terminal background shows through, which is
/// visually identical to a background fill and composits correctly over
/// theme changes.
pub fn decode(data: &[u8]) -> Option<SixelImage> {
    let mut palette: Vec<Rgb> = VT340_PALETTE
        .iter()
        .map(|&(r, g, b)| Rgb {
            r: pct(r),
            g: pct(g),
            b: pct(b),
        })
        .collect();
    palette.resize(256, Rgb { r: 0, g: 0, b: 0 });

    let mut canvas: Vec<u8> = Vec::new(); // RGBA
    // Allocation dims (geometric over-growth allowed) vs the logical extent
    // every draw/raster call actually asked for — output sizing must use the
    // logical pair, or the over-allocation would pad the produced image.
    let mut canvas_w: usize = 0;
    let mut canvas_h: usize = 0;
    let mut logical_w: usize = 0;
    let mut logical_h: usize = 0;

    let mut x: usize = 0;
    let mut band: usize = 0; // each band is 6 pixels tall
    let mut color = palette[0];
    let mut max_x: usize = 0;
    let mut max_y: usize = 0;

    let mut i = 0;

    // Grow the canvas to hold at least (w, h). Reallocates row-major.
    #[allow(clippy::too_many_arguments)]
    fn ensure_size(
        canvas: &mut Vec<u8>,
        canvas_w: &mut usize,
        canvas_h: &mut usize,
        logical_w: &mut usize,
        logical_h: &mut usize,
        w: usize,
        h: usize,
    ) -> bool {
        let (w, h) = (w.min(MAX_WIDTH), h.min(MAX_HEIGHT));
        *logical_w = (*logical_w).max(w);
        *logical_h = (*logical_h).max(h);
        if w <= *canvas_w && h <= *canvas_h {
            return true;
        }
        // Grow the ALLOCATION geometrically: without raster attributes every
        // 6px band used to reallocate + copy the whole canvas, O(n²) bytes
        // over the image height — a large sixel stalled the reader thread
        // for seconds. Output sizing reads the logical dims, so the slack
        // never pads the produced image.
        let new_w = w.max((*canvas_w * 2).min(MAX_WIDTH));
        let new_h = h.max((*canvas_h * 2).min(MAX_HEIGHT));
        let mut next = vec![0u8; new_w * new_h * 4];
        for row in 0..*canvas_h {
            let src = row * *canvas_w * 4;
            let dst = row * new_w * 4;
            next[dst..dst + *canvas_w * 4].copy_from_slice(&canvas[src..src + *canvas_w * 4]);
        }
        *canvas = next;
        *canvas_w = new_w;
        *canvas_h = new_h;
        true
    }

    // Parse an unsigned decimal number at data[i..], advancing i.
    fn number(data: &[u8], i: &mut usize) -> usize {
        let mut n: usize = 0;
        while let Some(&b @ b'0'..=b'9') = data.get(*i) {
            n = (n * 10 + (b - b'0') as usize).min(1_000_000);
            *i += 1;
        }
        n
    }

    while i < data.len() {
        let b = data[i];
        match b {
            b'"' => {
                // Raster attributes: Pan;Pad;Ph;Pv — pre-size the canvas.
                i += 1;
                let _pan = number(data, &mut i);
                if data.get(i) == Some(&b';') {
                    i += 1;
                }
                let _pad = number(data, &mut i);
                let mut ph = 0;
                let mut pv = 0;
                if data.get(i) == Some(&b';') {
                    i += 1;
                    ph = number(data, &mut i);
                }
                if data.get(i) == Some(&b';') {
                    i += 1;
                    pv = number(data, &mut i);
                }
                if ph > 0 && pv > 0 {
                    ensure_size(
                        &mut canvas,
                        &mut canvas_w,
                        &mut canvas_h,
                        &mut logical_w,
                        &mut logical_h,
                        ph,
                        pv,
                    );
                }
            }
            b'#' => {
                // Color: #Pc or #Pc;Pu;Px;Py;Pz
                i += 1;
                let reg = number(data, &mut i).min(255);
                if data.get(i) == Some(&b';') {
                    i += 1;
                    let system = number(data, &mut i);
                    let mut comp = [0usize; 3];
                    for c in &mut comp {
                        if data.get(i) == Some(&b';') {
                            i += 1;
                            *c = number(data, &mut i);
                        }
                    }
                    palette[reg] = match system {
                        1 => hls_to_rgb(comp[0] as u16, comp[1] as u16, comp[2] as u16),
                        _ => Rgb {
                            r: pct(comp[0] as u16),
                            g: pct(comp[1] as u16),
                            b: pct(comp[2] as u16),
                        },
                    };
                }
                color = palette[reg];
            }
            b'!' => {
                // Repeat: !n<sixel>
                i += 1;
                let n = number(data, &mut i);
                if let Some(&c @ 0x3f..=0x7e) = data.get(i) {
                    i += 1;
                    draw_sixel(
                        &mut canvas,
                        &mut canvas_w,
                        &mut canvas_h,
                        &mut logical_w,
                        &mut logical_h,
                        &mut x,
                        band,
                        c,
                        n,
                        color,
                        &mut max_x,
                        &mut max_y,
                    );
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                band += 1;
                x = 0;
                i += 1;
            }
            0x3f..=0x7e => {
                draw_sixel(
                    &mut canvas,
                    &mut canvas_w,
                    &mut canvas_h,
                    &mut logical_w,
                    &mut logical_h,
                    &mut x,
                    band,
                    b,
                    1,
                    color,
                    &mut max_x,
                    &mut max_y,
                );
                i += 1;
            }
            _ => i += 1, // CR/LF/space and anything else: skip
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_sixel(
        canvas: &mut Vec<u8>,
        canvas_w: &mut usize,
        canvas_h: &mut usize,
        logical_w: &mut usize,
        logical_h: &mut usize,
        x: &mut usize,
        band: usize,
        ch: u8,
        repeat: usize,
        color: Rgb,
        max_x: &mut usize,
        max_y: &mut usize,
    ) {
        let bits = ch - 0x3f;
        let repeat = repeat.clamp(1, MAX_WIDTH);
        let y0 = band * 6;
        if y0 >= MAX_HEIGHT || *x >= MAX_WIDTH {
            *x += repeat;
            return;
        }
        // Highest set bit decides how tall this column really is
        // (u8::leading_zeros counts from bit 7, so height = 8 − lz).
        let tall = if bits == 0 {
            0
        } else {
            8 - bits.leading_zeros() as usize
        };
        let needed_h = if bits == 0 { y0 + 1 } else { y0 + tall.min(6) };
        let needed_w = (*x + repeat).min(MAX_WIDTH);
        ensure_size(
            canvas, canvas_w, canvas_h, logical_w, logical_h, needed_w, needed_h,
        );
        if bits != 0 {
            for dy in 0..6usize {
                if bits & (1 << dy) == 0 {
                    continue;
                }
                let y = y0 + dy;
                if y >= *canvas_h {
                    break;
                }
                for dx in 0..repeat {
                    let px = *x + dx;
                    if px >= *canvas_w {
                        break;
                    }
                    let off = (y * *canvas_w + px) * 4;
                    canvas[off] = color.r;
                    canvas[off + 1] = color.g;
                    canvas[off + 2] = color.b;
                    canvas[off + 3] = 255;
                }
                *max_y = (*max_y).max(y + 1);
            }
            *max_x = (*max_x).max((*x + repeat).min(MAX_WIDTH));
        }
        *x += repeat;
    }

    // Prefer the raster/traversed extent when present; otherwise trim to the
    // painted extent. Logical dims, NOT the (possibly over-grown) allocation.
    let out_w = if logical_w > 0 && logical_h > 0 && max_x == 0 && max_y == 0 {
        // Nothing painted at all.
        return None;
    } else if max_x == 0 {
        logical_w
    } else {
        max_x.max(logical_w.min(MAX_WIDTH))
    };
    let out_h = if max_y == 0 {
        logical_h
    } else {
        max_y.max(logical_h.min(MAX_HEIGHT))
    };
    if out_w == 0 || out_h == 0 {
        return None;
    }

    // Crop/pad the canvas to (out_w, out_h).
    let mut rgba = vec![0u8; out_w * out_h * 4];
    for row in 0..out_h.min(canvas_h) {
        let src = row * canvas_w * 4;
        let dst = row * out_w * 4;
        let n = out_w.min(canvas_w) * 4;
        rgba[dst..dst + n].copy_from_slice(&canvas[src..src + n]);
    }

    Some(SixelImage {
        width: out_w,
        height: out_h,
        rgba,
    })
}

// ── placeholder injection ─────────────────────────────────────────────────────

/// Build the byte sequence that, fed through the ANSI parser, lays down the
/// Unicode-placeholder cells for a sixel image at the current cursor position
/// — exactly what a kitty client would print for a virtual placement.
///
/// Row movement is IND (ESC D: down, keeps scrolling like text at the bottom
/// margin) + CHA back to `start_col`, NEVER CR/LF: a CR would drop every row
/// after the first to column 0, which is exactly how yazi's right-hand
/// preview pane (images placed at a non-zero column via CUP) got shredded
/// into a staircase over the file list. The cursor ends on the line below
/// the image at its starting column (sixel scrolling-mode semantics).
pub fn placeholder_bytes(id: u32, cols: u16, rows: u16, start_col: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let (r, g, b) = ((id >> 16) & 0xff, (id >> 8) & 0xff, id & 0xff);
    // 24-bit fg carries the image id (kitty placeholder encoding).
    out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
    let mut buf = [0u8; 4];
    let cha = format!("\x1b[{}G", start_col as usize + 1);
    for row in 0..rows {
        // First cell of the row: explicit row+col diacritics; the rest
        // continue from their left neighbour (run-length form).
        out.extend_from_slice(PLACEHOLDER.encode_utf8(&mut buf).as_bytes());
        out.extend_from_slice(
            ROWCOL_DIACRITICS[row as usize % ROWCOL_DIACRITICS.len()]
                .encode_utf8(&mut buf)
                .as_bytes(),
        );
        out.extend_from_slice(ROWCOL_DIACRITICS[0].encode_utf8(&mut buf).as_bytes());
        for _ in 1..cols {
            out.extend_from_slice(PLACEHOLDER.encode_utf8(&mut buf).as_bytes());
        }
        out.extend_from_slice(b"\x1bD");
        out.extend_from_slice(cha.as_bytes());
    }
    out.extend_from_slice(b"\x1b[39m");
    out
}

/// Cursor correction appended after [`placeholder_bytes`] for a kitty CLASSIC
/// placement, whose cursor rules differ from sixel's "line below the image":
/// by default the cursor ends just right of the image on its last row; with
/// `C=1` it goes back to where it was. Both are expressed relative to where
/// `placeholder_bytes` leaves it (one row below, at `start_col`), so a
/// placement that scrolled the screen at the bottom margin still lands on
/// the right row.
pub fn placement_cursor_bytes(cols: u16, rows: u16, start_col: u16, cursor_stays: bool) -> Vec<u8> {
    if cursor_stays {
        format!("\x1b[{}A\x1b[{}G", rows.max(1), start_col as usize + 1).into_bytes()
    } else {
        format!("\x1b[1A\x1b[{}G", start_col as usize + cols as usize + 1).into_bytes()
    }
}

/// SGR bytes that restore a foreground color captured before placeholder
/// injection. `placeholder_bytes` already ends with `CSI 39 m` (default fg),
/// so only non-default foregrounds need extra bytes.
pub fn sgr_fg_bytes(fg: alacritty_terminal::vte::ansi::Color) -> Vec<u8> {
    use alacritty_terminal::vte::ansi::Color;
    match fg {
        Color::Spec(rgb) => format!("\x1b[38;2;{};{};{}m", rgb.r, rgb.g, rgb.b).into_bytes(),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m").into_bytes(),
        Color::Named(n) => {
            let i = n as usize;
            if i < 16 {
                format!("\x1b[38;5;{i}m").into_bytes()
            } else {
                Vec::new()
            }
        }
    }
}

/// Allocates synthetic image ids for sixel transmissions.
pub struct SixelIdAllocator {
    next: u32,
}

impl Default for SixelIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl SixelIdAllocator {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn next_id(&mut self) -> u32 {
        let id = SIXEL_ID_BASE + self.next;
        self.next = (self.next + 1) % SIXEL_ID_SPAN;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(bytes: &[u8]) -> Vec<SixelSequence> {
        let mut scanner = SixelScanner::new();
        let mut out = Vec::new();
        for &b in bytes {
            if let Some(seq) = scanner.advance(b) {
                out.push(seq);
            }
        }
        out
    }

    #[test]
    fn scanner_extracts_sixel_dcs() {
        let seqs = scan_all(b"before\x1bP0;1;0q#0;2;100;0;0~~\x1b\\after");
        assert_eq!(seqs.len(), 1);
        assert!(seqs[0].transparent_background);
        assert_eq!(seqs[0].data, b"#0;2;100;0;0~~");
    }

    #[test]
    fn scanner_ignores_other_dcs_and_plain_text() {
        // XTGETTCAP-style DCS and a kitty APC must not trigger.
        let seqs = scan_all(b"\x1bP+q544e\x1b\\\x1b_Ga=q;AAAA\x1b\\hello");
        assert!(seqs.is_empty());
    }

    #[test]
    fn scanner_handles_params_without_transparency() {
        let seqs = scan_all(b"\x1bP0;0;8qAB\x1b\\");
        assert_eq!(seqs.len(), 1);
        assert!(!seqs[0].transparent_background);
    }

    #[test]
    fn decode_solid_column() {
        // '~' = all six bits set; one column, red.
        let img = decode(b"#0;2;100;0;0~").unwrap();
        assert_eq!((img.width, img.height), (1, 6));
        for y in 0..6 {
            assert_eq!(&img.rgba[y * 4..y * 4 + 4], &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn decode_repeat_and_bands() {
        // Two bands: 3-wide top band via repeat, then next band one column.
        let img = decode(b"#0;2;0;100;0!3~-#0;2;0;0;100@").unwrap();
        // '@' = bit0 only → 1px tall in band 1 → height 7.
        assert_eq!((img.width, img.height), (3, 7));
        // top-left green
        assert_eq!(&img.rgba[0..4], &[0, 255, 0, 255]);
        // band 1 first row (y=6), blue
        let off = 6 * img.width * 4;
        assert_eq!(&img.rgba[off..off + 4], &[0, 0, 255, 255]);
        // untouched pixel stays transparent (y=6, x=1)
        assert_eq!(img.rgba[off + 7], 0);
    }

    #[test]
    fn decode_raster_attributes_size_canvas() {
        let img = decode(b"\"1;1;4;12~").unwrap();
        assert_eq!((img.width, img.height), (4, 12));
    }

    #[test]
    fn decode_carriage_return_overstrikes() {
        // Red column, CR, then green column overpaints the same x.
        let img = decode(b"#0;2;100;0;0~$#0;2;0;100;0~").unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(&img.rgba[0..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn decode_hls_red() {
        // DEC HLS: hue 120 = red (0 = blue), L=50, S=100.
        let img = decode(b"#0;1;120;50;100~").unwrap();
        let px = &img.rgba[0..4];
        assert!(px[0] > 200 && px[1] < 60 && px[2] < 60, "got {px:?}");
    }

    #[test]
    fn decode_empty_returns_none() {
        assert!(decode(b"").is_none());
        assert!(decode(b"$-$-").is_none());
    }

    #[test]
    fn placeholder_bytes_round_trip_shape() {
        let bytes = placeholder_bytes(SIXEL_ID_BASE + 5, 3, 2, 10);
        let s = String::from_utf8(bytes).unwrap();
        // 2 rows × (1 placeholder+2 diacritics + 2 bare placeholders)
        assert_eq!(s.matches(PLACEHOLDER).count(), 6);
        // Row movement is IND + CHA back to the start column — never CR/LF,
        // which would drop rows 2+ of a mid-screen placement to column 0
        // (the yazi preview staircase).
        assert!(!s.contains('\r'));
        assert!(!s.contains('\n'));
        assert_eq!(s.matches("\x1bD").count(), 2);
        assert_eq!(s.matches("\x1b[11G").count(), 2);
        assert!(s.starts_with("\x1b[38;2;"));
        assert!(s.ends_with("\x1b[39m"));
    }

    #[test]
    fn id_allocator_stays_in_sixel_range() {
        let mut alloc = SixelIdAllocator::new();
        let a = alloc.next_id();
        let b = alloc.next_id();
        assert_ne!(a, b);
        assert!((SIXEL_ID_BASE..SIXEL_ID_BASE + SIXEL_ID_SPAN).contains(&a));
    }

    #[test]
    fn classic_cursor_tail_moves_right_of_image_or_back_up() {
        // Default: up one row (placeholder_bytes left us one below), then
        // to the column after the image.
        assert_eq!(placement_cursor_bytes(4, 2, 10, false), b"\x1b[1A\x1b[15G");
        // C=1: back up over every image row, to the starting column.
        assert_eq!(placement_cursor_bytes(4, 2, 10, true), b"\x1b[2A\x1b[11G");
    }
}
