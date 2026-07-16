//! Process-global color palette — same lifecycle as [`crate::typography`]:
//! the embedder sets it once at startup and the renderer/color resolver read
//! it per frame, so a theme applies to every window and tab without threading
//! a parameter through the render path.
//!
//! `None` = the built-in defaults (today's hardcoded shikkoku/zouge + Tango
//! ANSI values), so an embedder that never calls [`set_palette`] behaves
//! exactly as before this module existed.

use parking_lot::RwLock;

/// An 8-bit-per-channel color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    const fn hex(v: u32) -> Self {
        Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn to_tuple(self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}

/// A full terminal palette: window background + default text, the 16 ANSI
/// colors (index 0..15 = black..white, brightBlack..brightWhite), and the
/// selection highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

/// The built-in palette: the values that were hardcoded across the engine
/// before theming (pane #1A1A1A / text #E8DCC8; Tango ANSI; selection the
/// cursor-line blue). `theme::*` accessors return these when no override is
/// set, so the default look is unchanged.
pub const DEFAULT: Palette = Palette {
    background: Rgb::hex(0x1A1A1A),
    foreground: Rgb::hex(0xE8DCC8),
    // The selection highlight's RGB; the renderer paints it translucent
    // (steel blue over the ink so selected text stays legible).
    selection: Rgb::hex(0x3465A4),
    ansi: [
        Rgb::hex(0x1E1E1E), // black
        Rgb::hex(0xCC0000), // red
        Rgb::hex(0x4E9A06), // green
        Rgb::hex(0xC4A000), // yellow
        Rgb::hex(0x3465A4), // blue
        Rgb::hex(0x75507B), // magenta
        Rgb::hex(0x06989A), // cyan
        Rgb::hex(0xD3D7CF), // white
        Rgb::hex(0x555753), // bright black
        Rgb::hex(0xEF2929), // bright red
        Rgb::hex(0x8AE234), // bright green
        Rgb::hex(0xFCE94F), // bright yellow
        Rgb::hex(0x729FCF), // bright blue
        Rgb::hex(0xAD7FA8), // bright magenta
        Rgb::hex(0x34E2E2), // bright cyan
        Rgb::hex(0xEEEEEC), // bright white
    ],
};

static PALETTE: RwLock<Option<Palette>> = RwLock::new(None);

/// Install a palette process-wide (startup, from config). Takes effect on the
/// next frame.
pub fn set_palette(p: Palette) {
    *PALETTE.write() = Some(p);
}

/// The active palette (override, or the built-in default).
pub fn palette() -> Palette {
    PALETTE.read().clone().unwrap_or(DEFAULT)
}

/// Window/pane background — the fallback for cells with no explicit bg.
pub fn background() -> Rgb {
    PALETTE
        .read()
        .as_ref()
        .map_or(DEFAULT.background, |p| p.background)
}

/// Default text color — the fallback for cells with no explicit fg.
pub fn foreground() -> Rgb {
    PALETTE
        .read()
        .as_ref()
        .map_or(DEFAULT.foreground, |p| p.foreground)
}

/// Selection highlight background.
pub fn selection() -> Rgb {
    PALETTE
        .read()
        .as_ref()
        .map_or(DEFAULT.selection, |p| p.selection)
}

/// One of the 16 ANSI colors (`idx` 0..15; out of range clamps to 15).
pub fn ansi(idx: u8) -> Rgb {
    let i = (idx as usize).min(15);
    PALETTE
        .read()
        .as_ref()
        .map_or(DEFAULT.ansi[i], |p| p.ansi[i])
}
