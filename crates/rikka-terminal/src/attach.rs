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

    // The real size lands with the window's first frame fit; the startup
    // count-chars (when the launch carried one) just seeds the interim.
    let cols = if args.startup.cols >= 2 {
        args.startup.cols
    } else {
        80
    };
    let rows = if args.startup.rows >= 2 {
        args.startup.rows
    } else {
        24
    };
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
    if let Some(title) = &args.startup.title {
        *session.title.lock() = Some(title.clone());
    }
    Ok(session)
}
