//! IME status integration for gpui windows — makes the OS input indicator (the
//! Windows taskbar あ/A today; Linux/macOS as later rounds) track the focused
//! gpui window.
//!
//! Why this exists: gpui's Windows backend composes text through IMM32, which
//! is enough to *input* Japanese but leaves the modern TSF-driven taskbar
//! indicator unaware of our windows. Windows 11's new IME (22H2+) no longer
//! emits the legacy `IMN_SETOPENSTATUS` / `IMN_SETCONVERSIONMODE` notifications
//! — a field trace of our window proc saw only candidate/composition messages,
//! never those two — so the indicator never learns our conversion mode. The
//! standard fix (per Microsoft's TSF guidance; winit and others are IMM32-only
//! and don't do it) is to participate in the Text Services Framework: activate
//! an `ITfThreadMgr` on the UI thread and give each window a focus document, so
//! TSF tracks our windows and drives the indicator. Composition itself keeps
//! running through gpui's IMM32 path; this only adds the missing TSF focus.
//!
//! The engine core (`rikka-terminal`) stays free of all this. The Windows TSF
//! backend lives behind a platform-neutral trait so a Linux (IBus/fcitx) or
//! macOS backend can be dropped in as a separate round without touching gpui —
//! and so the abstraction compiles and is checkable on non-Windows hosts.

use std::cell::RefCell;

/// Per-thread hook that keeps the OS input indicator in sync with the focused
/// window. One implementation per platform; [`NoopIme`] wherever unsupported.
///
/// TSF (and the equivalents on other platforms) is thread-affine and gpui runs
/// its window procedure on a single thread, so the integration is modelled at
/// thread scope with windows associated/dissociated by their native handle.
pub trait ImeThreadIntegration {
    /// A window on this thread was created; begin tracking it so the OS
    /// indicator reflects its IME state while it is focused. `hwnd` is the
    /// platform window handle as an `isize` (a Win32 `HWND`, etc.).
    fn associate_window(&mut self, hwnd: isize);
    /// A tracked window is going away; stop tracking it.
    fn dissociate_window(&mut self, hwnd: isize);
}

/// Fallback backend: does nothing. Used on platforms without a backend yet, and
/// when platform initialisation fails — IME input keeps working through the
/// existing path, only the indicator stays as it is today.
pub struct NoopIme;

impl ImeThreadIntegration for NoopIme {
    fn associate_window(&mut self, _hwnd: isize) {}
    fn dissociate_window(&mut self, _hwnd: isize) {}
}

#[cfg(windows)]
mod windows;

thread_local! {
    /// One integration per UI thread, built lazily on first use. Keeping it
    /// thread-local lets the gpui call sites stay free functions.
    static CURRENT: RefCell<Option<Box<dyn ImeThreadIntegration>>> = const { RefCell::new(None) };
}

/// Build the platform backend for this thread, falling back to [`NoopIme`].
fn backend() -> Box<dyn ImeThreadIntegration> {
    #[cfg(windows)]
    {
        match windows::WindowsTsfIme::new() {
            Ok(ime) => Box::new(ime),
            Err(_) => Box::new(NoopIme),
        }
    }
    #[cfg(not(windows))]
    {
        Box::new(NoopIme)
    }
}

/// Begin tracking a freshly created window (call from the platform's window
/// creation path). Initialises this thread's backend on first use. No-op where
/// no backend exists.
pub fn associate_window(hwnd: isize) {
    CURRENT.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.is_none() {
            *slot = Some(backend());
        }
        if let Some(ime) = slot.as_mut() {
            ime.associate_window(hwnd);
        }
    });
}

/// Stop tracking a window (call from the window destroy path). No-op if the
/// window was never associated or no backend is active.
pub fn dissociate_window(hwnd: isize) {
    CURRENT.with(|c| {
        if let Some(ime) = c.borrow_mut().as_mut() {
            ime.dissociate_window(hwnd);
        }
    });
}
