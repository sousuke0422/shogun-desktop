//! Windows Text Services Framework backend.
//!
//! Activates an `ITfThreadMgr` on the UI thread and, while an input is focused,
//! gives it a document whose context is backed by an `ITextStoreACP` text store
//! (this module). That is what makes TSF — and therefore the taskbar input
//! indicator — track our window (verified on hardware: A→あ→A). Once TSF is
//! engaged the IME composes *into the store*: the same COM object also
//! implements `ITfContextOwnerCompositionSink`, so composition boundaries are
//! known and the store synthesizes [`ImeEvent`]s — `Preedit` while composing,
//! `Commit` when the composition ends (or on direct insertion). The app drains
//! them via [`crate::drain_events`]; the drain is also where the document is
//! reset to empty after a commit (safe: no TSF lock is held in app control
//! flow, and the reset is announced through `OnTextChange`).
//!
//! Adapted from the arcweft project (dual Apache-2.0/MIT; used under MIT — see
//! CREDITS). Offsets are UTF-16 code units throughout, matching both TSF's ACP
//! and gpui's `InputHandler`.
#![allow(non_snake_case, clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ops::Range;
use std::ptr;
use std::rc::Rc;

use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, FORMATETC, IDataObject,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_ThreadMgr, ITextStoreACP, ITextStoreACP_Impl, ITextStoreACPSink, ITfCompositionView,
    ITfContext, ITfContextOwnerCompositionSink, ITfContextOwnerCompositionSink_Impl,
    ITfDocumentMgr, ITfRange, ITfThreadMgr, TEXT_STORE_LOCK_FLAGS, TEXT_STORE_TEXT_CHANGE_FLAGS,
    TF_E_DISCONNECTED, TS_AE_NONE, TS_ATTRVAL, TS_E_NOLAYOUT, TS_E_NOLOCK, TS_E_SYNCHRONOUS,
    TS_LF_SYNC, TS_RT_PLAIN, TS_RUNINFO, TS_S_ASYNC, TS_SELECTION_ACP, TS_STATUS, TS_TEXTCHANGE,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{
    BOOL, Error as WindowsError, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref,
    Result as WindowsResult, implement,
};

use crate::{CaretRect, ImeEvent, TextSnapshot};

// ── shared text-store state ────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lock {
    None,
    Read,
    ReadWrite,
}

/// Text state shared between the COM text store (which TSF calls) and the owning
/// [`WindowsTsf`]. `Rc<RefCell<..>>` because both live on the one UI thread.
struct Shared {
    hwnd: isize,
    destroyed: bool,
    lock: Lock,
    /// An async lock (upgrade) requested from inside `OnLockGranted`; granted
    /// after the current grant returns.
    pending_lock: Option<u32>,
    sink: Option<ITextStoreACPSink>,
    text: Vec<u16>,
    selection: Range<usize>,
    caret: Option<CaretRect>,
    /// Inside a composition (between OnStartComposition and OnEndComposition).
    composing: bool,
    /// Events for the app, drained by [`crate::drain_events`].
    events: Vec<ImeEvent>,
    /// A commit happened; the document must be reset to empty at the next
    /// drain (outside any TSF lock), announced via `OnTextChange`.
    needs_reset: bool,
}

type SharedHandle = Rc<RefCell<Shared>>;

impl Shared {
    fn new() -> Self {
        Self {
            hwnd: 0,
            destroyed: false,
            lock: Lock::None,
            pending_lock: None,
            sink: None,
            text: Vec::new(),
            selection: 0..0,
            caret: None,
            composing: false,
            events: Vec::new(),
            needs_reset: false,
        }
    }

    fn require_read(&self) -> WindowsResult<()> {
        if matches!(self.lock, Lock::Read | Lock::ReadWrite) {
            Ok(())
        } else {
            Err(WindowsError::from(TS_E_NOLOCK))
        }
    }

