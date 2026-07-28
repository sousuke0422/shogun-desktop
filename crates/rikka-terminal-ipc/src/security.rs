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

use std::io::{self, Read as _, Write as _};
use std::sync::OnceLock;

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

static CAPABILITY: OnceLock<String> = OnceLock::new();

/// Per-user random capability carried by every frame. The endpoint name is
/// public rendezvous data; possession of this owner-only file is the
/// application-level authorization check.
pub fn capability() -> io::Result<&'static str> {
    if let Some(value) = CAPABILITY.get() {
        return Ok(value);
    }
    let value = load_or_create_capability()?;
    let _ = CAPABILITY.set(value);
    Ok(CAPABILITY.get().expect("capability initialized"))
}

pub(crate) fn capability_matches(candidate: &str) -> io::Result<bool> {
    let expected = capability()?.as_bytes();
    let candidate = candidate.as_bytes();
    if expected.len() != candidate.len() {
        return Ok(false);
    }
    Ok(expected
        .iter()
        .zip(candidate)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0)
}

fn capability_path() -> io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::env::temp_dir()));
    base.map(|p| p.join("RikkaTerminal").join("ipc.cap"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no IPC capability directory"))
}

/// Accept only the exact shape we write: 64 hex digits, nothing else.
fn parse_capability(raw: &str) -> io::Result<String> {
    let value = raw.trim();
    if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(value.to_ascii_lowercase());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid IPC capability file",
    ))
}

fn read_capability(path: &std::path::Path) -> io::Result<String> {
    let mut raw = String::new();
    std::fs::File::open(path)?.read_to_string(&mut raw)?;
    parse_capability(&raw)
}

fn load_or_create_capability() -> io::Result<String> {
    load_or_create_capability_at(&capability_path()?)
}

fn load_or_create_capability_at(path: &std::path::Path) -> io::Result<String> {
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut raw = String::new();
        file.read_to_string(&mut raw)?;
        return parse_capability(&raw);
    }

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "capability has no parent"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(windows)]
    windows_impl::protect_path(parent)?;

    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let value: String = random.iter().map(|b| format!("{b:02x}")).collect();

    // Publish atomically. `create_new` makes only the file's EXISTENCE
    // atomic — the bytes land afterwards, so a starter racing us took the
    // AlreadyExists path, read a file that was still empty, and rejected it
    // as corrupt (InvalidData where the caller expected a clean mismatch).
    // Fill a private temp file first and hard-link it into place: the final
    // name never exists without its full contents, and the link still fails
    // for everyone but the first writer, so all starters agree on one value.
    // The temp name carries the value so two threads cannot share one.
    let temp = path.with_file_name(format!(
        "ipc.cap.{}.{}.tmp",
        std::process::id(),
        &value[..16]
    ));
    let published = (|| -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        windows_impl::protect_path(&temp)?;
        std::fs::hard_link(&temp, path)
    })();
    let _ = std::fs::remove_file(&temp);

    match published {
        Ok(()) => Ok(value),
        // Lost the race — the winner's file is complete by construction.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_capability(path),
        Err(e) => Err(e),
    }
}

/// Stable, non-spoofable per-user component for named-pipe endpoints.
pub fn user_key() -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_impl::current_user_sid_string()
    }
    #[cfg(not(windows))]
    {
        Ok(std::env::var("UID")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_default())
    }
}

/// Verify that the OS-attested peer process is owned by our user.
pub fn verify_peer_pid(pid: u32) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::verify_peer_pid(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Client-side monarch/window verification: same user plus the installed
/// RikkaTerminal server image. This is checked before any request bytes
/// (notably handle values or launch argv) leave the client.
pub fn verify_server_pid(pid: u32) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::verify_server_pid(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(())
    }
}

