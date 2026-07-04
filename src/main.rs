mod ansi;
mod app;
mod image_upload;
pub mod native_ssh;
mod settings;
mod shell_window;
mod ssh;
mod tabs;
mod terminal;
mod theme;
mod window;

use app::open_shogun_window;
use gpui::Application;
use std::borrow::Cow;

static MORALERSPACE_NEON: &[u8] = include_bytes!("../assets/fonts/MoralerspaceHWNeon-Regular.ttf");
// Bundled color emoji (Twemoji Mozilla, COLRv0/CPAL — the one color-glyph
// format both DirectWrite and CoreText rasterize). Every text run points at
// it via font fallbacks, so emoji look identical on every OS instead of
// falling through to Segoe UI Emoji / Apple Color Emoji. See CREDITS.
static TWEMOJI_MOZILLA: &[u8] = include_bytes!("../assets/fonts/Twemoji.Mozilla.ttf");

#[cfg(target_os = "windows")]
const SYSTEM_FONT_DIRS: &[&str] = &[
    r"C:\Windows\Fonts",
    r"C:\Users\Public\AppData\Local\Microsoft\Windows\Fonts",
];

#[cfg(target_os = "macos")]
const SYSTEM_FONT_DIRS: &[&str] = &["/Library/Fonts", "/System/Library/Fonts"];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const SYSTEM_FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// フォントファミリー名からシステムフォントを検索してロードする。
/// `.ttf` → `.ttc` → `.otc` の順で試みる。
/// 見つからなければ `None` を返す（GPUI の fallback に任せる）。
fn load_system_font(family: &str) -> Option<Vec<u8>> {
    let stems: &[String] = &[
        format!("{}-Regular.ttf", family),
        format!("{}Regular.ttf", family),
        format!("{}.ttf", family),
        format!("{}.ttc", family), // TrueType Collection (e.g. msgothic.ttc)
        format!("{}.otc", family), // OpenType Collection
    ];
    for dir in SYSTEM_FONT_DIRS {
        for stem in stems {
            let path = std::path::Path::new(dir).join(stem);
            if let Ok(data) = std::fs::read(&path) {
                return Some(data);
            }
        }
    }
    None
}

/// Register the bundled emoji font with the Windows *session* font table
/// before gpui builds its DirectWrite system font collection.
///
/// gpui 0.2.2's `generate_font_fallbacks` resolves user fallback families
/// against the SYSTEM font collection only (direct_write.rs:333) — fonts
/// embedded via `add_fonts` land in the CUSTOM collection and are silently
/// skipped ("No matching font found"), so emoji fell through to Segoe UI
/// Emoji. `AddFontResourceExW` without FR_PRIVATE makes the font visible in
/// the system collection for this login session (no install, refcounted,
/// gone after reboot). Upstream fix would be searching the custom
/// collection too.
#[cfg(target_os = "windows")]
fn register_session_emoji_font() {
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn AddFontResourceExW(name: *const u16, fl: u32, res: *mut core::ffi::c_void) -> i32;
    }

    let dir = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("shogun-desktop")
        .join("fonts");
    let path = dir.join("Twemoji.Mozilla.ttf");
    let up_to_date = std::fs::metadata(&path)
        .map(|m| m.len() == TWEMOJI_MOZILLA.len() as u64)
        .unwrap_or(false);
    if !up_to_date {
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        if std::fs::write(&path, TWEMOJI_MOZILLA).is_err() {
            return;
        }
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let added = unsafe { AddFontResourceExW(wide.as_ptr(), 0, std::ptr::null_mut()) };
    if added == 0 {
        eprintln!("emoji font session registration failed: {}", path.display());
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    register_session_emoji_font();

    Application::new().run(|cx| {
        let mut fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(MORALERSPACE_NEON),
            Cow::Borrowed(TWEMOJI_MOZILLA),
        ];

        // システムフォントを動的ロード:
        //   Cica → ユーザーが設定タブで選択した場合の CJK カバレッジ補完用
        // MS Gothic は削除: EAW=A (→ ◆ ▶ など) は alacritty_terminal が
        // display_width=1 (narrow) で返すため、MS Gothic の全角グリフを当てると
        // 1-cell コンテナをはみ出して表示が壊れる。
        // wt も EAW=A を narrow として扱う (PR #2928 / wcwidth() de facto standard)。
        for stem in &["Cica"] {
            if let Some(data) = load_system_font(stem) {
                fonts.push(Cow::Owned(data));
            }
        }

        cx.text_system()
            .add_fonts(fonts)
            .expect("Failed to load fonts");
        gpui_component::init(cx);
        // Reclaim tab / shift-tab for the terminal. gpui dispatches action
        // bindings BEFORE key listeners, so gpui_component's Root bindings
        // (tab → focus_next) would otherwise consume Tab and the terminal's
        // capture_key_down would never see it. These bindings target the
        // deeper "ShogunTerminal" key context, so they win while the terminal
        // is focused and Root's focus-cycling still works elsewhere.
        cx.bind_keys([
            gpui::KeyBinding::new(
                "tab",
                window::TerminalSendTab,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "shift-tab",
                window::TerminalSendBacktab,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            // Copy the mouse selection. Plain ctrl-c must keep sending SIGINT
            // to the PTY, so copy lives on the terminal-conventional
            // ctrl-shift-c (and cmd-c on macOS).
            gpui::KeyBinding::new(
                "ctrl-shift-c",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "cmd-c",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            // Paste (bracketed when the app enabled ?2004). Plain ctrl-v must
            // keep sending ^V (shell literal-next), so paste mirrors copy on
            // ctrl-shift-v (and cmd-v on macOS).
            gpui::KeyBinding::new(
                "ctrl-shift-v",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "cmd-v",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            // Classic terminal copy/paste keys (Windows Terminal binds them
            // too). Also the escape hatch when a resident app squats
            // ctrl-shift-c as a global hotkey: RegisterHotKey interception
            // happens before this app ever sees WM_KEYDOWN, so no in-app
            // binding can win (observed 2026-07-04).
            gpui::KeyBinding::new(
                "ctrl-insert",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "shift-insert",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
        ]);
        open_shogun_window(cx);
    });
}
