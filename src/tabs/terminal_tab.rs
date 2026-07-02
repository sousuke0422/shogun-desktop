use crate::terminal::GridSnapshot;
use crate::terminal::renderer::render_grid;
use crate::theme::Colors;
use crate::window::{
    ShogunWindow, TERMINAL_KEY_CONTEXT, TerminalCopy, TerminalSendBacktab, TerminalSendTab,
};
use gpui::{
    App, Context, DispatchPhase, ElementInputHandler, FocusHandle, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    ScrollDelta, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement, Styled, TextRun,
    UnderlineStyle, canvas, div, point, prelude::*, px, rgba,
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
    focus_handle: &FocusHandle,
    // IME composition (preedit) text, drawn inline at the terminal cursor.
    ime_preedit: Option<String>,
    // Normalized inclusive (start, end) cell range of the mouse selection.
    selection: Option<((usize, usize), (usize, usize))>,
    is_shogun: bool,
    font: &str,
    // Cell width in logical pixels — measured via `TextSystem::ch_advance`.
    cw: f32,
    // Cell height in logical pixels — measured via `ascent + descent`.
    ch: f32,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let scroll_handle = scroll_handle.clone();
    let scroll_for_overlay = scroll_handle.clone();
    let focus_handle = focus_handle.clone();
    let view = cx.entity();
    let (cursor_row, cursor_col) = snap.cursor;
    let grid_rows = snap.rows;
    let grid_cols = snap.cols;
    let font_name = font.to_string();
    v_flex().flex_1().size_full().bg(Colors::shikkoku()).child(
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
            // focusable() sets focusable=true + tab_stop=true.
            // This causes GPUI to auto-register a mouse-down handler that
            // calls window.focus(handle) on click, enabling key event delivery.
            // tab_stop(true) alone does NOT set focusable=true, so no FocusHandle
            // was created and key events were never dispatched here.
            //
            // capture_key_down fires in the CAPTURE phase (top-down), before GPUI's
            // action dispatch. on_key_down fires in the BUBBLE phase (after action
            // dispatch), so GPUI built-in actions (Enter, arrows, Tab, Escape…) would
            // consume the event first. A terminal emulator must intercept ALL keys
            // before GPUI's action system — capture phase is the correct hook.
            //
            // track_focus binds OUR FocusHandle (instead of the anonymous one
            // focusable() creates) so the IME input handler below can be
            // registered against the same handle.
            .track_focus(&focus_handle)
            // gpui dispatches action bindings BEFORE key listeners, so Root's
            // global tab/shift-tab (focus cycling) would eat Tab before
            // capture_key_down ever ran. This deeper key context carries our
            // own tab bindings (see main.rs), routed straight to the PTY.
            .key_context(TERMINAL_KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &TerminalSendTab, _window, _cx| {
                this.send_bytes_to_active(b"\t");
            }))
            .on_action(cx.listener(|this, _: &TerminalSendBacktab, _window, _cx| {
                this.send_bytes_to_active(b"\x1b[Z");
            }))
            .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                this.copy_selection(cx);
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                // Stop propagation for consumed keys so GPUI's own actions
                // (tab focus-cycling etc.) never steal them. Keys left to the
                // text-input path must keep propagating, or the platform never
                // generates the corresponding WM_CHAR.
                if this.handle_terminal_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _window, cx| {
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
                }),
            )
            .p_1()
            // Overlay with two jobs, both requiring paint-phase access:
            // 1. Register the IME input handler for the terminal viewport.
            //    Without this, WM_CHAR / IME composition events are dropped
            //    and Japanese input never reaches the PTY.
            // 2. Draw the IME preedit inline at the terminal cursor. GPUI
            //    consumes WM_IME_COMPOSITION, so the OS composition window
            //    never appears — if we don't draw the marked text, nothing
            //    is visible until the composition is committed.
            .child(
                canvas(
                    |_bounds, _window, _cx| (),
                    move |bounds, (), window, cx: &mut App| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, view.clone()),
                            cx,
                        );

                        // ── Mouse selection hit-testing ────────────────────
                        // Maps pointer position → grid cell. The highlight
                        // itself is painted by render_grid inside each row
                        // canvas: quads painted from THIS overlay canvas are
                        // not composited to the screen (empirically verified
                        // 2026-07-03 — handle_input and ShapedLine::paint work
                        // here, but paint_quad does not), and the row canvas
                        // already lives in grid coordinates anyway.
                        // The scroll offset is read at event time (not
                        // captured) because the pane keeps auto-scrolling.
                        let cell_at = move |scroll: &ScrollHandle, pos: Point<Pixels>| {
                            let off = scroll.offset();
                            let gx = f32::from(pos.x - bounds.origin.x) - f32::from(off.x);
                            let gy = f32::from(pos.y - bounds.origin.y) - f32::from(off.y);
                            let col = ((gx / cw).floor().max(0.0) as usize)
                                .min(grid_cols.saturating_sub(1));
                            let row = ((gy / ch).floor().max(0.0) as usize)
                                .min(grid_rows.saturating_sub(1));
                            (row, col)
                        };
                        window.on_mouse_event({
                            let view = view.clone();
                            let scroll = scroll_for_overlay.clone();
                            move |ev: &MouseDownEvent, phase, _window, cx| {
                                if phase != DispatchPhase::Bubble
                                    || ev.button != MouseButton::Left
                                    || !bounds.contains(&ev.position)
                                {
                                    return;
                                }
                                let (row, col) = cell_at(&scroll, ev.position);
                                view.update(cx, |this, cx| {
                                    this.begin_selection(is_shogun, row, col);
                                    cx.notify();
                                });
                            }
                        });
                        window.on_mouse_event({
                            let view = view.clone();
                            let scroll = scroll_for_overlay.clone();
                            move |ev: &MouseMoveEvent, phase, _window, cx| {
                                if phase != DispatchPhase::Bubble
                                    || ev.pressed_button != Some(MouseButton::Left)
                                {
                                    return;
                                }
                                let (row, col) = cell_at(&scroll, ev.position);
                                view.update(cx, |this, cx| {
                                    if this.update_selection(row, col) {
                                        cx.notify();
                                    }
                                });
                            }
                        });
                        window.on_mouse_event({
                            let view = view.clone();
                            move |ev: &MouseUpEvent, phase, _window, cx| {
                                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left
                                {
                                    return;
                                }
                                view.update(cx, |this, cx| {
                                    if this.end_selection() {
                                        cx.notify();
                                    }
                                });
                            }
                        });

                        let Some(pre) = ime_preedit.as_ref().filter(|s| !s.is_empty()) else {
                            return;
                        };
                        // Cursor cell position; the grid is bottom-anchored in
                        // the viewport when taller than it (auto-scroll).
                        let grid_h = grid_rows as f32 * ch;
                        let vp_h = f32::from(bounds.size.height);
                        let overflow = (grid_h - vp_h).max(0.0);
                        let x = f32::from(bounds.origin.x) + cursor_col as f32 * cw;
                        let y = f32::from(bounds.origin.y) + cursor_row as f32 * ch - overflow;

                        let fg: gpui::Hsla = rgba(0xffffffff).into();
                        let text_run = TextRun {
                            len: pre.len(),
                            font: gpui::font(font_name.clone()),
                            color: fg,
                            background_color: Some(rgba(0x1e3a5fff).into()),
                            underline: Some(UnderlineStyle {
                                color: Some(fg),
                                thickness: px(1.),
                                wavy: false,
                            }),
                            strikethrough: None,
                        };
                        let line = window.text_system().shape_line(
                            pre.clone().into(),
                            px(13.),
                            &[text_run],
                            None,
                        );
                        let origin = point(px(x), px(y));
                        let _ = line.paint_background(origin, px(ch), window, cx);
                        let _ = line.paint(origin, px(ch), window, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(render_grid(snap, font, cw, ch, selection)),
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
    v_flex().flex_1().size_full().bg(Colors::shikkoku()).child(
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
        .child(div().text_color(Colors::muted()).child("接続中..."))
}
