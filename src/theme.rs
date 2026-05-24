use gpui::{rgb, Rgba};

/// Shogun design system — from android/.interface-design/system.md
pub struct Colors;

impl Colors {
    /// #1A1A1A — Lacquered armor base (screen background)
    pub fn shikkoku() -> Rgba { rgb(0x1A1A1A) }
    /// #2D2D2D — Ink stone, castle wall (cards, pane tiles)
    pub fn sumi() -> Rgba { rgb(0x2D2D2D) }
    /// #363636 — Dropdowns, dialogs
    #[allow(dead_code)]
    pub fn raised() -> Rgba { rgb(0x363636) }
    /// #C9A94E — Gold leaf (primary text, accents, headings)
    pub fn kinpaku() -> Rgba { rgb(0xC9A94E) }
    /// #E8DCC8 — Washi paper, scroll (body text)
    pub fn zouge() -> Rgba { rgb(0xE8DCC8) }
    /// #B33B24 — Vermilion torii (action, CTA, destructive)
    #[allow(dead_code)]
    pub fn shuaka() -> Rgba { rgb(0xB33B24) }
    /// #3C6E47 — Pine garden (success, connected)
    pub fn matsuba() -> Rgba { rgb(0x3C6E47) }
    /// #3A4A5C — Iron armor plate (secondary, metadata)
    #[allow(dead_code)]
    pub fn tetsukon() -> Rgba { rgb(0x3A4A5C) }
    /// #CC3333 — Blood red (error, disconnected)
    pub fn kurenai() -> Rgba { rgb(0xCC3333) }
    /// #666666 — Disabled, placeholders
    pub fn muted() -> Rgba { rgb(0x666666) }
    /// Gold border at 20% opacity
    pub fn border() -> Rgba { rgba(0xC9A94E33) }
    /// Gold border at 40% opacity (emphasis)
    #[allow(dead_code)]
    pub fn border_emphasis() -> Rgba { rgba(0xC9A94E66) }
}

fn rgba(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xFF) as f32 / 255.0,
        g: ((hex >> 16) & 0xFF) as f32 / 255.0,
        b: ((hex >> 8) & 0xFF) as f32 / 255.0,
        a: (hex & 0xFF) as f32 / 255.0,
    }
}
