mod ansi;
mod app;
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

static MORALERSPACE_NEON: &[u8] =
    include_bytes!("../assets/fonts/MoralerspaceHWNeon-Regular.ttf");

/// Windows システムフォントディレクトリの候補（インストール先によって異なる）。
const SYSTEM_FONT_DIRS: &[&str] = &[
    r"C:\Windows\Fonts",
    r"C:\Users\Public\AppData\Local\Microsoft\Windows\Fonts",
];

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
        //   msgothic → "MS Gothic" ファミリー (EAW=A 全角対応の標準日本語端末フォント)
        //   Cica     → CJK カバレッジ補完用
        // msgothic.ttc が優先: → ◆ ─ │ あ など全角グリフが揃っている
        for stem in &["msgothic", "Cica"] {
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