    fn require_write(&self) -> WindowsResult<()> {
        if self.lock == Lock::ReadWrite {
            Ok(())
        } else {
            Err(WindowsError::from(TS_E_NOLOCK))
        }
    }

    fn len(&self) -> usize {
        self.text.len()
    }

    /// Queue a preedit update, coalescing consecutive preedits (each keystroke
    /// otherwise queues a full copy).
    fn push_preedit(&mut self) {
        let s = String::from_utf16_lossy(&self.text);
        if let Some(ImeEvent::Preedit(last)) = self.events.last_mut() {
            *last = s;
        } else {
            self.events.push(ImeEvent::Preedit(s));
        }
    }
}

/// Map a TSF ACP offset (`-1` = end of document) to a clamped UTF-16 index.
fn acp_to_off(acp: i32, len: usize) -> usize {
    if acp < 0 {
        len
    } else {
        (acp as usize).min(len)
    }
}

fn to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

// ── COM apartment RAII ─────────────────────────────────────────────────────

struct CoApartment;

impl CoApartment {
    fn init() -> WindowsResult<Self> {
        unsafe {
            // gpui already initialises COM (STA) for OLE drag-and-drop; this
            // returns S_FALSE ("already initialised") which `.ok()` accepts. The
            // matching CoUninitialize in Drop just balances the ref count.
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for CoApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// ── thread context (the Backend) ───────────────────────────────────────────

struct ActiveDocument {
    // Retained for the lifetime of the focus so the pushed context stays alive;
    // TSF callbacks use the shared state, not these handles directly.
    _context: ITfContext,
    _store: ITextStoreACP,
}

pub struct WindowsTsf {
    thread_mgr: ITfThreadMgr,
    client_id: u32,
    document_mgr: ITfDocumentMgr,
    document: Option<ActiveDocument>,
    state: SharedHandle,
    /// Declared last: struct fields drop in declaration order, and the COM
    /// interface releases above must happen before CoUninitialize.
    _apartment: CoApartment,
}

impl WindowsTsf {
    pub fn new() -> WindowsResult<Self> {
        let apartment = CoApartment::init()?;
        let thread_mgr: ITfThreadMgr =
            unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)? };
        let client_id = unsafe { thread_mgr.Activate()? };
        let document_mgr = unsafe { thread_mgr.CreateDocumentMgr()? };
        Ok(Self {
            thread_mgr,
            client_id,
            document_mgr,
            document: None,
            state: Rc::new(RefCell::new(Shared::new())),
            _apartment: apartment,
        })
    }

    fn begin_focus(&mut self) -> WindowsResult<()> {
        let store: ITextStoreACP = TextStore {
            state: Rc::clone(&self.state),
        }
        .into();
        let mut context: Option<ITfContext> = None;
        let mut edit_cookie = 0_u32;
        unsafe {
            self.document_mgr.CreateContext(
                self.client_id,
                0,
                &store,
                &mut context,
                &mut edit_cookie,
            )?;
        }
        let context = context.ok_or_else(|| WindowsError::from(E_FAIL))?;
        unsafe {
            self.document_mgr.Push(&context)?;
            self.thread_mgr.SetFocus(&self.document_mgr)?;
        }
        self.document = Some(ActiveDocument {
            _context: context,
            _store: store,
        });
        Ok(())
    }
}

impl crate::Backend for WindowsTsf {
    fn focus(&mut self, hwnd: isize, snapshot: TextSnapshot) {
        self.blur();
        {
            let mut state = self.state.borrow_mut();
            state.hwnd = hwnd;
            state.destroyed = false;
            let len = snapshot.text.len();
            state.text = snapshot.text;
            state.selection = clamp_range(snapshot.selection, len);
            state.caret = snapshot.caret;
            state.composing = false;
            state.events.clear();
            state.needs_reset = false;
        }
        // A failed activation leaves us in the pre-focus (IMM32) state rather
        // than a half-focused one, so input keeps working.
        match self.begin_focus() {
            Ok(()) => crate::tsf_log!("tsf: document focused (create/push/SetFocus ok)"),
            Err(e) => crate::tsf_log!("tsf: begin_focus FAILED ({e})"),
        }
    }