pub fn verify_client_pid(pid: u32) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::verify_client_pid(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(())
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::io;

    use interprocess::local_socket::ListenerOptions;
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptor as _, SecurityDescriptor,
    };
    use std::path::Path;
    use widestring::U16CString;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, PSECURITY_DESCRIPTOR, SetFileSecurityW,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::core::{PCWSTR, PWSTR};

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
    pub(super) fn current_user_sid_string() -> io::Result<String> {
        token_sid_string(unsafe { GetCurrentProcess() })
    }

    fn token_sid_string(process: HANDLE) -> io::Result<String> {
        unsafe {
            let mut raw = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut raw)?;
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

    pub(super) fn verify_peer_pid(pid: u32) -> io::Result<()> {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let process = OwnedToken(process);
        if token_sid_string(process.0)? == current_user_sid_string()? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC peer belongs to a different user",
            ))
        }
    }

    fn verified_image_name(pid: u32) -> io::Result<String> {
        verify_peer_pid(pid)?;
        if pid == unsafe { GetCurrentProcessId() } {
            return Ok("self".into());
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let process = OwnedToken(process);
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                process.0,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut len,
            )
        }
        .map_err(io::Error::other)?;
        path.truncate(len as usize);
        let peer = std::path::PathBuf::from(String::from_utf16(&path).map_err(io::Error::other)?);
        let current = std::env::current_exe()?;
        if peer.parent() != current.parent() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC peer executable is outside the RikkaTerminal install",
            ));
        }
        peer.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "invalid IPC peer path"))
    }

    pub(super) fn verify_server_pid(pid: u32) -> io::Result<()> {
        let name = verified_image_name(pid)?;
        if name == "self" || name == "rikka-terminal.exe" {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC server executable is not the installed RikkaTerminal",
            ))
        }
    }

    pub(super) fn verify_client_pid(pid: u32) -> io::Result<()> {
        let name = verified_image_name(pid)?;
        if matches!(
            name.as_str(),
            "self" | "rikka-terminal.exe" | "rt.exe" | "rikka-handoff.exe"
        ) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC client executable is not a RikkaTerminal component",
            ))
        }
    }

    fn owner_descriptor() -> io::Result<SecurityDescriptor> {
        let sid = current_user_sid_string()?;
        let sddl = format!("D:P(A;;FA;;;{sid})");
        let wide = U16CString::from_str(&sddl)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SDDL had an interior NUL"))?;
        SecurityDescriptor::deserialize(wide.as_ucstr()).map_err(io::Error::other)
    }

    pub(super) fn protect_path(path: &Path) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt as _;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let sd = owner_descriptor()?;
        unsafe {
            SetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                DACL_SECURITY_INFORMATION,
                PSECURITY_DESCRIPTOR(sd.as_sd() as *mut _),
            )
        }
        .ok()
        .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every starter must come away with the SAME capability, and none may
    /// observe a half-written file. Before the publish was made atomic the
    /// losers of this race opened the winner's still-empty file and failed
    /// with InvalidData, which surfaced as a capability check reporting
    /// corruption where the caller expected a clean mismatch.
    #[test]
    fn concurrent_creation_agrees_on_one_capability() {
        // The window this guards is a few microseconds wide, so one round of
        // threads rarely lands in it. Release the starters from a barrier so
        // they collide on purpose, and repeat over fresh directories — the
        // pre-fix code has to survive every round to pass.
        const STARTERS: usize = 16;
        const ROUNDS: usize = 40;

        for round in 0..ROUNDS {
            let dir =
                std::env::temp_dir().join(format!("rikka-ipc-cap-{}-{round}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("ipc.cap");

            let barrier = std::sync::Barrier::new(STARTERS);
            let results: Vec<io::Result<String>> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..STARTERS)
                    .map(|_| {
                        s.spawn(|| {
                            barrier.wait();
                            load_or_create_capability_at(&path)
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });

            // No scratch file may outlive a publish, won or lost.
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .expect("the capability directory exists")
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .filter(|n| n.to_string_lossy().ends_with(".tmp"))
                .collect();
            let _ = std::fs::remove_dir_all(&dir);

            let first = results[0]
                .as_ref()
                .unwrap_or_else(|e| panic!("round {round}: the first starter failed: {e}"));
            assert_eq!(first.len(), 64, "a capability is 64 hex digits");
            for (i, result) in results.iter().enumerate() {
                match result {
                    Ok(value) => {
                        assert_eq!(value, first, "round {round}: starter {i} disagreed")
                    }
                    Err(e) => panic!("round {round}: starter {i} failed: {e}"),
                }
            }
            assert!(
                leftovers.is_empty(),
                "round {round}: temp files left behind: {leftovers:?}"
            );
        }
    }

    #[test]
    fn only_the_exact_shape_is_accepted() {
        assert_eq!(parse_capability(&"a".repeat(64)).unwrap(), "a".repeat(64));
        assert_eq!(parse_capability(&"AB".repeat(32)).unwrap(), "ab".repeat(32));
        for bad in ["", &"a".repeat(32), &"a".repeat(65), &"z".repeat(64)] {
            assert_eq!(
                parse_capability(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "rejected: {bad:?}"
            );
        }
    }
}
