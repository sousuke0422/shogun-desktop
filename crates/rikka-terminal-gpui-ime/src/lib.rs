//! IME text-input integration for gpui windows — participates in the Windows
//! Text Services Framework so the OS input indicator (taskbar あ/A) tracks the
//! focused window and reflects its conversion mode.
//!
//! Why: gpui's Windows backend composes through IMM32, which inputs Japanese
//! fine but leaves the modern TSF-driven taskbar indicator unaware of our
//! windows (a field trace saw candidate/composition messages arrive but never
//! `IMN_SETCONVERSIONMODE` / `IMN_SETOPENSTATUS` — Win11's new IME keeps that
//! state in TSF). Making TSF track the window requires giving it a focus
//! document backed by a real [`ITextStoreACP`] text store — an *empty* focus
//! document instead silently steals input. So this crate implements that text
//! store and bridges it to gpui's text via [`TsfTextClient`].
//!
//! The Windows COM lives behind a platform-neutral trait, so a Linux (IBus) or
//! macOS backend can be a later round, and the core here compiles everywhere.
//!
//! The Windows TSF text store is adapted from the arcweft project
//! (<https://github.com/Sanzentyo/arcweft>, dual Apache-2.0/MIT, used here under
//! MIT). See CREDITS.

use std::cell::RefCell;
use std::ops::Range;

/// A snapshot of the focused input's editable text, selection and caret, in the
/// UTF-16 code-unit offsets shared by TSF's ACP and gpui's `InputHandler` (so no
/// conversion is needed between them).
#[derive(Clone, Debug, Default)]
pub struct TextSnapshot {
    /// The editable text as UTF-16 code units.
    pub text: Vec<u16>,
    /// Selection/caret as UTF-16 offsets into `text` (`start == end` = caret).
    pub selection: Range<usize>,
    /// Screen-space caret rectangle for placing the candidate window. `None`
    /// reports "no layout yet" to TSF (composition still works; the candidate
    /// window falls back to a default position).
    pub caret: Option<CaretRect>,
}

/// Screen-space rectangle in physical pixels. Plain ints keep the trait
/// platform-neutral (the Windows backend converts to `RECT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaretRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// An edit the IME asked us to make, in UTF-16 offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEdit {
    /// Replace `start..end` with `text` (a composition update or a commit).
    Replace {
        start: usize,
        end: usize,
        text: Vec<u16>,
    },
    /// Move the selection/caret to `start..end`.
    SetSelection { start: usize, end: usize },
}

/// The application's editable text as the IME sees it. gpui implements this over
/// its `InputHandler`.
///
/// Called only from the application's own control flow — on focus and when
/// draining queued IME edits — never re-entrantly from inside a TSF COM
/// callback. That is why the backend serves TSF reads from a cached
/// [`TextSnapshot`] and queues writes for the app to apply later.
pub trait TsfTextClient {
    /// The focused input's current text, selection and caret rectangle.
    fn snapshot(&mut self) -> TextSnapshot;
    /// Apply queued IME edits to the focused input, in order.
    fn apply(&mut self, edits: &[TextEdit]);
}

/// Platform backend: keeps the OS IME/indicator in sync with the focused input.
trait Backend {
    /// The window `hwnd` gained focus with the given initial text state.
    fn focus(&mut self, hwnd: isize, snapshot: TextSnapshot);
    /// The focused input lost focus.
    fn blur(&mut self);
    /// Remove and return edits the IME has queued since the last drain.
    fn take_pending(&mut self) -> Vec<TextEdit>;
    /// Replace the cached text state TSF reads from.
    fn set_snapshot(&mut self, snapshot: TextSnapshot);
}

/// No-op backend: platforms without an implementation, or when init fails.
struct NoopBackend;
impl Backend for NoopBackend {
    fn focus(&mut self, _hwnd: isize, _snapshot: TextSnapshot) {}
    fn blur(&mut self) {}
    fn take_pending(&mut self) -> Vec<TextEdit> {
        Vec::new()
    }
    fn set_snapshot(&mut self, _snapshot: TextSnapshot) {}
}

