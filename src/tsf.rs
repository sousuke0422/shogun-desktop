//! App-driven Windows TSF IME hookup (see the rikka-terminal-gpui-ime crate).
//!
//! gpui composes through IMM32, which leaves the Windows taskbar input
//! indicator (あ/A) unaware of our windows. Giving TSF a focus document backed
//! by a real text store makes the indicator track us. We drive focus/blur from
//! the app's own window focus events so gpui stays untouched.
//!
//! Gated by the `SHOGUN_TSF` env var while the input path is still being wired
//! (M1a is focus-only — it verifies the indicator tracks; input application
//! follows), so default behaviour is unchanged and input keeps using IMM32.

use std::sync::OnceLock;

use rikka_terminal_gpui_ime::{TextEdit, TextSnapshot, TsfTextClient};

/// Opt-in while the TSF input path is under construction.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SHOGUN_TSF").is_some())
}

/// A terminal has no editable document — the store reports "empty" and (for
/// now) discards edits. Replaced by a PTY-committing client once composition
/// tracking lands (M1b).
struct EmptyClient;

impl TsfTextClient for EmptyClient {
    fn snapshot(&mut self) -> TextSnapshot {
        TextSnapshot::default()
    }

    fn apply(&mut self, _edits: &[TextEdit]) {}
}

/// A terminal input gained keyboard focus.
pub fn on_input_focus() {
    if enabled() {
        // hwnd 0 → the store uses GetForegroundWindow (the focused window).
        rikka_terminal_gpui_ime::focus(0, &mut EmptyClient);
    }
}

/// A terminal input lost keyboard focus.
pub fn on_input_blur() {
    if enabled() {
        rikka_terminal_gpui_ime::blur();
    }
}
