//! HarfBuzz-semantics shaping + COLR rasterisation for ZWJ emoji clusters.
//!
//! The platform shaper (DirectWrite, via gpui) kills the ZWJ in VS16-led
//! sequences before GSUB runs — ❤️‍🔥 comes back as `[heart, .notdef, fire]`
//! (measured with `RIKKA_DEBUG_CLUSTER`), so the bundled font's ccmp
//! ligature can never match, even though the font demonstrably has it
//! (glyph 13669, components `[VS16, ZWJ, fire]`). rustybuzz keeps variation
//! selectors alive as glyphs through GSUB, exactly like HarfBuzz, so the
//! same font ligates the same sequence correctly here.
//!
//! Scope is deliberately narrow: ONLY ZWJ emoji clusters take this path,
//! and only when shaping collapses the whole cluster into a single ligature
//! glyph. Everything else — single emoji, combining marks, IVS, text —
//! stays on the DirectWrite path, which handles them correctly and shares
//! its glyph atlas with the rest of the grid.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::RenderImage;
use image::{Frame, RgbaImage};

/// The bundled colour emoji font (same file the DirectWrite fallback uses).
static FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rikka-terminal/assets/fonts/Twemoji.ttf"
));

/// A cluster spelling the ligature path handles: a ZWJ sequence with a
/// pictograph, an emoji-presentation keycap (VS16 + U+20E3), or a tag
/// sequence (subdivision flags). Letters joined by ZWJ (Arabic/Indic
/// joining control) must not come here.
pub(crate) fn is_emoji_ligature_cluster(text: &str) -> bool {
    let pictographic = text
        .chars()
        .any(|c| matches!(c as u32, 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x1F000..=0x1FAFF));
    let zwj = text.contains('\u{200D}') && pictographic;
    let keycap = text.contains('\u{FE0F}') && text.contains('\u{20E3}');
    let tag_flag = pictographic && text.chars().any(|c| matches!(c as u32, 0xE0020..=0xE007F));
    zwj || keycap || tag_flag
}

/// Shape `text` with rustybuzz against the bundled font. `Some(gids)` iff
/// every glyph resolved (no .notdef anywhere).
/// The parsed shaping face, built once — `FONT_BYTES` is `'static`, and a
/// coverage sweep (or a busy grid) would otherwise re-parse the whole font
/// per cluster.
fn face() -> Option<&'static rustybuzz::Face<'static>> {
    static FACE: OnceLock<Option<rustybuzz::Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| rustybuzz::Face::from_slice(FONT_BYTES, 0))
        .as_ref()
}

pub(crate) fn shape_cluster_gids(text: &str) -> Option<Vec<u32>> {
    let face = face()?;
    let shape = |t: &str| -> Option<Vec<u32>> {
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(t);
        let shaped = rustybuzz::shape(face, &[], buf);
        let gids: Vec<u32> = shaped.glyph_infos().iter().map(|i| i.glyph_id).collect();
        (!gids.is_empty() && !gids.contains(&0)).then_some(gids)
    };
    let first = shape(text);
    // nanoemoji derives ligatures from asset file names, and Twemoji names
    // keycaps (and some others) WITHOUT the VS16 — `31-20e3.svg` — so the
    // canonical `1 FE0F 20E3` stream misses the ligature. If the plain
    // shape didn't collapse, retry with every VS16 stripped and keep the
    // shorter result.
    if first.as_ref().is_none_or(|g| g.len() > 1) && text.contains('\u{FE0F}') {
        let stripped: String = text.chars().filter(|&c| c != '\u{FE0F}').collect();
        if let Some(g2) = shape(&stripped)
            && first.as_ref().is_none_or(|g1| g2.len() < g1.len())
        {
            return Some(g2);
        }
    }
    first
}

/// Rasterised single-ligature cluster: the image plus its pixel size, cached
/// by (cluster text, pixel size). `None` (also cached) means "no single
/// ligature for this cluster at all" — the caller falls back to the
/// DirectWrite path and its budget clip.
pub(crate) fn cluster_image(text: &str, cell_h_px: f32) -> Option<(Arc<RenderImage>, u32, u32)> {
    let px = cell_h_px.round().max(4.0) as u32;
    type CacheMap = HashMap<(String, u32), Option<(Arc<RenderImage>, u32, u32)>>;
    static CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (text.to_string(), px);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let computed = render_ligature(text, px as f32);
    cache.lock().unwrap().insert(key, computed.clone());
    computed
}

