//! Monarch-side adoption of an `attach` request (IPC.md): take ownership of
//! the sender's handles, then either relay them to a fresh window process
//! (the crash-isolation default) or assemble an engine session in-process.
//! Windows-only — the OS default-terminal handoff has no equivalent
//! elsewhere (Unix tab moves will ride `SCM_RIGHTS` instead).
//!
//! Two ways in, two ways out:
//! - [`pull_attach`]: the handle values live in the SENDER — duplicate them
//!   across (`DUPLICATE_CLOSE_SOURCE`, receiver-pulls).
//! - [`local_attach`]: the values are already valid here (a cold start's
//!   inherited handles) — just take ownership.
//! - [`LocalAttach::relay_to_window_process`]: re-launch them into their own
//!   process via the cold-start form (inheritance is the transfer again).
//! - [`LocalAttach::into_session`]: build the session in this process.

use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use anyhow::{Context as _, Result, bail, ensure};
use rikka_terminal_core::pty_handoff::{HandoffPty, TransferKit};
use rikka_terminal_core::{TerminalSession, xtversion};
use rikka_terminal_ipc as ipc;
use windows::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

/// An attach whose handles are now owned by THIS process, wire identities
/// intact (relaying needs the positions, assembly needs the split).
pub struct LocalAttach {
    input: OwnedHandle,
    output: OwnedHandle,
    signal: Option<OwnedHandle>,
    reference: Option<OwnedHandle>,
    server: Option<OwnedHandle>,
    client: Option<OwnedHandle>,
    hpcon: Option<OwnedHandle>,
    shell: Option<OwnedHandle>,
    pub startup: ipc::StartupInfo,
    /// A tab move's screen replay (core `replay_bytes`), fed to the
    /// assembled session's parser before any pipe byte — the moved tab
    /// arrives wearing the sender's screen instead of blank.
    state_vt: Option<Vec<u8>>,
}

/// Duplicate one raw handle value out of `source` into this process.
/// `DUPLICATE_CLOSE_SOURCE` closes the sender's copy — ownership moves, per
/// the receiver-pulls contract in IPC.md. `0` means "not sent".
fn pull(source: HANDLE, raw: i64) -> Result<Option<OwnedHandle>> {
    if raw == 0 {
        return Ok(None);
    }
    let mut dup = HANDLE::default();
    unsafe {
        DuplicateHandle(
            source,
            HANDLE(raw as _),
            GetCurrentProcess(),
            &mut dup,
            0,
            false,
            DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
        )
    }
    .with_context(|| format!("DuplicateHandle({raw:#x}) from attach sender"))?;
    Ok(Some(unsafe { OwnedHandle::from_raw_handle(dup.0) }))
}

/// Pull every handle the sender advertised. Runs on the IPC thread — the
/// sender blocks on our response, so its process (and handle table) stays
/// alive throughout the pull; once we respond `ok` it may exit, and by then
/// ownership has moved.
pub fn pull_attach(args: &ipc::AttachArgs) -> Result<LocalAttach> {
    let source = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, args.pid) }
        .with_context(|| format!("OpenProcess({}) for attach handle pull", args.pid))?;
    // Owned wrapper so the process handle closes on every return path.
    let source = unsafe { OwnedHandle::from_raw_handle(source.0) };
    let src = HANDLE(source.as_raw_handle());
    take(args, |raw| pull(src, raw))
}

/// A raw handle value that is ALREADY valid in this process — a cold start's
/// inherited handles (IPC.md "attach cold": inheritance was the transfer, so
/// there is nothing to duplicate, just ownership to take).
fn owned_local(raw: i64) -> Result<Option<OwnedHandle>> {
    Ok((raw != 0).then(|| unsafe { OwnedHandle::from_raw_handle(raw as isize as _) }))
}

/// Take ownership of a cold-start attach: the same message shape as the IPC
/// path, but the values are interpreted in OUR handle table.
pub fn local_attach(args: &ipc::AttachArgs) -> Result<LocalAttach> {
    take(args, owned_local)
}