    fn blur(&mut self) {
        if self.document.take().is_some() {
            crate::tsf_log!("tsf: blur (popping document)");
            unsafe {
                // Idempotent: TSF errors if the stack is empty, which we ignore.
                let _ = self.document_mgr.Pop(0);
            }
        }
        let mut state = self.state.borrow_mut();
        state.sink = None;
        state.lock = Lock::None;
        state.pending_lock = None;
        state.composing = false;
        // Undrained events belong to the input that just went away — a commit
        // delivered to whatever gets focus next would go to the wrong PTY.
        state.events.clear();
        state.needs_reset = false;
    }

    fn take_events(&mut self) -> Vec<ImeEvent> {
        // Take the queue and finish any deferred document reset while no TSF
        // lock can be active (we are in app control flow).
        let (events, reset_len, sink) = {
            let mut state = self.state.borrow_mut();
            if state.lock != Lock::None {
                // Paranoia: never reset mid-lock; try again next drain.
                return std::mem::take(&mut state.events);
            }
            let events = std::mem::take(&mut state.events);
            let mut reset_len = 0usize;
            if state.needs_reset {
                reset_len = state.len();
                state.text.clear();
                state.selection = 0..0;
                state.needs_reset = false;
            }
            (events, reset_len, state.sink.clone())
        };
        if reset_len > 0
            && let Some(sink) = sink
        {
            let change = TS_TEXTCHANGE {
                acpStart: 0,
                acpOldEnd: to_i32(reset_len),
                acpNewEnd: 0,
            };
            unsafe {
                let _ = sink.OnTextChange(TEXT_STORE_TEXT_CHANGE_FLAGS(0), &change);
                let _ = sink.OnSelectionChange();
            }
            crate::tsf_log!("tsf: document reset after commit ({reset_len} u16 cleared)");
        }
        if !events.is_empty() {
            crate::tsf_log!("tsf: drained {} event(s)", events.len());
        }
        events
    }
}

fn clamp_range(r: Range<usize>, len: usize) -> Range<usize> {
    let start = r.start.min(len);
    let end = r.end.min(len);
    start.min(end)..start.max(end)
}

impl Drop for WindowsTsf {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.destroyed = true;
            state.sink = None;
            state.events.clear();
        }
        <Self as crate::Backend>::blur(self);
        unsafe {
            let _ = self.thread_mgr.Deactivate();
        }
    }
}

// ── the ITextStoreACP text store + composition sink ────────────────────────

#[implement(ITextStoreACP, ITfContextOwnerCompositionSink)]
struct TextStore {
    state: SharedHandle,
}

impl TextStore {
    fn state(&self) -> WindowsResult<std::cell::RefMut<'_, Shared>> {
        self.state
            .try_borrow_mut()
            .map_err(|_| WindowsError::from(E_FAIL))
    }

    fn state_ref(&self) -> WindowsResult<std::cell::Ref<'_, Shared>> {
        self.state
            .try_borrow()
            .map_err(|_| WindowsError::from(E_FAIL))
    }
}

#[allow(non_snake_case)]
impl ITfContextOwnerCompositionSink_Impl for TextStore_Impl {
    fn OnStartComposition(
        &self,
        _pcomposition: Ref<'_, ITfCompositionView>,
    ) -> WindowsResult<BOOL> {
        crate::tsf_log!("store: OnStartComposition");
        self.state()?.composing = true;
        // TRUE = allow the composition.
        Ok(BOOL(1))
    }

    fn OnUpdateComposition(
        &self,
        _pcomposition: Ref<'_, ITfCompositionView>,
        _prangenew: Ref<'_, ITfRange>,
    ) -> WindowsResult<()> {
        // The text itself arrives through SetText; nothing to do here.
        Ok(())
    }