fn render_ligature(text: &str, px: f32) -> Option<(Arc<RenderImage>, u32, u32)> {
    // Only a full collapse into ONE glyph is worth leaving the text path
    // for; multi-glyph results mean the font has no ligature and the
    // DirectWrite fragments are the honest rendering.
    let gids = shape_cluster_gids(text)?;
    let &[gid] = gids.as_slice() else {
        return None;
    };

    let font = swash::FontRef::from_index(FONT_BYTES, 0)?;
    let mut ctx = swash::scale::ScaleContext::new();
    let mut scaler = ctx.builder(font).size(px).hint(false).build();
    use swash::scale::{Render, Source, StrikeWith};
    let img = Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ])
    .render(&mut scaler, gid as u16)?;
    if !matches!(img.content, swash::scale::image::Content::Color) {
        return None; // monochrome mask — let the text path colour it instead
    }
    let (w, h) = (img.placement.width, img.placement.height);
    if w == 0 || h == 0 {
        return None;
    }
    let mut data = img.data;
    // RGBA → BGRA, matching gpui's atlas (same swizzle as kitty graphics).
    for p in data.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let rgba = RgbaImage::from_raw(w, h, data)?;
    let image = Arc::new(RenderImage::new(vec![Frame::new(rgba)]));
    Some((image, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Glyph ids are pinned to the bundled Twemoji build (jdecked v17.0.3,
    // nanoemoji glyf_colr_0) — update them (from a RIKKA_DEBUG_CLUSTER dump
    // or a GSUB parse) if the font asset is ever swapped.

    #[test]
    fn heart_on_fire_ligates_to_one_glyph() {
        let gids = shape_cluster_gids("\u{2764}\u{FE0F}\u{200D}\u{1F525}").unwrap();
        assert_eq!(gids, vec![4014]);
    }

    #[test]
    fn zwj_family_ligates_to_one_glyph() {
        let gids = shape_cluster_gids("👨\u{200D}👩\u{200D}👧").unwrap();
        assert_eq!(gids, vec![1192]);
    }

    #[test]
    fn letters_with_zwj_are_not_emoji_clusters() {
        assert!(!is_emoji_ligature_cluster("a\u{200D}b"));
        assert!(is_emoji_ligature_cluster(
            "\u{2764}\u{FE0F}\u{200D}\u{1F525}"
        ));
        assert!(is_emoji_ligature_cluster("1\u{FE0F}\u{20E3}"));
        assert!(is_emoji_ligature_cluster(
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}"
        ));
    }

    #[test]
    fn keycap_ligates_to_one_glyph() {
        let gids = shape_cluster_gids("1\u{FE0F}\u{20E3}").unwrap();
        assert_eq!(gids.len(), 1, "keycap should collapse: {gids:?}");
        assert_ne!(gids[0], 0);
        // Same result for the VS16-less spelling some emitters use.
        assert_eq!(shape_cluster_gids("1\u{20E3}").unwrap().len(), 1);
    }

    #[test]
    fn tag_flag_ligates_to_one_glyph() {
        let gids =
            shape_cluster_gids("\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}")
                .unwrap();
        assert_eq!(gids.len(), 1, "tag flag should collapse: {gids:?}");
        assert_ne!(gids[0], 0);
    }

    /// Every fully-qualified RGI sequence from Unicode's emoji-test.txt
    /// must resolve in the bundled font: no .notdef anywhere, and every
    /// multi-codepoint sequence must collapse to a single glyph (via the
    /// ligature tables or the VS16-strip retry). This is the net that
    /// catches a font swap losing coverage — the keycap miss that needed
    /// the VS16 retry would have been caught here on day one.
    ///
    /// The fixture is Emoji 16.0 (17.0's file was not yet published at the
    /// pinned URL); the bundled font is an Emoji 17 build, so this asserts
    /// a lower bound. Refresh testdata/emoji-test.txt when Unicode ships it.
    #[test]
    fn rgi_coverage_is_total() {
        let data = include_str!("../testdata/emoji-test.txt");
        let mut total = 0u32;
        let mut failures: Vec<String> = Vec::new();
        for line in data.lines() {
            let Some((seq, rest)) = line.split_once(';') else {
                continue;
            };
            if !rest.trim_start().starts_with("fully-qualified") {
                continue;
            }
            let text: String = seq
                .trim()
                .split_whitespace()
                .map(|h| char::from_u32(u32::from_str_radix(h, 16).unwrap()).unwrap())
                .collect();
            total += 1;
            match shape_cluster_gids(&text) {
                Some(gids) if gids.len() == 1 => {}
                got => failures.push(format!("{text} {:X?} -> {got:?}", text.chars())),
            }
        }
        assert!(
            total > 3700,
            "fixture parsed suspiciously few sequences: {total}"
        );
        assert!(
            failures.is_empty(),
            "{} of {total} RGI sequences unresolved:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn ligature_rasterises_in_colour() {
        let (_img, w, h) = cluster_image("\u{2764}\u{FE0F}\u{200D}\u{1F525}", 28.0).unwrap();
        assert!(w > 0 && h > 0);
    }
}
