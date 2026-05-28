use crate::terminal::renderer::render_grid;
use crate::terminal::GridSnapshot;
use crate::theme::Colors;
use crate::window::ShogunWindow;
use gpui::{
    div, prelude::*, px, Context, IntoElement, KeyDownEvent, ParentElement, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement, Styled,
};
use gpui_component::v_flex;

const SCROLL_OFFSET_EPSILON: f32 = 0.01;

fn scroll_delta_y(event: &ScrollWheelEvent) -> f32 {
    match &event.delta {
        ScrollDelta::Pixels(p) => p.y / px(1.),
        ScrollDelta::Lines(l) => l.y,
    }
}

pub fn render_terminal_tab(
    snap: &GridSnapshot,
    scroll_handle: &ScrollHandle,
    is_shogun: bool,
    font: &str,
    // Cell width in logical pixels — measured via `TextSystem::ch_advance`.
    cw: f32,
    // Cell height in logical pixels — measured via `ascent + descent`.
    ch: f32,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let scroll_handle = scroll_handle.clone();
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .id(if is_shogun {
                    "terminal-pane-shogun"
                } else {
                    "terminal-pane-multiagent"
                })
                .flex_1()
                .w_full()
                .track_scroll(&scroll_handle)
                .overflow_y_scroll()
                .tab_stop(true)
                .on_key_down(cx.listener(
                    |this, event: &KeyDownEvent, _window, cx| {
                        this.handle_terminal_key(event, cx);
                    },
                ))
                .on_scroll_wheel(cx.listener(
                    move |this, event: &ScrollWheelEvent, _window, cx| {
                        let delta_y = scroll_delta_y(event);
                        if delta_y < 0.0 {
                            if is_shogun {
                                this.shogun_scroll_locked = true;
                            } else {
                                this.multiagent_scroll_locked = true;
                            }
                        } else if delta_y > 0.0 {
                            let cur_y = scroll_handle.offset().y / px(1.);
                            let prev_y = if is_shogun {
                                this.shogun_prev_offset_y
                            } else {
                                this.multiagent_prev_offset_y
                            };
                            if (cur_y - prev_y).abs() < SCROLL_OFFSET_EPSILON {
                                if is_shogun {
                                    this.shogun_scroll_locked = false;
                                } else {
                                    this.multiagent_scroll_locked = false;
                                }
                            }
                        }
                        cx.notify();
                    },
                ))
                .p_1()
                .child(render_grid(snap, font, cw, ch)),
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