    fn OnEndComposition(&self, _pcomposition: Ref<'_, ITfCompositionView>) -> WindowsResult<()> {
        let mut state = self.state()?;
        state.composing = false;
        let text = String::from_utf16_lossy(&state.text);
        crate::tsf_log!("store: OnEndComposition ({} u16)", state.text.len());
        if text.is_empty() {
            // Cancelled composition: just clear any preedit the app shows.
            state.push_preedit();
        } else {
            // With the document reset after every commit, the whole document
            // is exactly the finished composition.
            state.events.push(ImeEvent::Commit(text));
            state.needs_reset = true;
        }
        drop(state);
        crate::wake();
        Ok(())
    }
}

#[allow(non_snake_case)]
impl ITextStoreACP_Impl for TextStore_Impl {
    fn AdviseSink(
        &self,
        riid: *const GUID,
        punk: Ref<'_, IUnknown>,
        _dwmask: u32,
    ) -> WindowsResult<()> {
        if riid.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let Some(punk) = punk.as_ref() else {
            return Err(WindowsError::from(E_INVALIDARG));
        };
        let sink: ITextStoreACPSink = punk.cast()?;
        self.state()?.sink = Some(sink);
        crate::tsf_log!("store: AdviseSink");
        Ok(())
    }

    fn UnadviseSink(&self, punk: Ref<'_, IUnknown>) -> WindowsResult<()> {
        if punk.as_ref().is_none() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        self.state()?.sink = None;
        Ok(())
    }

    fn RequestLock(&self, dwlockflags: u32) -> WindowsResult<HRESULT> {
        crate::tsf_log!("store: RequestLock(flags={dwlockflags:#x})");
        {
            let mut state = self.state()?;
            if state.lock != Lock::None {
                // Re-entrant request from inside OnLockGranted (an upgrade).
                return if dwlockflags & TS_LF_SYNC != 0 {
                    // A synchronous lock cannot be granted while one is held.
                    Ok(TS_E_SYNCHRONOUS)
                } else {
                    state.pending_lock = Some(dwlockflags);
                    Ok(TS_S_ASYNC)
                };
            }
        }
        let mut flags = dwlockflags;
        let mut first_result: Option<HRESULT> = None;
        loop {
            let sink = {
                let mut state = self.state()?;
                // Bit 0x4 = TS_LF_WRITE (TS_LF_READWRITE = READ | WRITE).
                state.lock = if flags & 0x4 != 0 {
                    Lock::ReadWrite
                } else {
                    Lock::Read
                };
                state.sink.clone()
            };
            let result = if let Some(sink) = sink {
                unsafe { sink.OnLockGranted(TEXT_STORE_LOCK_FLAGS(flags)) }
            } else {
                Err(WindowsError::from(TF_E_DISCONNECTED))
            };
            let hr = result.map_or_else(|error| error.code(), |()| HRESULT(0));
            if first_result.is_none() {
                first_result = Some(hr);
            }
            let next = {
                let mut state = self.state()?;
                state.lock = Lock::None;
                state.pending_lock.take()
            };
            match next {
                Some(f) => flags = f, // grant the queued upgrade now
                None => return Ok(first_result.unwrap_or(HRESULT(0))),
            }
        }
    }

    fn GetStatus(&self) -> WindowsResult<TS_STATUS> {
        Ok(TS_STATUS {
            dwDynamicFlags: 0,
            dwStaticFlags: 0,
        })
    }

    fn QueryInsert(
        &self,
        acpTestStart: i32,
        acpTestEnd: i32,
        cch: u32,
        pacpResultStart: *mut i32,
        pacpResultEnd: *mut i32,
    ) -> WindowsResult<()> {
        if pacpResultStart.is_null() || pacpResultEnd.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let len = self.state_ref()?.len();
        let start = acp_to_off(acpTestStart, len);
        let _end = acp_to_off(acpTestEnd, len);
        unsafe {
            *pacpResultStart = to_i32(start);
            *pacpResultEnd = to_i32(start + cch as usize);
        }
        Ok(())
    }

