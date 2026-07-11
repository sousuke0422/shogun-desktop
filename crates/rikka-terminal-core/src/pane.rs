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

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, ElementInputHandler, Entity, EntityInputHandler as _, FocusHandle, IntoElement, Pixels,
    Window, canvas, prelude::*, px,
};

use crate::ime::{ImeHost, TerminalIme};
use crate::selection::{SelectionHost, register_mouse_selection};

/// Caret rectangle in client-area **physical pixels**: `(left, top, right,
/// bottom)`. Handed to the app so it can place an IME candidate window (TSF);
/// `None` means no layout yet or the input isn't focused. Plain ints keep the
/// engine free of the platform IME crate's types.
pub type CaretPx = (i32, i32, i32, i32);

/// Geometry and wiring for [`pane_overlay`].
///
/// Grouped into a struct because the positional list had grown long enough
/// (cell metrics, grid dims, inset) to invite silent `cw`/`ch`/`inset`
/// mix-ups. The two hosts differ only in the last three fields — the flush
/// vs. padded pin, the caret gate, and whether they want the painted size
/// reported back — so those are the fields worth reading at each call site.
pub struct PaneOverlay<V, H>
where
    V: SelectionHost + 'static,
    H: ImeHost + 'static,
{
    /// The pane's focus handle: the IME input handler is registered against
    /// it (gpui only routes WM_CHAR / composition to the focused element).
    pub focus_handle: FocusHandle,
    /// Shared IME text-input handler (see [`crate::ime`]).
    pub ime: Entity<TerminalIme<H>>,
    /// The host view — owns the selection state and the pane's session.
    pub view: Entity<V>,
    /// Pane index for multi-pane hosts (shogun's SSH panes); single-pane = 0.
    pub pane: usize,
    /// Cell width / height (logical px) for the click→cell mapping.
    pub cw: f32,
    pub ch: f32,
    /// Visible grid dimensions used to clamp the hit test.
    pub grid_rows: usize,
    pub grid_cols: usize,
    /// Inset (logical px) applied to all four edges when pinning the overlay.
    /// `0.0` pins flush to the relative parent (rikka, where the wrapper is
    /// already the grid's content box); shogun's shell pane insets by its
    /// `.p_1()` so the overlay lines up with the padded grid content box.
    pub inset: f32,
    /// Caret gate. When `false` the per-frame `bounds_for_range` is skipped
    /// and `caret_sink` is never called — wire it to the platform IME's
    /// enabled flag (shogun: `tsf::enabled()`; rikka feeds it unconditionally).
    pub caret_enabled: bool,
    /// Optional size sink. When `Some`, the painted pane size (logical px,
    /// padding box) is written back and a deferred `notify` scheduled, so the
    /// host can resize the PTY to the true painted fit instead of estimating
    /// chrome. `None` hosts (rikka) derive the size from the viewport.
    pub measured: Option<Rc<Cell<(f32, f32)>>>,
}

/// The overlay canvas, pinned over its relative parent's content box.
///
/// Register it as the sibling of `render_grid` inside a `relative()` wrapper
/// that is the pane's content box. Pinning is a four-edge inset: a bare
/// `absolute()` would fall back to the static position *below* the grid
/// sibling and break the bounds every listener checks (selection dead, caret
/// far off). `inset == 0.0` reproduces a flush `top_0/left_0/size_full` pin;
/// a positive inset lines the overlay up with a padded grid. It re-registers
/// every frame (listeners are per-frame).
///
/// `caret_sink` receives the caret rect while the input is focused and
/// [`PaneOverlay::caret_enabled`] is set — the app forwards it to its
/// platform IME (or ignores it).
pub fn pane_overlay<V, H>(
    args: PaneOverlay<V, H>,
    caret_sink: impl Fn(Option<CaretPx>) + 'static,
) -> impl IntoElement
where
    V: SelectionHost + 'static,
    H: ImeHost + 'static,
{
    let PaneOverlay {
        focus_handle,
        ime,
        view,
        pane,
        cw,
        ch,
        grid_rows,
        grid_cols,
        inset,
        caret_enabled,
        measured,
    } = args;
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds: Bounds<Pixels>, (), window: &mut Window, cx| {
            // Report the painted pane size back to the host (deferred notify —
            // we are inside paint) so the PTY is resized to the true fit.
            // Hosts that estimate the size from the viewport pass `None`.
            if let Some(measured) = &measured {
                let painted = (bounds.size.width / px(1.), bounds.size.height / px(1.));
                let (pw, ph) = measured.get();
                if (pw - painted.0).abs() > 0.5 || (ph - painted.1).abs() > 0.5 {
                    measured.set(painted);
                    let view = view.clone();
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |_, cx| cx.notify());
                    });
                }
            }
            // Route WM_CHAR / IME composition to the terminal's input handler.
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, ime.clone()),
                cx,
            );
            // Feed the caret rect (client physical px) so an IME candidate
            // window opens at the terminal cursor.
            if caret_enabled && focus_handle.is_focused(window) {
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
    .top(px(inset))
    .left(px(inset))
    .right(px(inset))
    .bottom(px(inset))
}
