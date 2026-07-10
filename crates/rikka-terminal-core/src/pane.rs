//! The shared terminal-pane overlay.
//!
//! Both shogun-desktop and rikka-terminal build their terminal pane as
//! `render_grid(...)` plus this paint-phase overlay canvas. The overlay is
//! where the click→cell→link mapping, the drag selection, and the IME input
//! handler all live, so keeping it here — one implementation, one place to
//! test — means a fix (or a regression test) in one product covers the
//! other. The surrounding chrome (tabs, SSH panes, padding) stays per-app;
//! only this hit-testing/IME core is shared.
//!
//! Nothing is drawn from the overlay (paint calls issued here never reach the
//! screen — verified 2026-07-03); the grid and the selection highlight are
//! painted by [`crate::renderer::render_grid`].

use gpui::{
    Bounds, ElementInputHandler, Entity, EntityInputHandler as _, FocusHandle, IntoElement, Pixels,
    Window, canvas, prelude::*,
};

use crate::ime::{ImeHost, TerminalIme};
use crate::selection::{SelectionHost, register_mouse_selection};

/// Caret rectangle in client-area **physical pixels**: `(left, top, right,
/// bottom)`. Handed to the app so it can place an IME candidate window (TSF);
/// `None` means no layout yet or the input isn't focused. Plain ints keep the
/// engine free of the platform IME crate's types.
pub type CaretPx = (i32, i32, i32, i32);

/// The overlay canvas, pinned flush to its relative parent's origin.
///
/// Register it as the sibling of `render_grid` inside a `relative()` wrapper
/// that is the pane's content box. Pinning is `top_0/left_0/size_full`: a
/// bare `absolute()` would fall back to the static position *below* the grid
/// sibling and break the bounds every listener checks (selection dead, caret
/// far off). It re-registers every frame (listeners are per-frame).
///
/// `caret_sink` receives the caret rect while the input is focused — the app
/// forwards it to its platform IME (or ignores it).
pub fn pane_overlay<V, H>(
    focus_handle: FocusHandle,
    ime: Entity<TerminalIme<H>>,
    view: Entity<V>,
    pane: usize,
    cw: f32,
    ch: f32,
    grid_rows: usize,
    grid_cols: usize,
    caret_sink: impl Fn(Option<CaretPx>) + 'static,
) -> impl IntoElement
where
    V: SelectionHost + 'static,
    H: ImeHost + 'static,
{
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds: Bounds<Pixels>, (), window: &mut Window, cx| {
            // Route WM_CHAR / IME composition to the terminal's input handler.
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, ime.clone()),
                cx,
            );
            // Feed the caret rect (client physical px) so an IME candidate
            // window opens at the terminal cursor.
            if focus_handle.is_focused(window) {
                let caret =
                    ime.update(cx, |ime, cx| ime.bounds_for_range(0..0, bounds, window, cx));
                let scale = window.scale_factor();
                caret_sink(caret.map(|b| {
                    let x = f32::from(b.origin.x);
                    let y = f32::from(b.origin.y);
                    let w = f32::from(b.size.width);
                    let h = f32::from(b.size.height);
                    (
                        (x * scale) as i32,
                        (y * scale) as i32,
                        ((x + w) * scale) as i32,
                        ((y + h) * scale) as i32,
                    )
                }));
            }
            register_mouse_selection(
                window,
                view.clone(),
                bounds,
                pane,
                cw,
                ch,
                grid_rows,
                grid_cols,
            );
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}
