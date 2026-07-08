//! Windows Text Services Framework backend.
//!
//! Activates an `ITfThreadMgr` on the UI thread and, while an input is focused,
//! gives it a document whose context is backed by an `ITextStoreACP` text store
//! (this module). That is what makes TSF — and therefore the taskbar input
//! indicator — track our window and reflect its conversion mode. TSF reads the
//! text from a cached snapshot and queues edits; the app drains and applies
//! them via [`crate::TsfTextClient`] outside the COM callbacks.
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
    CLSID_TF_ThreadMgr, ITextStoreACP, ITextStoreACP_Impl, ITextStoreACPSink, ITfContext,
    ITfDocumentMgr, ITfThreadMgr, TEXT_STORE_LOCK_FLAGS, TF_E_DISCONNECTED, TS_AE_NONE, TS_ATTRVAL,
    TS_E_NOLAYOUT, TS_E_NOLOCK, TS_RT_PLAIN, TS_RUNINFO, TS_SELECTION_ACP, TS_STATUS, TS_TEXTCHANGE,
};
use windows::core::{
    BOOL, Error as WindowsError, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref,
    Result as WindowsResult, implement,
};

use crate::{CaretRect, TextEdit, TextSnapshot};

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
    sink: Option<ITextStoreACPSink>,
    text: Vec<u16>,
    selection: Range<usize>,
    caret: Option<CaretRect>,
    pending: Vec<TextEdit>,
}

type SharedHandle = Rc<RefCell<Shared>>;

impl Shared {
    fn new() -> Self {
        Self {
            hwnd: 0,
            destroyed: false,
            lock: Lock::None,
            sink: None,
            text: Vec::new(),
            selection: 0..0,
            caret: None,
            pending: Vec::new(),
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
    _apartment: CoApartment,
    thread_mgr: ITfThreadMgr,
    client_id: u32,
    document_mgr: ITfDocumentMgr,
    document: Option<ActiveDocument>,
    state: SharedHandle,
}

impl WindowsTsf {
    pub fn new() -> WindowsResult<Self> {
        let apartment = CoApartment::init()?;
        let thread_mgr: ITfThreadMgr =
            unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)? };
        let client_id = unsafe { thread_mgr.Activate()? };
        let document_mgr = unsafe { thread_mgr.CreateDocumentMgr()? };
        Ok(Self {
            _apartment: apartment,
            thread_mgr,
            client_id,
            document_mgr,
            document: None,
            state: Rc::new(RefCell::new(Shared::new())),
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
            state.text = snapshot.text;
            state.selection = clamp_range(snapshot.selection, state_len(&state.text));
            state.caret = snapshot.caret;
            state.pending.clear();
        }
        // A failed activation leaves us in the pre-focus (IMM32) state rather
        // than a half-focused one, so input keeps working.
        let _ = self.begin_focus();
    }

    fn blur(&mut self) {
        if self.document.take().is_some() {
            unsafe {
                // Idempotent: TSF errors if the stack is empty, which we ignore.
                let _ = self.document_mgr.Pop(0);
            }
        }
        let mut state = self.state.borrow_mut();
        state.sink = None;
        state.lock = Lock::None;
    }

    fn take_pending(&mut self) -> Vec<TextEdit> {
        self.state.borrow_mut().pending.drain(..).collect()
    }

    fn set_snapshot(&mut self, snapshot: TextSnapshot) {
        let mut state = self.state.borrow_mut();
        let len = snapshot.text.len();
        state.text = snapshot.text;
        state.selection = clamp_range(snapshot.selection, len);
        state.caret = snapshot.caret;
    }
}

fn state_len(text: &[u16]) -> usize {
    text.len()
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
            state.pending.clear();
        }
        <Self as crate::Backend>::blur(self);
        unsafe {
            let _ = self.thread_mgr.Deactivate();
        }
    }
}

// ── the ITextStoreACP text store ───────────────────────────────────────────

#[implement(ITextStoreACP)]
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
        let sink = {
            let mut state = self.state()?;
            // Bit 0x4 = TS_LF_WRITE (TS_LF_READWRITE = READ | WRITE).
            state.lock = if dwlockflags & 0x4 != 0 {
                Lock::ReadWrite
            } else {
                Lock::Read
            };
            state.sink.clone()
        };
        let result = if let Some(sink) = sink {
            unsafe { sink.OnLockGranted(TEXT_STORE_LOCK_FLAGS(dwlockflags)) }
        } else {
            Err(WindowsError::from(TF_E_DISCONNECTED))
        };
        self.state()?.lock = Lock::None;
        Ok(result.map_or_else(|error| error.code(), |()| HRESULT(0)))
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
        state.pending.push(TextEdit::SetSelection { start, end });
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
            *pcchPlainRet = to_i32(take) as u32;
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
        state.pending.push(TextEdit::Replace {
            start,
            end,
            text: text.clone(),
        });
        // Mirror into the cached snapshot so subsequent reads in the same lock
        // see the edit.
        state.text.splice(start..end, text);
        let caret = start + new_len;
        state.selection = caret..caret;
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
        let caret = state.caret.ok_or_else(|| WindowsError::from(TS_E_NOLAYOUT))?;
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
        Ok(HWND(self.state_ref()?.hwnd as *mut core::ffi::c_void))
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
