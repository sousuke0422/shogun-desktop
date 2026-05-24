use crate::theme::Colors;
use gpui::{div, App, IntoElement, ParentElement, Styled, Window};

pub fn render_agents_tab(_window: &mut Window, _cx: &mut App) -> impl IntoElement {
    div()
        .flex_1()
        .p_4()
        .bg(Colors::shikkoku())
        .text_color(Colors::zouge())
        .child("エージェントタブ — 布陣一覧は Phase 2 で実装予定")
}