/// Common front: run `acquire` over every advertised handle value.
fn take(
    args: &ipc::AttachArgs,
    mut acquire: impl FnMut(i64) -> Result<Option<OwnedHandle>>,
) -> Result<LocalAttach> {
    let (Some(input), Some(output)) = (acquire(args.handles.input)?, acquire(args.handles.output)?)
    else {
        bail!("attach carries no input/output handles");
    };
    Ok(LocalAttach {
        input,
        output,
        signal: acquire(args.handles.signal)?,
        reference: acquire(args.handles.reference)?,
        server: acquire(args.handles.server)?,
        client: acquire(args.handles.client)?,
        hpcon: acquire(args.handles.hpcon)?,
        shell: acquire(args.handles.shell)?,
        startup: args.startup.clone(),
        state_vt: ipc::vt_from_state(&args.state),
    })
}

impl LocalAttach {
    /// Wrap a live session's birth-time transfer duplicates (a quiesced tab
    /// on its way OUT of this process) in the six-slot wire identities: the
    /// keepalive processes ride the `server`/`client` slots in order, and
    /// `reference` is never present — the session released it when it went
    /// live.
    pub fn from_transfer(
        kit: TransferKit,
        startup: ipc::StartupInfo,
        state_vt: Option<Vec<u8>>,
    ) -> Result<Self> {
        ensure!(
            kit.keepalive.len() <= 2,
            "transfer kit carries more keepalive handles than the wire has slots"
        );
        let mut keepalive = kit.keepalive.into_iter();
        Ok(LocalAttach {
            input: kit.input,
            output: kit.output,
            signal: kit.signal,
            reference: None,
            server: keepalive.next(),
            client: keepalive.next(),
            hpcon: None,
            shell: None,
            startup,
            state_vt,
        })
    }

    /// Assemble the engine session in this process. The startup seeds the
    /// interim size (the real one lands with the window's first frame fit)
    /// and title.
    pub fn into_session(self) -> Result<TerminalSession> {
        let LocalAttach {
            input,
            output,
            signal,
            reference,
            server,
            client,
            hpcon,
            shell,
            startup,
            state_vt,
        } = self;
        let cols = if startup.cols >= 2 { startup.cols } else { 80 };
        let rows = if startup.rows >= 2 { startup.rows } else { 24 };
        let keepalive: Vec<OwnedHandle> = [server, client, hpcon, shell]
            .into_iter()
            .flatten()
            .collect();
        let session = rikka_terminal_core::pty_handoff::build_handoff_session_with_preface(
            cols,
            rows,
            HandoffPty {
                input,
                output,
                signal,
                keepalive,
            },
            state_vt.unwrap_or_default(),
            &xtversion::engine_identity(),
        )?;
        // The \Reference handle keeps conhost serving even after its last
        // client left (winconpty.h) — held for the session's lifetime, an
        // exited shell never breaks our output pipe and the tab lingers
        // forever. Upstream releases it the moment the connection starts;
        // same here, now that the session is live.
        drop(reference);
        if let Some(title) = &startup.title {
            *session.title.lock() = Some(title.clone());
        }
        Ok(session)
    }

