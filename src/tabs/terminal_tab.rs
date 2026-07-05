use crate::terminal::GridSnapshot;
use crate::terminal::ime::TerminalIme;
use crate::terminal::renderer::render_grid;
use crate::terminal::selection;
use crate::theme::Colors;
use crate::window::{
    ShogunWindow, TERMINAL_KEY_CONTEXT, TERMINAL_PANE_PADDING_PX, TerminalCopy, TerminalPaste,
    TerminalSendBacktab, TerminalSendTab, selection_pane,
};
use gpui::{
    App, Context, ElementInputHandler, Entity, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, ScrollDelta, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement, Styled,
    canvas, div, prelude::*, px,
};
use gpui_component::menu::ContextMenuExt as _;
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
    // Shared IME text-input handler entity (registered on the overlay canvas).
    ime: Entity<TerminalIme<ShogunWindow>>,
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
    // Written back with the pane's painted size (padding box) each frame so
    // the view derives rows/cols from reality instead of a chrome estimate.
    pane_measured: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let scroll_handle = scroll_handle.clone();
    let focus_handle = focus_handle.clone();
    let menu_focus = focus_handle.clone();
    let view = cx.entity();
    let grid_rows = snap.rows;
    let grid_cols = snap.cols;
    let pane = div()
            .id(if is_shogun {
                "terminal-pane-shogun"
            } else {
                "terminal-pane-multiagent"
            })
            .size_full()
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
                this.send_tab_to_active(false);
            }))
            .on_action(cx.listener(|this, _: &TerminalSendBacktab, _window, _cx| {
                this.send_tab_to_active(true);
            }))
            .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &TerminalPaste, _window, cx| {
                this.paste_clipboard(cx);
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
            .child(render_grid(snap, font, cw, ch, selection, ime_preedit))
            // Right-click menu: dispatches the same actions as the keyboard
            // shortcuts. action_context routes them to the terminal focus
            // handle, i.e. the pane's TERMINAL_KEY_CONTEXT on_action handlers.
            .context_menu(move |menu, _window, _cx| {
                menu.action_context(menu_focus.clone())
                    .menu("コピー", Box::new(TerminalCopy))
                    .menu("ペースト", Box::new(TerminalPaste))
            });

    // Overlay with three jobs, all requiring paint-phase access:
    // 1. Register the IME input handler for the terminal viewport.
    //    Without this, WM_CHAR / IME composition events are dropped
    //    and Japanese input never reaches the PTY.
    // 2. Mouse-selection hit-testing (window-level listeners).
    // 3. Report the pane size for PTY resize (pane_measured).
    // NOTE: nothing may be *drawn* from this canvas — paint calls
    // issued here never reach the screen (verified 2026-07-03). The
    // preedit and selection highlight are painted by render_grid.
    //
    // The canvas lives OUTSIDE the scroll container, as a sibling in a
    // relative wrapper: taffy sizes absolute children of a scroll container
    // to its CONTENT box, so inside the pane the overlay's height tracked the
    // grid (rows × cell) instead of the viewport — rows could then never
    // shrink or grow past the current grid height (the resize-never-fires
    // bug, found 2026-07-05). The wrapper's box IS the pane's border box, and
    // the pane never scrolls (the grid always fits exactly), so the overlay
    // geometry is identical.
    let overlay = canvas(
                    |_bounds, _window, _cx| (),
                    move |bounds, (), window, cx: &mut App| {
                        // Report the painted pane size back to the view.
                        // A change re-renders (deferred — we are inside
                        // paint) so the PTY is resized to the true fit.
                        let painted = (bounds.size.width / px(1.), bounds.size.height / px(1.));
                        let (pw, ph) = pane_measured.get();
                        if (pw - painted.0).abs() > 0.5 || (ph - painted.1).abs() > 0.5 {
                            pane_measured.set(painted);
                            let view = view.clone();
                            cx.defer(move |cx| view.update(cx, |_, cx| cx.notify()));
                        }

                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, ime.clone()),
                            cx,
                        );

                        // ── Mouse selection hit-testing ────────────────────
                        // Shared listeners (terminal::selection) map pointer →
                        // grid cell. The highlight itself is painted by
                        // render_grid inside each row canvas (paint calls from
                        // this overlay canvas do not reach the screen).
                        selection::register_mouse_selection(
                            window,
                            view.clone(),
                            bounds,
                            selection_pane(is_shogun),
                            cw,
                            ch,
                            grid_rows,
                            grid_cols,
                        );
                    },
                )
    // Pinned to the pane's content box (border box minus the pane's p_1
    // padding) via the relative wrapper. The overlay origin coincides with
    // the grid origin, so the selection pointer→cell mapping has no skew.
    .absolute()
    .top(px(TERMINAL_PANE_PADDING_PX / 2.0))
    .left(px(TERMINAL_PANE_PADDING_PX / 2.0))
    .right(px(TERMINAL_PANE_PADDING_PX / 2.0))
    .bottom(px(TERMINAL_PANE_PADDING_PX / 2.0));

    v_flex().flex_1().size_full().bg(Colors::shikkoku()).child(
        div()
            .relative()
            .flex_1()
            .w_full()
            .child(pane)
            .child(overlay),
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
