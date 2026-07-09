//! App-driven Windows TSF IME hookup (see the rikka-terminal-gpui-ime crate).
//!
//! gpui composes through IMM32, which leaves the Windows taskbar input
//! indicator (あ/A) unaware of our windows. Giving TSF a focus document backed
//! by a real text store makes the indicator track us (hardware-verified:
//! A→あ→A). Once TSF is engaged the IME composes into that store instead of
//! the IMM32 path, so the app drains [`ImeEvent`]s: `Preedit` renders inline
//! at the terminal cursor (the existing `ime.marked` path) and `Commit` goes
//! to the PTY. We drive focus/blur from the app's own window focus events so
//! gpui stays untouched.
//!
//! Gated by the `SHOGUN_TSF` env var until the TSF input path has soaked;
//! default behaviour is unchanged (pure IMM32).

use std::sync::OnceLock;

use rikka_terminal_gpui_ime::{ImeEvent, TextSnapshot, TsfTextClient};

/// Opt-in while the TSF input path is under validation.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SHOGUN_TSF").is_some())
}

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
    if enabled() {
        rikka_terminal_gpui_ime::set_waker(waker);
        // hwnd 0 → the store answers GetWnd with the foreground window.
        rikka_terminal_gpui_ime::focus(0, &mut EmptyClient);
    }
}

/// The focused terminal input lost focus.
pub fn on_input_blur() {
    if enabled() {
        rikka_terminal_gpui_ime::blur();
    }
}

/// Drain queued IME events, oldest first (always empty when the gate is off).
pub fn drain() -> Vec<ImeEvent> {
    if enabled() {
        rikka_terminal_gpui_ime::drain_events()
    } else {
        Vec::new()
    }
}
