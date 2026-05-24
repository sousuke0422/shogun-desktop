use crate::ansi::strip_ansi;
use crate::theme::Colors;
use gpui::{div, App, IntoElement, ParentElement, Styled, Window};

pub fn render_shogun_tab(_window: &mut Window, _cx: &mut App) -> impl IntoElement {
    let sample = "\x1b[31m将軍\x1b[0m terminal";
    let plain = strip_ansi(sample);
    div()
        .flex_1()
        .p_4()
        .bg(Colors::shikkoku())
        .text_color(Colors::zouge())
        .child("将軍タブ — tmux pane 表示は Phase 2 で実装予定")
        .child(format!("AnsiStripper 動作確認: {plain}"))
}