    /// Crash isolation: hand this session to its OWN window process, reusing
    /// the cold-start launch form (`--window-process --attach …`, transfer by
    /// CreateProcess inheritance — the same mirror the shim uses when no
    /// monarch runs). The caller drops `self` on success: our copies close,
    /// the child keeps its inherited ones. On failure the caller still owns
    /// everything and can fall back to [`Self::into_session`].
    pub fn relay_to_window_process(&self) -> Result<()> {
        if self.hpcon.is_some() || self.shell.is_some() {
            // hpcon/shell have no slot in the 6-value --attach form (they
            // only travel with tab moves, which route directly in inc6).
            bail!("hpcon/shell handles cannot ride the relay launch");
        }
        let raw = |h: &Option<OwnedHandle>| -> i64 {
            h.as_ref().map_or(0, |h| h.as_raw_handle() as isize as i64)
        };
        let handles = [
            self.input.as_raw_handle() as isize as i64,
            self.output.as_raw_handle() as isize as i64,
            raw(&self.signal),
            raw(&self.reference),
            raw(&self.server),
            raw(&self.client),
        ];
        for value in handles {
            if value != 0 {
                unsafe {
                    SetHandleInformation(
                        HANDLE(value as isize as _),
                        HANDLE_FLAG_INHERIT.0,
                        HANDLE_FLAG_INHERIT,
                    )
                }
                .context("SetHandleInformation(HANDLE_FLAG_INHERIT)")?;
            }
        }
        let exe = std::env::current_exe().context("current_exe")?;
        let csv = handles.map(|v| v.to_string()).join(",");
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--window-process").arg("--attach").arg(csv);
        if let Some(title) = &self.startup.title {
            cmd.arg("--attach-title").arg(title);
        }
        if self.startup.cols >= 2 && self.startup.rows >= 2 {
            cmd.arg("--size")
                .arg(format!("{},{}", self.startup.cols, self.startup.rows));
        }
        // The screen replay is bulk bytes — handles ride inheritance, this
        // rides a temp file the child reads once and deletes. A stale file
        // (child died first) is a few KB in %TEMP%, harmless.
        if let Some(vt) = &self.state_vt {
            let path = std::env::temp_dir().join(format!(
                "rikka-move-{}-{:x}.vt",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            std::fs::write(&path, vt).context("write tab-move state file")?;
            cmd.arg("--attach-state").arg(&path);
        }
        // Rust's std spawns with bInheritHandles=TRUE — the transfer.
        cmd.spawn().context("spawn window process for attach")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::os::windows::io::IntoRawHandle as _;

    /// Inherited-style raw values (no duplication) become a live session:
    /// output flows into the grid, input reaches the console side, and the
    /// startup title lands. The IPC-pull variant differs only in the
    /// DuplicateHandle front, exercised for real at P3.
    #[test]
    fn local_raw_values_become_a_live_session() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (mut in_read, in_write) = std::io::pipe().expect("input pipe");
        let args = ipc::AttachArgs {
            pid: std::process::id(),
            handles: ipc::Handles {
                input: in_write.into_raw_handle() as isize as i64,
                output: out_read.into_raw_handle() as isize as i64,
                ..Default::default()
            },
            startup: ipc::StartupInfo {
                title: Some("cmd".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let session = local_attach(&args)
            .and_then(LocalAttach::into_session)
            .expect("session over inherited values");
        assert_eq!(session.title.lock().as_deref(), Some("cmd"));

        out_write.write_all(b"hi").unwrap();
        drop(out_write);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader must reach EOF");
        let row0: String = session.snapshot.lock().cells[0]
            .iter()
            .map(|c| c.c)
            .collect();
        assert!(row0.starts_with("hi"), "got {row0:?}");

        session.send_bytes(b"x");
        let mut buf = [0u8; 1];
        in_read.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"x");
    }

    #[test]
    fn local_attach_without_pipes_is_refused() {
        let args = ipc::AttachArgs::default();
        assert!(local_attach(&args).is_err());
    }

    /// The ConDrv reference handle must be closed once the session is live —
    /// holding it would keep conhost serving after the shell exits, so the
    /// tab would never see EOF. Proven here by EOF on the peer end of a pipe
    /// standing in for the reference, while the session is still running.
    #[test]
    fn reference_is_dropped_once_the_session_is_live() {
        let (out_read, _out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let (mut ref_read, ref_write) = std::io::pipe().expect("reference stand-in");
        let args = ipc::AttachArgs {
            pid: std::process::id(),
            handles: ipc::Handles {
                input: in_write.into_raw_handle() as isize as i64,
                output: out_read.into_raw_handle() as isize as i64,
                reference: ref_write.into_raw_handle() as isize as i64,
                ..Default::default()
            },
            ..Default::default()
        };
        let session = local_attach(&args)
            .and_then(LocalAttach::into_session)
            .expect("session over inherited values");

        // Read on a helper thread so a regression fails the timeout instead
        // of hanging the test on a pipe whose write end is still open.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1];
            tx.send(ref_read.read(&mut buf).expect("peer read")).ok();
        });
        let n = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reference still held: peer saw no EOF");
        assert_eq!(n, 0, "expected EOF on the reference peer");
        drop(session);
    }
}
