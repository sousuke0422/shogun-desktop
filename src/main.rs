mod ansi;
mod app;
pub mod native_ssh;
mod settings;
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

fn main() {
    Application::new().run(|cx| {
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(MORALERSPACE_NEON)])
            .expect("Failed to load MoralerspaceHW Neon font");
        gpui_component::init(cx);
        open_shogun_window(cx);
    });
}