#[cfg(windows)]
mod windows;

/// Field diagnostics: set `SHOGUN_TSF_LOG=<path>` before launch to append every
/// integration event (backend init, focus/blur, and the TSF calls that reach
/// the text store). Off — and cost-free past one env lookup — when unset.
pub(crate) fn tsf_log_write(msg: std::fmt::Arguments<'_>) {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var("SHOGUN_TSF_LOG").ok()) else {
        return;
    };
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

macro_rules! tsf_log {
    ($($arg:tt)*) => { $crate::tsf_log_write(format_args!($($arg)*)) };
}
// Path-addressable (`crate::tsf_log!`) for the windows module; on other
// platforms nothing else needs the re-export.
#[cfg_attr(not(windows), allow(unused_imports))]
pub(crate) use tsf_log;

/// Run the full COM plumbing end-to-end (thread-manager activation, document
/// creation, context push, focus, blur, teardown) and report each step — a
/// headless smoke test for field diagnosis. Does not touch this thread's
/// backend. On non-Windows platforms it just says so.
pub fn self_check() -> String {
    #[cfg(windows)]
    {
        windows::self_check()
    }
    #[cfg(not(windows))]
    {
        "tsf self-check: unsupported platform (no backend)".to_string()
    }
}

thread_local! {
    /// One backend per UI thread, built lazily on first focus. TSF objects are
    /// thread-affine and gpui's window procedure is single-threaded, so a
    /// thread-local keeps the call sites as free functions.
    static BACKEND: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
}

fn make_backend() -> Box<dyn Backend> {
    #[cfg(windows)]
    {
        match windows::WindowsTsf::new() {
            Ok(backend) => {
                tsf_log!("backend: WindowsTsf activated");
                Box::new(backend)
            }
            Err(e) => {
                tsf_log!("backend: WindowsTsf init FAILED ({e}); falling back to no-op");
                Box::new(NoopBackend)
            }
        }
    }
    #[cfg(not(windows))]
    {
        Box::new(NoopBackend)
    }
}

fn with_backend<R>(f: impl FnOnce(&mut dyn Backend) -> R) -> Option<R> {
    BACKEND.with(|b| b.borrow_mut().as_mut().map(|be| f(be.as_mut())))
}

/// The window `hwnd` gained keyboard focus on an editable input; begin TSF
/// tracking so the OS indicator reflects its IME mode. Loads the initial text
/// state from `client`. Builds this thread's backend on first use.
pub fn focus(hwnd: isize, client: &mut dyn TsfTextClient) {
    tsf_log!("facade: focus(hwnd={hwnd:#x})");
    let snapshot = client.snapshot();
    BACKEND.with(|b| {
        let mut slot = b.borrow_mut();
        if slot.is_none() {
            *slot = Some(make_backend());
        }
        if let Some(backend) = slot.as_mut() {
            backend.focus(hwnd, snapshot);
        }
    });
}

/// The focused input lost focus; end TSF tracking.
pub fn blur() {
    tsf_log!("facade: blur");
    with_backend(|backend| backend.blur());
}

/// Apply any IME edits queued since the last call, then refresh the cached text
/// state TSF reads from. Call from the app's control flow when it is safe to
/// touch the input (e.g. after dispatching a window message).
pub fn sync(client: &mut dyn TsfTextClient) {
    let edits = with_backend(|backend| backend.take_pending()).unwrap_or_default();
    if !edits.is_empty() {
        client.apply(&edits);
    }
    let snapshot = client.snapshot();
    with_backend(|backend| backend.set_snapshot(snapshot));
}

/// Tear down this thread's backend (e.g. on shutdown).
pub fn shutdown() {
    BACKEND.with(|b| {
        b.borrow_mut().take();
    });
}
