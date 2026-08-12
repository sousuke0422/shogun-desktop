//! Headless-ish diagnostic: which font does an emoji glyph resolve to?
//!
//! Usage:  fontprobe [--gdi]
//!   --gdi  register the bundled Twemoji with AddFontResourceExW first
//!          (mirrors register_session_emoji_font in main.rs)
//!
//! Opens a tiny window for one frame (shape_line needs WindowTextSystem),
//! shapes "A😀" under several base fonts, prints the resolved font family
//! of every run, then exits.

use gpui::prelude::*;
use gpui::{
    App, Application, Bounds, Context, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight,
    Render, TextRun, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use std::borrow::Cow;

static TWEMOJI: &[u8] = include_bytes!("../../assets/fonts/Twemoji.ttf");
static MORALERSPACE_NEON: &[u8] =
    include_bytes!("../../assets/fonts/MoralerspaceHWNeon-Regular.ttf");

#[cfg(target_os = "windows")]
fn register_session_emoji_font() {
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn AddFontResourceExW(name: *const u16, fl: u32, res: *mut core::ffi::c_void) -> i32;
    }
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap()
        .join("shogun-desktop")
        .join("fonts");
    let path = dir.join("Twemoji.ttf");
    std::fs::create_dir_all(&dir).unwrap();
    // The file may be locked by the session font table (registered by an
    // earlier process); the bytes are identical, so just reuse it.
    let same = std::fs::metadata(&path)
        .map(|m| m.len() == TWEMOJI.len() as u64)
        .unwrap_or(false);
    if !same {
        std::fs::write(&path, TWEMOJI).unwrap();
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let added = unsafe { AddFontResourceExW(wide.as_ptr(), 0, std::ptr::null_mut()) };
    println!("AddFontResourceExW -> {added}");
}

fn font(family: &str, with_fallback: bool) -> Font {
    Font {
        family: family.to_string().into(),
        features: FontFeatures::default(),
        fallbacks: with_fallback.then(|| FontFallbacks::from_fonts(vec!["Twemoji".to_string()])),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

struct Probe;

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ts = window.text_system();
        // Resolve the candidate emoji fonts up front so their FontIds are
        // known; a fallback-resolved run reusing the same face gets the same
        // id via font_id_by_identifier.
        let id_twemoji = cx.text_system().resolve_font(&font("Twemoji", false));
        let id_segoe_emoji = cx
            .text_system()
            .resolve_font(&font("Segoe UI Emoji", false));
        println!("candidates: twemoji={id_twemoji:?} segoe-emoji={id_segoe_emoji:?}");
        let text: gpui::SharedString = "A😀".into();
        for (label, base) in [
            ("mono+fallback", font("Moralerspace Neon HW", true)),
            ("segoe+fallback", font("Segoe UI", true)),
            ("mono no-fallback", font("Moralerspace Neon HW", false)),
        ] {
            let runs = [TextRun {
                len: text.len(),
                font: base,
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            let line = ts.shape_line(text.clone(), px(16.), &runs, None);
            let resolved: Vec<String> = line
                .runs
                .iter()
                .map(|r| {
                    let fam = cx
                        .text_system()
                        .get_font_for_id(r.font_id)
                        .map(|f| f.family.to_string())
                        .unwrap_or_else(|| "?".into());
                    format!("{:?}={fam}(glyphs={})", r.font_id, r.glyphs.len())
                })
                .collect();
            println!("{label}: {resolved:?}");
        }
        std::process::exit(0);
        #[allow(unreachable_code)]
        div()
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    if std::env::args().any(|a| a == "--gdi") {
        register_session_emoji_font();
    }

    Application::new().run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(MORALERSPACE_NEON),
                Cow::Borrowed(TWEMOJI),
            ])
            .unwrap();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(120.), px(60.)),
                })),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Probe),
        )
        .unwrap();
    });
}
