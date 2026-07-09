//! IME text-input integration for gpui windows — participates in the Windows
//! Text Services Framework so the OS input indicator (taskbar あ/A) tracks the
//! focused window and reflects its conversion mode.
//!
//! Why: gpui's Windows backend composes through IMM32, which inputs Japanese
//! fine but leaves the modern TSF-driven taskbar indicator unaware of our
//! windows (a field trace saw candidate/composition messages arrive but never
//! `IMN_SETCONVERSIONMODE` / `IMN_SETOPENSTATUS` — Win11's new IME keeps that
//! state in TSF). Making TSF track the window requires a focus document backed
//! by a real `ITextStoreACP` text store — an *empty* focus document silently
//! steals input. So this crate implements the store, and once TSF is engaged
//! the IME composes *into it*: the store turns that into [`ImeEvent`]s
//! (preedit updates and commits) which the application drains and applies —
//! for a terminal, preedit renders inline and commits go to the PTY.
//!
//! Verified on hardware (2026-07-09): with the store focused, the taskbar
//! indicator follows the window A→あ→A (e2e/tsf-indicator-test.ps1).
//!
//! The Windows COM lives behind a platform-neutral trait, so a Linux (IBus) or
//! macOS backend can be a later round, and the core here compiles everywhere.
//!
//! The Windows TSF text store is adapted from the arcweft project
//! (<https://github.com/Sanzentyo/arcweft>, dual Apache-2.0/MIT, used here under
//! MIT). See CREDITS.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// A snapshot of the focused input's editable text, selection and caret, in the
/// UTF-16 code-unit offsets shared by TSF's ACP and gpui's `InputHandler` (so no
/// conversion is needed between them). A terminal has no editable document and
/// supplies the default (empty) snapshot.
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

/// What the IME did, as far as the application needs to know. Drained in order
/// via [`drain_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// The live composition (preedit) text changed; replaces any previous
    /// preedit. Empty means the composition was cancelled/emptied.
    Preedit(String),
    /// Text was committed; deliver it to the application (a terminal writes it
    /// to the PTY). Any visible preedit should be cleared.
    Commit(String),
}

/// The application's editable text as the IME sees it at focus time. Called
/// only from the application's own control flow, never re-entrantly from
/// inside a TSF COM callback.
pub trait TsfTextClient {
    /// The focused input's current text, selection and caret rectangle.
    fn snapshot(&mut self) -> TextSnapshot;
}

/// Platform backend: keeps the OS IME/indicator in sync with the focused input.
trait Backend {
    /// The window `hwnd` gained focus with the given initial text state.
    fn focus(&mut self, hwnd: isize, snapshot: TextSnapshot);
    /// The focused input lost focus. Discards undrained events — they belong
    /// to the input that just went away.
    fn blur(&mut self);
    /// Remove and return the IME events queued since the last drain. Also
    /// performs any deferred document reset (safe here: no TSF lock is held in
    /// app control flow).
    fn take_events(&mut self) -> Vec<ImeEvent>;
}

/// No-op backend: platforms without an implementation, or when init fails.
struct NoopBackend;
impl Backend for NoopBackend {
    fn focus(&mut self, _hwnd: isize, _snapshot: TextSnapshot) {}
    fn blur(&mut self) {}
    fn take_events(&mut self) -> Vec<ImeEvent> {
        Vec::new()
    }
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

thread_local! {
    /// One backend per UI thread, built lazily on first focus. TSF objects are
    /// thread-affine and gpui's window procedure is single-threaded, so a
    /// thread-local keeps the call sites as free functions.
    static BACKEND: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
    /// Application wake-up hook, invoked (on this same UI thread) whenever new
    /// [`ImeEvent`]s are queued. The app schedules a re-render / drain from it;
    /// it must only *schedule* (e.g. onto an executor), not re-enter the UI.
    static WAKER: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
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

/// Invoke the app waker, if any. Called by the backend after queueing events —
/// possibly from inside a TSF callback, which is why wakers only schedule.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn wake() {
    let waker = WAKER.with(|w| w.borrow().clone());
    if let Some(waker) = waker {
        waker();
    }
}

/// Install the wake-up hook for this UI thread (see [`WAKER`]). The last
/// caller wins; installing at focus time keeps it pointing at the focused
/// window.
pub fn set_waker(waker: Box<dyn Fn()>) {
    WAKER.with(|w| *w.borrow_mut() = Some(Rc::from(waker)));
}

/// The window `hwnd` gained keyboard focus on an editable input; begin TSF
/// tracking so the OS indicator reflects its IME mode. Loads the initial text
/// state from `client`. Builds this thread's backend on first use. Pass
/// `hwnd = 0` to let the store answer `GetWnd` with the foreground window.
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

/// The focused input lost focus; end TSF tracking and drop undrained events.
pub fn blur() {
    tsf_log!("facade: blur");
    with_backend(|backend| backend.blur());
}

/// Remove and return the IME events queued since the last drain, oldest first.
/// Call from the app's own control flow (e.g. the render pass the waker
/// scheduled); the backend also uses this safe point to finish any deferred
/// document reset.
pub fn drain_events() -> Vec<ImeEvent> {
    with_backend(|backend| backend.take_events()).unwrap_or_default()
}

/// Tear down this thread's backend and waker (e.g. on shutdown).
pub fn shutdown() {
    BACKEND.with(|b| {
        b.borrow_mut().take();
    });
    WAKER.with(|w| {
        w.borrow_mut().take();
    });
}

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
