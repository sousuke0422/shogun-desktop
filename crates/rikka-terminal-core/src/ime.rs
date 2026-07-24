//! Shared IME / text-input integration for terminal windows.
//!
//! GPUI only delivers WM_CHAR and IME composition events to an input handler
//! registered on the focused element (`Window::handle_input`). Every terminal
//! window needs the exact same handler behavior — a terminal has no editable
//! document: committed text is forwarded to the PTY, the preedit is drawn
//! inline at the cursor by `render_grid`, and all document queries answer
//! "empty".
//!
//! [`EntityInputHandler`] cannot be blanket-implemented for host windows
//! (orphan rule: foreign trait over an uncovered type parameter), so the
//! shared implementation lives on the dedicated [`TerminalIme`] entity
//! instead. Each window owns one, registers it via
//! `ElementInputHandler::new(bounds, ime.clone())`, and observes it so
//! composition changes trigger a re-render.

use crate::TerminalSession;
use crate::renderer::measure_cell_metrics;
use gpui::{Bounds, Context, Pixels, UTF16Selection, WeakEntity, Window, point, px, size};

/// What a window must provide for its [`TerminalIme`] handler.
pub trait ImeHost: 'static {
    /// The PTY session that should receive committed text (and whose cursor
    /// positions the IME candidate window).
    fn ime_session(&self) -> Option<&TerminalSession>;
    /// Terminal font, for the caret-rect cell metrics.
    fn ime_font(&self) -> &str;
    /// Deliver committed/typed text (WM_CHAR and IME commits land here).
    /// Default: the focused session. Hosts that fan input out (broadcast
    /// input) override this — it is the single typed-text choke point.
    fn ime_commit(&self, text: &str) {
        if let Some(session) = self.ime_session() {
            session.send_bytes(text.as_bytes());
        }
    }
}

/// Reusable text-input handler entity for one terminal window.
pub struct TerminalIme<H: ImeHost> {
    host: WeakEntity<H>,
    /// Current IME composition (marked) text, if any. The host window reads
    /// this each render and passes it to `render_grid`, which draws it inline
    /// at the terminal cursor.
    pub marked: Option<String>,
}

impl<H: ImeHost> TerminalIme<H> {
    pub fn new(host: WeakEntity<H>) -> Self {
        Self { host, marked: None }
    }
}

impl<H: ImeHost> gpui::EntityInputHandler for TerminalIme<H> {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // A zero-width caret; required so the platform can query the caret
        // rect (bounds_for_range) to position the IME candidate window.
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked.as_ref().map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if let Some(host) = self.host.upgrade() {
            host.read(cx).ime_commit(text);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // GPUI consumes WM_IME_COMPOSITION, so the OS never draws its own
        // composition window — the preedit is rendered inline at the terminal
        // cursor by render_grid.
        self.marked = Some(new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let host = self.host.upgrade()?;
        let host = host.read(cx);
        let (row, col, rows) = {
            let session = host.ime_session()?;
            let snap = session.snapshot.lock();
            let (row, col) = snap.cursor;
            (row, col, snap.rows)
        };

        let (cw, ch) =
            measure_cell_metrics(&cx.text_system(), host.ime_font(), window.scale_factor());

        // The grid is bottom-anchored in its scroll viewport when it is taller
        // than the visible area (auto scroll-to-bottom), so shift the caret up
        // by the overflow.
        let grid_h = rows as f32 * ch;
        let viewport_h = f32::from(element_bounds.size.height);
        let scroll_overflow = (grid_h - viewport_h).max(0.0);

        Some(Bounds {
            origin: point(
                element_bounds.origin.x + px(col as f32 * cw),
                element_bounds.origin.y + px(row as f32 * ch - scroll_overflow),
            ),
            size: size(px(cw), px(ch)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
