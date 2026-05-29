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
    Application::new().run(|cx| {
        let mut fonts: Vec<Cow<'static, [u8]>> = vec![Cow::Borrowed(MORALERSPACE_NEON)];

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
        open_shogun_window(cx);
    });
}
