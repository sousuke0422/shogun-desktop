mod ansi;
mod app;
mod native_ssh;
mod settings;
mod ssh;
mod tabs;
mod terminal;
mod theme;
mod window;

use app::open_shogun_window;
use gpui::Application;

fn main() {
    Application::new().run(|cx| {
        gpui_component::init(cx);
        open_shogun_window(cx);
    });
}