    fn GetSelection(
        &self,
        _ulIndex: u32,
        _ulCount: u32,
        pSelection: *mut TS_SELECTION_ACP,
        pcFetched: *mut u32,
    ) -> WindowsResult<()> {
        if pSelection.is_null() || pcFetched.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let state = self.state_ref()?;
        state.require_read()?;
        unsafe {
            (*pSelection).acpStart = to_i32(state.selection.start);
            (*pSelection).acpEnd = to_i32(state.selection.end);
            (*pSelection).style.ase = TS_AE_NONE;
            (*pSelection).style.fInterimChar = BOOL(0);
            *pcFetched = 1;
        }
        Ok(())
    }

    fn SetSelection(&self, ulCount: u32, pSelection: *const TS_SELECTION_ACP) -> WindowsResult<()> {
        if ulCount != 1 || pSelection.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let mut state = self.state()?;
        state.require_write()?;
        let native = unsafe { *pSelection };
        let len = state.len();
        let start = acp_to_off(native.acpStart, len);
        let end = acp_to_off(native.acpEnd, len);
        state.selection = start.min(end)..start.max(end);
        Ok(())
    }

    fn GetText(
        &self,
        acpStart: i32,
        acpEnd: i32,
        pchPlain: PWSTR,
        cchPlainReq: u32,
        pcchPlainRet: *mut u32,
        prgRunInfo: *mut TS_RUNINFO,
        ulRunInfoReq: u32,
        pulRunInfoOut: *mut u32,
        pacpNext: *mut i32,
    ) -> WindowsResult<()> {
        if pcchPlainRet.is_null() || pulRunInfoOut.is_null() || pacpNext.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let state = self.state_ref()?;
        state.require_read()?;
        let len = state.len();
        let start = acp_to_off(acpStart, len);
        let end = acp_to_off(acpEnd, len).max(start);
        let available = &state.text[start..end];
        let take = available.len().min(cchPlainReq as usize);
        if !pchPlain.is_null() && take > 0 {
            unsafe {
                ptr::copy_nonoverlapping(available.as_ptr(), pchPlain.0, take);
            }
        }
        unsafe {
            *pcchPlainRet = take as u32;
            *pulRunInfoOut = 0;
            *pacpNext = to_i32(start + take);
            if !prgRunInfo.is_null() && ulRunInfoReq > 0 && take > 0 {
                (*prgRunInfo).uCount = take as u32;
                (*prgRunInfo).r#type = TS_RT_PLAIN;
                *pulRunInfoOut = 1;
            }
        }
        Ok(())
    }

