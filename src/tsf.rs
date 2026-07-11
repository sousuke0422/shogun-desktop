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
//! Controlled by the `terminal.tsf` setting (on by default). The `SHOGUN_TSF`
//! env var, if set, overrides the setting either way (`SHOGUN_TSF=0` forces
//! off) — the e2e harness and quick A/B checks use it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rikka_terminal_gpui_ime::{ImeEvent, TextSnapshot, TsfTextClient};

/// Runtime enable flag. Seeded from `terminal.tsf` at startup (see
/// [`set_enabled`], called from `main`) and updated when settings are saved.
/// Defaults on so a process that never seeds it still uses the TSF path.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// `SHOGUN_TSF` override, parsed once: `Some(true/false)` when the var is set,
/// `None` when absent (fall through to the setting). Presence means on unless
/// the value is an explicit falsey token, so `SHOGUN_TSF=1` and a bare
/// `SHOGUN_TSF=` both force on while `SHOGUN_TSF=0` forces off.
fn env_override() -> Option<bool> {
    static OVERRIDE: OnceLock<Option<bool>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("SHOGUN_TSF").ok().map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
    })
}

/// Whether the TSF input path is active. The env override wins; otherwise the
/// persisted `terminal.tsf` setting (via [`set_enabled`]).
pub fn enabled() -> bool {
    env_override().unwrap_or_else(|| ENABLED.load(Ordering::Relaxed))
}

/// Apply the persisted `terminal.tsf` setting. Call at startup and whenever
/// settings are saved. The env override, when present, still wins in
/// [`enabled`], so this is a no-op from the user's point of view under it.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
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

/// Update the focused terminal's caret rectangle (client-area physical px) so
/// the IME candidate window opens at the cursor. No-op when the gate is off.
pub fn set_caret(rect: Option<rikka_terminal_gpui_ime::CaretRect>) {
    if enabled() {
        rikka_terminal_gpui_ime::set_caret(rect);
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
