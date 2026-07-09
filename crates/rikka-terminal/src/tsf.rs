//! Always-on Windows TSF IME hookup (rikka-terminal-gpui-ime).
//!
//! Same glue as shogun-desktop's `tsf` module minus the `SHOGUN_TSF` gate:
//! RikkaTerminal is the soak vehicle for the TSF store — daily use here is
//! the validation that lets shogun-desktop eventually drop its gate. The
//! taskbar input indicator (あ/A) tracks the window, composition flows
//! through the real text store ([`ImeEvent::Preedit`] renders inline via
//! `ime.marked`, [`ImeEvent::Commit`] goes to the PTY), and the candidate
//! window opens at the caret rect the app feeds back. On non-Windows the
//! backend is a no-op.

use rikka_terminal_gpui_ime::{ImeEvent, TextSnapshot, TsfTextClient};

/// A terminal has no editable document — the store starts empty and is reset
/// after every commit, so the document is always exactly the live composition.
struct EmptyClient;

impl TsfTextClient for EmptyClient {
    fn snapshot(&mut self) -> TextSnapshot {
        TextSnapshot::default()
    }
}

/// A terminal input gained keyboard focus. `waker` must schedule (not run) a
/// re-render of the owning window; it is invoked whenever the IME queues new
/// events, so the window's render can drain them promptly via [`drain`].
pub fn on_input_focus(waker: Box<dyn Fn()>) {
    rikka_terminal_gpui_ime::set_waker(waker);
    // hwnd 0 → the store answers GetWnd with the foreground window, which
    // also keeps detached tab windows correct: whichever window's terminal
    // takes focus re-binds the store to itself.
    rikka_terminal_gpui_ime::focus(0, &mut EmptyClient);
}

/// The focused terminal input lost focus.
pub fn on_input_blur() {
    rikka_terminal_gpui_ime::blur();
}

/// Update the focused terminal's caret rectangle (client-area physical px) so
/// the IME candidate window opens at the cursor.
pub fn set_caret(rect: Option<rikka_terminal_gpui_ime::CaretRect>) {
    rikka_terminal_gpui_ime::set_caret(rect);
}

/// Drain queued IME events, oldest first.
pub fn drain() -> Vec<ImeEvent> {
    rikka_terminal_gpui_ime::drain_events()
}
