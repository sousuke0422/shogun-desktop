//! Monarch-side adoption of an `attach` request (IPC.md): pull the sender's
//! handles across with `DuplicateHandle`, then assemble an engine session
//! over them. Windows-only — the OS default-terminal handoff has no
//! equivalent elsewhere (Unix tab moves will ride `SCM_RIGHTS` instead).

use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use anyhow::{Context as _, Result, bail};
use rikka_terminal_core::pty_handoff::{HandoffPty, build_handoff_session};
use rikka_terminal_core::{TerminalSession, xtversion};
use rikka_terminal_ipc as ipc;
use windows::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

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

/// Adopt an attach: pull every handle the sender advertised and build the
/// session over them. Runs on the IPC thread — the sender blocks on our
/// response, so its process (and handle table) stays alive throughout the
/// pull; once we respond `ok` it may exit, and by then ownership has moved.
pub fn session_from_attach(args: &ipc::AttachArgs) -> Result<TerminalSession> {
    let source = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, args.pid) }
        .with_context(|| format!("OpenProcess({}) for attach handle pull", args.pid))?;
    // Owned wrapper so the process handle closes on every return path.
    let source = unsafe { OwnedHandle::from_raw_handle(source.0) };
    let src = HANDLE(source.as_raw_handle());

    let (Some(input), Some(output)) = (
        pull(src, args.handles.input)?,
        pull(src, args.handles.output)?,
    ) else {
        bail!("attach carries no input/output handles");
    };
    let signal = pull(src, args.handles.signal)?;
    let keepalive: Vec<OwnedHandle> = [
        args.handles.reference,
        args.handles.server,
        args.handles.client,
        args.handles.hpcon,
        args.handles.shell,
    ]
    .into_iter()
    .filter_map(|raw| pull(src, raw).transpose())
    .collect::<Result<_>>()?;

    assemble(input, output, signal, keepalive, &args.startup)
}

/// A raw handle value that is ALREADY valid in this process — a cold start's
/// inherited handles (IPC.md "attach cold": inheritance was the transfer, so
/// there is nothing to duplicate, just ownership to take).
fn owned_local(raw: i64) -> Option<OwnedHandle> {
    (raw != 0).then(|| unsafe { OwnedHandle::from_raw_handle(raw as isize as _) })
}

/// Adopt a cold-start attach: the same message shape as the IPC path, but
/// the handle values are interpreted in OUR handle table (inherited through
/// CreateProcess by the handoff shim).
pub fn session_from_local(args: &ipc::AttachArgs) -> Result<TerminalSession> {
    let (Some(input), Some(output)) = (
        owned_local(args.handles.input),
        owned_local(args.handles.output),
    ) else {
        bail!("attach carries no input/output handles");
    };
    let signal = owned_local(args.handles.signal);
    let keepalive: Vec<OwnedHandle> = [
        args.handles.reference,
        args.handles.server,
        args.handles.client,
        args.handles.hpcon,
        args.handles.shell,
    ]
    .into_iter()
    .filter_map(owned_local)
    .collect();

    assemble(input, output, signal, keepalive, &args.startup)
}

/// Common tail: owned handles → live session, startup seeding the interim
/// size (the real one lands with the window's first frame fit) and title.
fn assemble(
    input: OwnedHandle,
    output: OwnedHandle,
    signal: Option<OwnedHandle>,
    keepalive: Vec<OwnedHandle>,
    startup: &ipc::StartupInfo,
) -> Result<TerminalSession> {
    let cols = if startup.cols >= 2 { startup.cols } else { 80 };
    let rows = if startup.rows >= 2 { startup.rows } else { 24 };
    let session = build_handoff_session(
        cols,
        rows,
        HandoffPty {
            input,
            output,
            signal,
            keepalive,
        },
        &xtversion::engine_identity(),
    )?;
    if let Some(title) = &startup.title {
        *session.title.lock() = Some(title.clone());
    }
    Ok(session)
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
        let session = session_from_local(&args).expect("session over inherited values");
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
        assert!(session_from_local(&args).is_err());
    }
}
