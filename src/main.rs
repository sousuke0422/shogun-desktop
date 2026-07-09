// Release builds are GUI-subsystem: no console window tags along (and
// closing it can no longer kill the app with it). Debug builds keep the
// console for printf-style work; diagnostics in release go to the panic log
// and SHOGUN_*_LOG files.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ansi;
mod app;
mod image_upload;
pub mod native_ssh;
mod notify_toast;
mod pty_spawn;
mod settings;
mod shell_integration;
mod shell_window;
mod ssh;
mod tabs;
mod taskbar_progress;
mod tsf;
// Terminal engine extracted to the rikka-terminal-core workspace crate; keep
// the old `crate::terminal::` paths alive via a root re-export.
pub use rikka_terminal_core as terminal;
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

fn main() {
    // Panics land in %TEMP%/shogun-tsf/panic.log — field diagnosis for the
    // "grid goes permanently black" class (a dead parse thread is invisible
    // from a GUI shell).
    rikka_terminal_core::install_panic_log();
    // gpui reports recoverable errors via log::error (log_err) — without a
    // logger they vanish. warn+ goes to %TEMP%/shogun-tsf/shogun-desktop.log.
    rikka_terminal_core::install_file_logger("shogun-desktop");
    // OpenType features apply engine-globally; set them before the first
    // frame (saving settings re-applies at runtime).
    rikka_terminal_core::renderer::set_font_features(settings::parse_font_features(
        &settings::load_settings()
            .unwrap_or_default()
            .terminal
            .font_features,
    ));
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
        // The app is a fixed dark palette (crate::theme::Colors); force the
        // component theme to match instead of following the OS appearance.
        // Otherwise on a light-mode OS the inputs/radios/switches render
        // light on first paint until something re-syncs the theme.
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
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
            // Copy / paste bindings. NOTE: later bindings take precedence in
            // gpui, and UI affordances (the right-click menu) display the
            // highest-precedence chord — so the conventional ctrl-shift-c /
            // ctrl-shift-v must be registered LAST.
            //
            // ctrl-insert / shift-insert: classic terminal keys (Windows
            // Terminal binds them too), and the escape hatch when a resident
            // app squats ctrl-shift-c as a RegisterHotKey global hotkey —
            // interception happens before this app ever sees WM_KEYDOWN, so
            // no in-app binding can win (observed 2026-07-04).
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
            // macOS chords.
            gpui::KeyBinding::new(
                "cmd-c",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "cmd-v",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            // Primary chords (displayed in menus). Plain ctrl-c must keep
            // sending SIGINT and plain ctrl-v must keep sending ^V (shell
            // literal-next), so copy/paste live on the shifted variants.
            gpui::KeyBinding::new(
                "ctrl-shift-c",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "ctrl-shift-v",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
        ]);
        // On macOS the platform-native cmd chords are the ones menus should
        // display — re-register them after everything else so they take the
        // highest precedence there.
        #[cfg(target_os = "macos")]
        cx.bind_keys([
            gpui::KeyBinding::new(
                "cmd-c",
                window::TerminalCopy,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
            gpui::KeyBinding::new(
                "cmd-v",
                window::TerminalPaste,
                Some(window::TERMINAL_KEY_CONTEXT),
            ),
        ]);
        open_shogun_window(cx);
    });
}
