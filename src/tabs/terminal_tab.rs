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
    App, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, IntoElement,
    KeyDownEvent, ParentElement, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement,
    Styled, canvas, div, prelude::*, px,
};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::v_flex;

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
    // Ctrl-hovered OSC 8 hyperlink index for the hover underline.
    hover_link: Option<u16>,
    // Kitty-graphics image store of the session shown in this pane.
    images: Option<&crate::terminal::kitty_graphics::KittyImageStore>,
    is_shogun: bool,
    font: &str,
    // Cell width in logical pixels — measured via `TextSystem::ch_advance`.
    cw: f32,
    // Cell height in logical pixels — measured via `ascent + descent`.
    ch: f32,
    // Written back with the pane's painted size (padding box) each frame so
    // the view derives rows/cols from reality instead of a chrome estimate.
    pane_measured: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    // Written back with the pane's painted origin (content box) so wheel
    // positions can be mapped to grid cells for mouse reporting.
    pane_origin: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let scroll_handle = scroll_handle.clone();
    let focus_handle = focus_handle.clone();
    let menu_focus = focus_handle.clone();
    let view = cx.entity();
    let grid_rows = snap.rows;
    let grid_cols = snap.cols;
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
                    // tmux scroll: when the application asked for the wheel
                    // (tmux `mouse on` → mouse reporting, or alternate
                    // scroll), forward it to the PTY — tmux then scrolls its
                    // *server-side* history (copy-mode), which is where the
                    // scrollback actually lives for these panes. A wheel the
                    // PTY doesn't take has nothing local to move: the pane
                    // renders only the visible grid (display_iter) and the
                    // PTY is resized to fit, so the container never
                    // overflows. (The autoscroll-lock bookkeeping that used
                    // to live here predated PTY-fit sizing and was inert —
                    // and its wheel-direction test was inverted to boot.)
                    if this.wheel_to_pty_for_pane(is_shogun, event, cw, ch) {
                        cx.stop_propagation();
                    }
                }),
            )
            .p_1()
            // Overlay with two jobs, both requiring paint-phase access:
            // 1. Register the IME input handler for the terminal viewport.
            //    Without this, WM_CHAR / IME composition events are dropped
            //    and Japanese input never reaches the PTY.
            // 2. Mouse-selection hit-testing (window-level listeners).
            // NOTE: nothing may be *drawn* from this canvas — paint calls
            // issued here never reach the screen (verified 2026-07-03). The
            // preedit and selection highlight are painted by render_grid.
            .child(
                canvas(
                    |_bounds, _window, _cx| (),
                    move |bounds, (), window, cx: &mut App| {
                        // Report the painted pane size back to the view.
                        // A change re-renders (deferred — we are inside
                        // paint) so the PTY is resized to the true fit.
                        let painted = (bounds.size.width / px(1.), bounds.size.height / px(1.));
                        pane_origin.set((bounds.origin.x / px(1.), bounds.origin.y / px(1.)));
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

                        // TSF (gated): feed the caret rect (client physical
                        // px) so the IME candidate window opens at the
                        // terminal cursor.
                        if crate::tsf::enabled() && focus_handle.is_focused(window) {
                            let caret = ime.update(cx, |ime, cx| {
                                ime.bounds_for_range(0..0, bounds, window, cx)
                            });
                            let scale = window.scale_factor();
                            crate::tsf::set_caret(caret.map(|b| {
                                rikka_terminal_gpui_ime::CaretRect {
                                    left: (f32::from(b.origin.x) * scale) as i32,
                                    top: (f32::from(b.origin.y) * scale) as i32,
                                    right: ((f32::from(b.origin.x) + f32::from(b.size.width))
                                        * scale) as i32,
                                    bottom: ((f32::from(b.origin.y) + f32::from(b.size.height))
                                        * scale) as i32,
                                }
                            }));
                        }

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
                // Pinned to the CONTENT box (inset by the pane padding), NOT
                // size_full: taffy counts absolute children toward the
                // scroll container's content size (inflow ⊔ absolute), and a
                // padding-box-sized overlay plus the re-added padding made
                // the pane permanently scrollable by 8px — the micro-scroll.
                // scroll_to_bottom() then pinned that phantom overflow,
                // clipping the top row and leaving a bottom gap. Bonus: the
                // overlay origin now coincides with the grid origin, so the
                // selection pointer→cell mapping loses its 4px skew.
                .absolute()
                .top(px(TERMINAL_PANE_PADDING_PX / 2.0))
                .left(px(TERMINAL_PANE_PADDING_PX / 2.0))
                .right(px(TERMINAL_PANE_PADDING_PX / 2.0))
                .bottom(px(TERMINAL_PANE_PADDING_PX / 2.0)),
            )
            .child(render_grid(
                snap,
                font,
                cw,
                ch,
                selection,
                hover_link,
                images,
                ime_preedit,
            ))
            // Right-click menu: dispatches the same actions as the keyboard
            // shortcuts. action_context routes them to the terminal focus
            // handle, i.e. the pane's TERMINAL_KEY_CONTEXT on_action handlers.
            .context_menu(move |menu, _window, _cx| {
                menu.action_context(menu_focus.clone())
                    .menu("コピー", Box::new(TerminalCopy))
                    .menu("ペースト", Box::new(TerminalPaste))
            }),
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
