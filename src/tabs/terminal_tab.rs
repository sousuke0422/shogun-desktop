use crate::terminal::renderer::render_grid;
use crate::terminal::GridSnapshot;
use crate::theme::Colors;
use crate::window::ShogunWindow;
use gpui::{div, prelude::*, px, Context, IntoElement, ParentElement, ScrollHandle, Styled};
use gpui_component::v_flex;

pub fn render_terminal_tab(
    snap: &GridSnapshot,
    scroll_handle: &ScrollHandle,
    _cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .id("terminal-pane")
                .flex_1()
                .w_full()
                .track_scroll(scroll_handle)
                .overflow_y_scroll()
                .p_1()
                .child(render_grid(snap)),
        )
}

pub fn render_terminal_tab_disconnected(
    reconnect_btn: impl IntoElement,
    _cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .text_color(Colors::kurenai())
                .child("SSH接続が切れました"),
        )
        .child(div().p_2().child(reconnect_btn))
}

pub fn render_terminal_tab_error(msg: String, _cx: &mut Context<ShogunWindow>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .text_color(Colors::kurenai())
                .text_size(px(14.))
                .child(msg),
        )
}

pub fn render_terminal_tab_empty(_cx: &mut Context<ShogunWindow>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .text_color(Colors::muted())
                .child("接続中..."),
        )
}
