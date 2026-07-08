//! Windows Text Services Framework backend.
//!
//! Activates an `ITfThreadMgr` on the (single) UI thread and gives each gpui
//! window its own focus document via `AssociateFocus`, so TSF — which drives
//! the Windows 10/11 taskbar input indicator — tracks our windows and reflects
//! the conversion mode. Composition itself still runs through gpui's IMM32
//! path; this supplies only the TSF focus participation IMM32 alone lacks.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::TextServices::{CLSID_TF_ThreadMgr, ITfDocumentMgr, ITfThreadMgr};
use windows::core::Result;

use crate::ImeThreadIntegration;

pub struct WindowsTsfIme {
    thread_mgr: ITfThreadMgr,
    _client_id: u32,
    /// Each window's focus document, kept alive for the window's lifetime.
    docs: Vec<(isize, ITfDocumentMgr)>,
}

impl WindowsTsfIme {
    pub fn new() -> Result<Self> {
        unsafe {
            // gpui initialises COM (STA) on the UI thread for OLE drag-and-drop,
            // so the thread manager is created without our own CoInitialize.
            let thread_mgr: ITfThreadMgr =
                CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)?;
            let client_id = thread_mgr.Activate()?;
            Ok(Self {
                thread_mgr,
                _client_id: client_id,
                docs: Vec::new(),
            })
        }
    }

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }
}

impl ImeThreadIntegration for WindowsTsfIme {
    fn associate_window(&mut self, hwnd: isize) {
        if self.docs.iter().any(|(h, _)| *h == hwnd) {
            return;
        }
        unsafe {
            let Ok(doc) = self.thread_mgr.CreateDocumentMgr() else {
                return;
            };
            // Associate the window with its (empty) focus document. TSF now
            // focuses this document whenever the window is focused, which is
            // what makes the taskbar indicator track our window's IME mode.
            let _prev = self.thread_mgr.AssociateFocus(Self::hwnd(hwnd), &doc);
            self.docs.push((hwnd, doc));
        }
    }

    fn dissociate_window(&mut self, hwnd: isize) {
        if let Some(pos) = self.docs.iter().position(|(h, _)| *h == hwnd) {
            unsafe {
                // Clear the association; the document is dropped with the Vec entry.
                let _ = self.thread_mgr.AssociateFocus(Self::hwnd(hwnd), None);
            }
            self.docs.remove(pos);
        }
    }
}

impl Drop for WindowsTsfIme {
    fn drop(&mut self) {
        unsafe {
            let _ = self.thread_mgr.Deactivate();
        }
    }
}
