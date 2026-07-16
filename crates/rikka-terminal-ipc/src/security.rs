//! Listener hardening — the OS-level access boundary for the local IPC.
//!
//! The socket NAME is a rendezvous, never a permission check: any local
//! process that knows the name can attempt to connect (the per-user prefix
//! only avoids collisions — it is trivially spoofable). The real boundary
//! lives here. [`owner_only`] restricts a listener so only the current
//! user's processes can connect; the consumers additionally bind every
//! handle transfer to the verified peer PID (see `Conn::peer_pid` and
//! `attach::pull_attach`), so even a same-user process cannot make us pull
//! handles out of a third victim.
//!
//! Only the Windows path is implemented today, on purpose behind a seam:
//! adding macOS/Linux is one more arm in [`owner_only`], not edits
//! scattered through `transport.rs`. That is the wider-scope work the
//! abstraction exists to keep cheap.

use std::io;

use interprocess::local_socket::ListenerOptions;

/// Restrict `opts` so only the current user's processes can connect.
///
/// Windows: a DACL granting full access to just the current user's SID —
/// the default named-pipe DACL is configuration-dependent and can admit
/// other users, so we set it ourselves rather than trust it. Other
/// platforms: unchanged for now. The abstract-namespace socket we use on
/// Linux is reachable by any process in the network namespace, so a real
/// Unix deployment must switch to a filesystem socket at mode 0600 — that
/// work belongs in THIS function, keeping the platform seam in one place.
pub fn owner_only(opts: ListenerOptions) -> io::Result<ListenerOptions> {
    #[cfg(windows)]
    {
        windows_impl::owner_only(opts)
    }
    #[cfg(not(windows))]
    {
        Ok(opts)
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::io;

    use interprocess::local_socket::ListenerOptions;
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PWSTR;

    pub fn owner_only(opts: ListenerOptions) -> io::Result<ListenerOptions> {
        let sid = current_user_sid_string()?;
        // D:P — a DACL, Protected (blocks inherited ACEs). One ACE: Allow
        // (A) Full Access (FA) to the current user's SID. No other grantee,
        // so cross-user and lower-integrity opens are denied at the pipe.
        let sddl = format!("D:P(A;;FA;;;{sid})");
        let wide = U16CString::from_str(&sddl)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SDDL had an interior NUL"))?;
        let sd = SecurityDescriptor::deserialize(wide.as_ucstr()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("build security descriptor: {e}"),
            )
        })?;
        Ok(opts.security_descriptor(sd))
    }

    /// Closes the token handle on every return path.
    struct OwnedToken(HANDLE);
    impl Drop for OwnedToken {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// The current process user's SID as an SDDL string (`S-1-5-21-…`).
    fn current_user_sid_string() -> io::Result<String> {
        unsafe {
            let mut raw = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw)?;
            let token = OwnedToken(raw);

            // First call sizes the buffer (returns ERROR_INSUFFICIENT_BUFFER).
            let mut len = 0u32;
            let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buf.as_mut_ptr() as *mut _),
                len,
                &mut len,
            )?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);

            let mut pwstr = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut pwstr)?;
            let s = pwstr.to_string().map_err(io::Error::other)?;
            let _ = LocalFree(Some(HLOCAL(pwstr.0 as *mut _)));
            Ok(s)
        }
    }
}
