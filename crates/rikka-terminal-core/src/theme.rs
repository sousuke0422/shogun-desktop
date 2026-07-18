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

impl Palette {
    /// Wire form for a tab move: `[background, foreground, selection,
    /// ansi0..ansi15]` as `0xRRGGBB` — 19 plain integers, so the IPC layer
    /// needs no knowledge of this type.
    pub fn to_wire(&self) -> Vec<u32> {
        let pack = |c: Rgb| ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        let mut v = Vec::with_capacity(19);
        v.push(pack(self.background));
        v.push(pack(self.foreground));
        v.push(pack(self.selection));
        v.extend(self.ansi.iter().map(|&c| pack(c)));
        v
    }

    /// Inverse of [`Self::to_wire`]. `None` when the payload isn't the
    /// expected 19 entries (unknown/foreign sender — fail open, no theme).
    pub fn from_wire(v: &[u32]) -> Option<Self> {
        if v.len() != 19 {
            return None;
        }
        let mut ansi = [Rgb::new(0, 0, 0); 16];
        for (slot, &raw) in ansi.iter_mut().zip(&v[3..19]) {
            *slot = Rgb::hex(raw);
        }
        Some(Palette {
            background: Rgb::hex(v[0]),
            foreground: Rgb::hex(v[1]),
            selection: Rgb::hex(v[2]),
            ansi,
        })
    }
}

static PALETTE: RwLock<Option<Palette>> = RwLock::new(None);

/// Install a palette process-wide. Takes effect on the next frame. The
/// embedder swaps this per active tab (rikka renders only the active tab, so
/// one global palette suffices for per-tab theming).
pub fn set_palette(p: Palette) {
    *PALETTE.write() = Some(p);
}

/// Drop any override, reverting to the built-in [`DEFAULT`] — the active tab
/// carries no theme and no global one is configured.
pub fn clear_palette() {
    *PALETTE.write() = None;
}

/// Whether a palette override is currently installed (vs. the built-in
/// default). The embedder keys the pane surround color on this so an
/// unthemed session keeps the chrome's own fill.
pub fn is_overridden() -> bool {
    PALETTE.read().is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The primitive the per-tab theming rests on: installing a palette makes
    /// the accessors and `is_overridden` follow it, and clearing reverts to
    /// the built-in default. (Serialized: one global, shared across tests.)
    #[test]
    fn set_and_clear_swap_the_active_palette() {
        let mut ubuntu = DEFAULT;
        ubuntu.background = Rgb::hex(0x300A24);
        ubuntu.ansi[1] = Rgb::hex(0xAABBCC);

        clear_palette();
        assert!(!is_overridden());
        assert_eq!(background(), DEFAULT.background);

        set_palette(ubuntu.clone());
        assert!(is_overridden());
        assert_eq!(background(), Rgb::hex(0x300A24));
        assert_eq!(ansi(1), Rgb::hex(0xAABBCC));

        // Swapping back to the config/base (here: default) reverts every read.
        clear_palette();
        assert!(!is_overridden());
        assert_eq!(background(), DEFAULT.background);
        assert_eq!(ansi(1), DEFAULT.ansi[1]);
    }

    /// The tab-move wire form: 19 packed 0xRRGGBB values, lossless both ways;
    /// a malformed length fails open (no theme).
    #[test]
    fn palette_wire_roundtrip() {
        let mut p = DEFAULT;
        p.background = Rgb::new(0x30, 0x0A, 0x24);
        p.ansi[15] = Rgb::new(0x01, 0x02, 0x03);
        let wire = p.to_wire();
        assert_eq!(wire.len(), 19);
        assert_eq!(wire[0], 0x300A24);
        assert_eq!(wire[18], 0x010203);
        assert_eq!(Palette::from_wire(&wire), Some(p));
        assert_eq!(Palette::from_wire(&wire[..18]), None);
        assert_eq!(Palette::from_wire(&[]), None);
    }
}