    fn SetText(
        &self,
        _dwFlags: u32,
        acpStart: i32,
        acpEnd: i32,
        pchText: &PCWSTR,
        cch: u32,
    ) -> WindowsResult<TS_TEXTCHANGE> {
        let mut state = self.state()?;
        state.require_write()?;
        let len = state.len();
        let start = acp_to_off(acpStart, len);
        let end = acp_to_off(acpEnd, len).max(start);
        let text: Vec<u16> = if pchText.0.is_null() || cch == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(pchText.0, cch as usize) }.to_vec()
        };
        let new_len = text.len();
        crate::tsf_log!("store: SetText({start}..{end}, {new_len} u16)");
        state.text.splice(start..end, text.iter().copied());
        let caret = start + new_len;
        state.selection = caret..caret;
        if state.composing {
            // Live preedit update; the commit comes at OnEndComposition.
            state.push_preedit();
        } else if new_len > 0 {
            // Direct insertion without a composition (e.g. some TIPs commit
            // punctuation or reconversion results straight in).
            let committed = String::from_utf16_lossy(&state.text[start..start + new_len]);
            state.events.push(ImeEvent::Commit(committed));
            state.needs_reset = true;
        }
        drop(state);
        crate::wake();
        Ok(TS_TEXTCHANGE {
            acpStart: to_i32(start),
            acpOldEnd: to_i32(end),
            acpNewEnd: to_i32(start + new_len),
        })
    }

    fn GetEndACP(&self) -> WindowsResult<i32> {
        let state = self.state_ref()?;
        state.require_read()?;
        Ok(to_i32(state.len()))
    }

    fn GetActiveView(&self) -> WindowsResult<u32> {
        Ok(0)
    }

    fn GetTextExt(
        &self,
        _vcView: u32,
        _acpStart: i32,
        _acpEnd: i32,
        prc: *mut RECT,
        pfClipped: *mut BOOL,
    ) -> WindowsResult<()> {
        if prc.is_null() || pfClipped.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let state = self.state_ref()?;
        state.require_read()?;
        let caret = state.caret.ok_or_else(|| {
            crate::tsf_log!("store: GetTextExt -> TS_E_NOLAYOUT (no caret rect)");
            WindowsError::from(TS_E_NOLAYOUT)
        })?;
        unsafe {
            *prc = rect_of(caret);
            *pfClipped = BOOL(0);
        }
        Ok(())
    }

    fn GetScreenExt(&self, _vcView: u32) -> WindowsResult<RECT> {
        let state = self.state_ref()?;
        let caret = state.caret.ok_or_else(|| WindowsError::from(TS_E_NOLAYOUT))?;
        Ok(rect_of(caret))
    }

    fn GetWnd(&self, _vcView: u32) -> WindowsResult<HWND> {
        let hwnd = self.state_ref()?.hwnd;
        // 0 = "use the foreground window" — lets the app drive focus without
        // plumbing a raw HWND out of gpui (the focused window is foreground).
        let resolved = if hwnd != 0 {
            HWND(hwnd as *mut core::ffi::c_void)
        } else {
            unsafe { GetForegroundWindow() }
        };
        crate::tsf_log!("store: GetWnd -> {:#x}", resolved.0 as isize);
        Ok(resolved)
    }

    fn GetFormattedText(&self, _acpStart: i32, _acpEnd: i32) -> WindowsResult<IDataObject> {
        Err(WindowsError::from(E_NOTIMPL))
    }

    fn GetEmbedded(
        &self,
        _acpPos: i32,
        _rguidService: *const GUID,
        _riid: *const GUID,
    ) -> WindowsResult<IUnknown> {
        Err(WindowsError::from(E_NOTIMPL))
    }

    fn QueryInsertEmbedded(
        &self,
        _pguidService: *const GUID,
        _pFormatEtc: *const FORMATETC,
    ) -> WindowsResult<BOOL> {
        Ok(BOOL(0))
    }

    fn InsertEmbedded(
        &self,
        _dwFlags: u32,
        _acpStart: i32,
        _acpEnd: i32,
        _pDataObject: Ref<'_, IDataObject>,
    ) -> WindowsResult<TS_TEXTCHANGE> {
        Err(WindowsError::from(E_NOTIMPL))
    }

    fn InsertTextAtSelection(
        &self,
        dwFlags: u32,
        pchText: &PCWSTR,
        cch: u32,
        pacpStart: *mut i32,
        pacpEnd: *mut i32,
        pChange: *mut TS_TEXTCHANGE,
    ) -> WindowsResult<()> {
        if pacpStart.is_null() || pacpEnd.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        let acp = to_i32(self.state_ref()?.selection.start);
        let change = self.SetText(dwFlags, acp, acp, pchText, cch)?;
        unsafe {
            *pacpStart = change.acpStart;
            *pacpEnd = change.acpNewEnd;
            if !pChange.is_null() {
                *pChange = change;
            }
        }
        Ok(())
    }

    fn InsertEmbeddedAtSelection(
        &self,
        _dwFlags: u32,
        _pDataObject: Ref<'_, IDataObject>,
        _pacpStart: *mut i32,
        _pacpEnd: *mut i32,
        _pChange: *mut TS_TEXTCHANGE,
    ) -> WindowsResult<()> {
        Err(WindowsError::from(E_NOTIMPL))
    }

    fn RequestSupportedAttrs(
        &self,
        _dwFlags: u32,
        _cFilterAttrs: u32,
        _paFilterAttrs: *const GUID,
    ) -> WindowsResult<()> {
        Ok(())
    }

    fn RequestAttrsAtPosition(
        &self,
        _acpPos: i32,
        _cFilterAttrs: u32,
        _paFilterAttrs: *const GUID,
        _dwFlags: u32,
    ) -> WindowsResult<()> {
        Ok(())
    }

    fn RequestAttrsTransitioningAtPosition(
        &self,
        _acpPos: i32,
        _cFilterAttrs: u32,
        _paFilterAttrs: *const GUID,
        _dwFlags: u32,
    ) -> WindowsResult<()> {
        Ok(())
    }

    fn FindNextAttrTransition(
        &self,
        _acpStart: i32,
        acpHalt: i32,
        _cFilterAttrs: u32,
        _paFilterAttrs: *const GUID,
        _dwFlags: u32,
        pacpNext: *mut i32,
        pfFound: *mut BOOL,
        _plFoundOffset: *mut i32,
    ) -> WindowsResult<()> {
        if pacpNext.is_null() || pfFound.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        unsafe {
            *pacpNext = acpHalt;
            *pfFound = BOOL(0);
        }
        Ok(())
    }

    fn RetrieveRequestedAttrs(
        &self,
        _ulCount: u32,
        _paAttrVals: *mut TS_ATTRVAL,
        pcFetched: *mut u32,
    ) -> WindowsResult<()> {
        if pcFetched.is_null() {
            return Err(WindowsError::from(E_INVALIDARG));
        }
        unsafe {
            *pcFetched = 0;
        }
        Ok(())
    }

    fn GetACPFromPoint(
        &self,
        _vcView: u32,
        _pt: *const POINT,
        _dwFlags: u32,
    ) -> WindowsResult<i32> {
        Ok(0)
    }
}

