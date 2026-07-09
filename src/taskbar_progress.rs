//! Windows taskbar-button progress (OSC 9;4 Phase 2).
//!
//! Mirrors a window's terminal progress onto its own taskbar button via
//! `ITaskbarList3`, so an agent's activity is visible at a glance even when
//! the window is minimized or behind. The HWND is resolved by (our PID, exact
//! window title) through `EnumWindows` — no gpui accessor needed — and cached
//! until it stops answering or the title changes. Calls are deduplicated, so
//! the render-loop call sites cost nothing while the state is stable.
//! Non-Windows builds are a no-op.

use crate::terminal::progress::ProgressState;

/// Reflect `progress` on the taskbar button of the window titled `title`
/// (`None` clears the overlay). Call from the owning window's render pass.
pub fn update(title: &str, progress: Option<(ProgressState, u8)>) {
    imp::update(title, progress);
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;

    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
        TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    };
    use windows::core::BOOL;

    use super::ProgressState;

    struct Entry {
        hwnd: isize,
        last: Option<Option<(ProgressState, u8)>>,
    }

    struct Inner {
        /// `None` = init failed once; stay silent for the session.
        taskbar: Option<ITaskbarList3>,
        /// title → resolved HWND + last state, one entry per calling window
        /// (main and shell both call every render; separate entries keep the
        /// dedup effective instead of ping-ponging).
        windows: std::collections::HashMap<String, Entry>,
    }

    thread_local! {
        static INNER: RefCell<Option<Inner>> = const { RefCell::new(None) };
    }

    pub(super) fn update(title: &str, progress: Option<(ProgressState, u8)>) {
        INNER.with(|cell| {
            let mut slot = cell.borrow_mut();
            let inner = slot.get_or_insert_with(|| Inner {
                taskbar: init_taskbar(),
                windows: std::collections::HashMap::new(),
            });
            let Some(taskbar) = inner.taskbar.as_ref() else {
                return;
            };
            // Resolve (and cache) our window by exact title; re-resolve if the
            // cached HWND stopped answering (window closed / title moved on).
            let cached_ok = matches!(
                inner.windows.get(title),
                Some(e) if unsafe { IsWindow(Some(HWND(e.hwnd as _))).as_bool() }
            );
            if !cached_ok {
                inner.windows.remove(title);
                if let Some(h) = find_window_by_title(title) {
                    inner.windows.insert(title.to_string(), Entry { hwnd: h, last: None });
                }
            }
            let Some(entry) = inner.windows.get_mut(title) else {
                return;
            };
            if entry.last == Some(progress) {
                return;
            }
            let hwnd = HWND(entry.hwnd as _);
            unsafe {
                match progress {
                    None => {
                        let _ = taskbar.SetProgressState(hwnd, TBPF_NOPROGRESS);
                    }
                    Some((state, percent)) => {
                        let flag = match state {
                            ProgressState::Normal => TBPF_NORMAL,
                            ProgressState::Error => TBPF_ERROR,
                            ProgressState::Warning => TBPF_PAUSED,
                            ProgressState::Indeterminate => TBPF_INDETERMINATE,
                        };
                        let _ = taskbar.SetProgressState(hwnd, flag);
                        if state != ProgressState::Indeterminate {
                            let _ =
                                taskbar.SetProgressValue(hwnd, u64::from(percent).min(100), 100);
                        }
                    }
                }
            }
            entry.last = Some(progress);
        });
    }

    fn init_taskbar() -> Option<ITaskbarList3> {
        unsafe {
            let taskbar: ITaskbarList3 =
                CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()?;
            taskbar.HrInit().ok()?;
            Some(taskbar)
        }
    }

    /// First visible top-level window of this process whose title matches
    /// exactly.
    fn find_window_by_title(title: &str) -> Option<isize> {
        struct Search {
            pid: u32,
            want: Vec<u16>,
            found: Option<isize>,
        }
        unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != search.pid || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                return BOOL(1);
            }
            let mut buf = [0u16; 256];
            let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
            if buf[..len] == search.want[..] {
                search.found = Some(hwnd.0 as isize);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut search = Search {
            pid: unsafe { GetCurrentProcessId() },
            want: title.encode_utf16().collect(),
            found: None,
        };
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&raw mut search as isize));
        }
        search.found
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ProgressState;

    pub(super) fn update(_title: &str, _progress: Option<(ProgressState, u8)>) {}
}