fn rect_of(c: CaretRect) -> RECT {
    RECT {
        left: c.left,
        top: c.top,
        right: c.right,
        bottom: c.bottom,
    }
}

/// End-to-end COM plumbing check (see [`crate::self_check`]): runs the whole
/// activate → create/push/SetFocus → blur → teardown cycle on a throwaway
/// instance and reports each step. Headless-safe — it only touches this
/// process's thread-local TSF state.
pub(crate) fn self_check() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("tsf self-check (windows)\n");
    let mut tsf = match WindowsTsf::new() {
        Ok(t) => {
            let _ = writeln!(out, "  activate thread mgr: ok (client_id={})", t.client_id);
            t
        }
        Err(e) => {
            let _ = writeln!(out, "  activate thread mgr: FAILED ({e})");
            return out;
        }
    };
    match tsf.begin_focus() {
        Ok(()) => {
            let _ = writeln!(out, "  create/push/SetFocus document: ok");
        }
        Err(e) => {
            let _ = writeln!(out, "  create/push/SetFocus document: FAILED ({e})");
            return out;
        }
    }
    let sink_advised = tsf.state.borrow().sink.is_some();
    let _ = writeln!(
        out,
        "  sink advised by TSF: {}",
        if sink_advised { "yes" } else { "not yet (may be lazy)" }
    );
    <WindowsTsf as crate::Backend>::blur(&mut tsf);
    let _ = writeln!(out, "  blur/pop: ok");
    drop(tsf);
    let _ = writeln!(out, "  deactivate/teardown: ok");
    out
}
