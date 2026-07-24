//! Sending half of a cross-process tab move (IPC.md "One primitive, three
//! uses"): quiesce the source, then hand the session's birth-time transfer
//! duplicates to their new owner — a fresh window process (cross-process
//! detach, transfer by CreateProcess inheritance) or an existing window's
//! own socket (merge, transfer by receiver pull).
//!
//! Ordering contract: pause and drain the source before connecting to the
//! destination, then send the prepare frame immediately. The pause is
//! recoverable until the receiver acknowledges commit, so a stale endpoint
//! resumes the source without exposing a silent connection to the receiver's
//! first-frame deadline. Two readers on one pipe shred the VT stream, so OUR
//! reader must remain paused until the destination is prepared and must be
//! provably dead before the destination starts reading.
//!
//! The move carries the screen: scrollback + visible grid + cursor + mode
//! bits ride `state` as replayable VT (core `replay_bytes`), so the tab
//! arrives wearing what it showed. See IPC.md "Screen carry".

#![cfg(windows)]

use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::sync::atomic::Ordering;

use anyhow::{Context as _, Result, ensure};
use rikka_terminal_core::TerminalSession;
use rikka_terminal_core::pty_handoff::TransferKit;
use rikka_terminal_ipc as ipc;

use crate::attach::LocalAttach;

/// Where the tab goes.
pub enum Destination {
    /// A fresh window process — crash isolation for the detached tab.
    NewProcess,
    /// An existing window process, addressed by its own socket (resolved
    /// through the monarch before calling). `drop_at` = screen-pixel
    /// cursor position of a drag-merge drop; the receiver inserts the tab
    /// at the strip position under it (`None` = append, e.g. Ctrl+Shift+X).
    Window {
        id: u64,
        endpoint: String,
        drop_at: Option<(i32, i32)>,
    },
}

/// Whether the session can leave this process at all — born handoff-shaped
/// with its transfer kit still in stock. `false` = legacy portable-pty (or
/// an already-moved session); callers fall back to the in-process split,
/// which never risks the session.
pub fn is_transferable(session: &TerminalSession) -> bool {
    session.transfer.lock().is_some()
}

/// Move a live session out of this process. On `Ok` the caller closes the
/// tab: the session's remaining handles are independent duplicates of what
/// the new owner holds, so dropping them cannot break the pipes or kill the
/// console.
///
/// Failure ordering: the transfer kit and handle count are checked before
/// pausing. Connection, validation, and prepare failures resume the source
/// from its retained handles; only a successfully acknowledged commit makes
/// the pause irreversible.
pub fn send_tab(
    session: &TerminalSession,
    palette: Option<Vec<u32>>,
    dest: Destination,
) -> Result<()> {
    // Refuse before quiescing — a session without a kit (SSH, legacy
    // portable-pty, or already moved) must stay fully alive.
    let kit = session
        .transfer
        .lock()
        .take()
        .context("session is not transferable (no ConPTY transfer kit)")?;
    ensure!(
        kit.keepalive.len() <= 2,
        "transfer kit carries more keepalive handles than the wire has slots"
    );
    let startup = ipc::StartupInfo {
        title: session.title.lock().clone(),
        x: 0,
        y: 0,
        cols: session.cols.load(Ordering::Relaxed),
        rows: session.rows.load(Ordering::Relaxed),
    };
    if let Err(e) = session.pause_for_transfer() {
        *session.transfer.lock() = Some(kit);
        return Err(e);
    }
    // Serialize AFTER pausing — the reader is blocked, the Term is finally
    // still. The receiver replays this as its parser preface, so the tab
    // arrives wearing the sender's screen instead of blank. The image store
    // travels alongside; placeholders of images that made the budget are
    // re-emitted in the replay (the rest blank, as before).
    let images = rikka_terminal_core::pty_handoff::image_payloads(session);
    let shipped: std::collections::HashSet<u32> = images.iter().map(|(id, ..)| *id).collect();
    let vt = rikka_terminal_core::pty_handoff::replay_bytes(session, &shipped);
    let result = match dest {
        // Inheritance COPIES the handles into the child, so the kit keeps
        // ownership of ours throughout — plain drop semantics on every path.
        Destination::NewProcess => (|| {
            LocalAttach::from_transfer(kit.try_clone()?, startup, Some(vt), palette, images)?
                .relay_to_window_process()
                .context("relay the detached tab to its window process")
        })(),
        Destination::Window {
            id,
            endpoint,
            drop_at,
        } => (|| {
            // Connect only once the bounded pause and replay serialization
            // are complete. Otherwise the receiver's equally bounded
            // first-frame read races the local pause and can close a valid
            // transfer just as PrepareAttach is sent.
            let conn = ipc::transport::connect(&endpoint)
                .with_context(|| format!("connect window socket {endpoint}"))?;
            push_to_window(
                conn, session, &kit, startup, vt, images, palette, id, drop_at,
            )
        })(),
    };
    match result {
        Ok(()) => {
            if session.reader_thread.lock().is_some() {
                session.commit_transfer()?;
            }
            Ok(())
        }
        Err(e) => {
            if session.reader_thread.lock().is_some() {
                session.resume_after_failed_transfer();
                *session.transfer.lock() = Some(kit);
            }
            Err(e)
        }
    }
}

/// Send the kit over an already-open window socket. Prepare duplicates
/// without consuming these handles; only after readiness + commit ACKs do
/// we permanently stop the source reader and finalize adoption.
fn push_to_window(
    mut conn: ipc::transport::Conn,
    session: &TerminalSession,
    kit: &TransferKit,
    startup: ipc::StartupInfo,
    state_vt: Vec<u8>,
    images: Vec<ipc::ImagePayload>,
    palette: Option<Vec<u32>>,
    id: u64,
    drop_at: Option<(i32, i32)>,
) -> Result<()> {
    let raw = |h: &OwnedHandle| h.as_raw_handle() as isize as i64;
    let mut keepalive = kit.keepalive.iter();
    let args = ipc::AttachArgs {
        pid: std::process::id(),
        handles: ipc::Handles {
            input: raw(&kit.input),
            output: raw(&kit.output),
            signal: kit.signal.as_ref().map(&raw).unwrap_or(0),
            reference: 0,
            server: keepalive.next().map(&raw).unwrap_or(0),
            client: keepalive.next().map(&raw).unwrap_or(0),
            ..Default::default()
        },
        startup,
        state: Some(ipc::state_from_parts(&state_vt, &images)),
        elevated: false,
        // Informational: an attach on a window socket always adopts there.
        target: ipc::Target::Window(id),
        drop_at,
        palette,
    };
    conn.send_request(&ipc::Request::PrepareAttach(args))
        .context("prepare tab-move attach")?;
    let resp = conn.recv_response().context("await tab-move response")?;
    ensure!(
        resp.ok,
        "window refused the tab: {}",
        resp.error.unwrap_or_default()
    );
    conn.send_request(&ipc::Request::CommitAttach)
        .context("commit prepared tab move")?;
    let resp = conn.recv_response().context("await tab-move commit")?;
    ensure!(
        resp.ok,
        "window refused tab commit: {}",
        resp.error.unwrap_or_default()
    );
    session.commit_transfer()?;
    conn.send_request(&ipc::Request::FinalizeAttach)
        .context("finalize prepared tab move")?;
    let resp = conn.recv_response().context("await tab-move finalize")?;
    ensure!(
        resp.ok,
        "window failed to finalize tab: {}",
        resp.error.unwrap_or_default()
    );
    Ok(())
}

/// Windows the monarch knows about, minus this process's own. Entries are
/// per-window now; `pid` is the ownership test (with the legacy id==pid
/// fallback for a directory served by an older monarch).
fn other_windows() -> Result<Vec<ipc::WindowInfo>> {
    let mut conn = ipc::transport::connect(&ipc::transport::endpoint_name())
        .context("connect the monarch (is another window running?)")?;
    conn.send_request(&ipc::Request::ListWindows)?;
    let resp = conn.recv_response()?;
    ensure!(resp.ok, "list_windows: {}", resp.error.unwrap_or_default());
    let me = std::process::id();
    Ok(resp
        .windows
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.pid != me && w.id != u64::from(me))
        .collect())
}

/// Resolve a window id to its own socket endpoint through the monarch
/// (IPC.md `resolve_window` — the monarch never proxies handles). Doubles
/// as the "is that a rikka window?" probe for the drag-merge gesture: an
/// unknown pid simply fails to resolve.
pub(crate) fn resolve_window(window: u64) -> Result<String> {
    let mut conn = ipc::transport::connect(&ipc::transport::endpoint_name())?;
    conn.send_request(&ipc::Request::ResolveWindow { window })?;
    let resp = conn.recv_response()?;
    ensure!(
        resp.ok,
        "resolve_window({window}): {}",
        resp.error.unwrap_or_default()
    );
    resp.endpoint
        .context("resolve_window answered without an endpoint")
}

/// Move the session into the first OTHER window process the monarch knows.
/// v1 granularity is the process (window_id = pid): windows detached
/// in-process share a pid and cannot be addressed individually — use the
/// in-process merge for those.
pub fn move_to_any_other_window(
    session: &TerminalSession,
    palette: Option<Vec<u32>>,
) -> Result<()> {
    let others = other_windows()?;
    let target = others
        .first()
        .context("no other window process to move the tab to")?;
    let endpoint = resolve_window(target.id)?;
    send_tab(
        session,
        palette,
        Destination::Window {
            id: target.id,
            endpoint,
            drop_at: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::windows::io::OwnedHandle;

    use rikka_terminal_core::pty_handoff::{HandoffPty, build_handoff_session};

    use super::*;

    /// A dead destination discovered after the recoverable pause must leave
    /// the tab fully alive — kit restocked and reader pumping output again.
    /// A stale directory entry may abort a move, but must never cost the tab.
    #[test]
    fn dead_endpoint_leaves_the_tab_alive() {
        let (out_read, mut out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("live session");

        let err = send_tab(
            &session,
            None,
            Destination::Window {
                id: 9,
                endpoint: format!("rikka-test-dead-{}.sock", std::process::id()),
                drop_at: None,
            },
        )
        .expect_err("a dead endpoint must refuse the move");
        assert!(err.to_string().contains("connect"), "{err:#}");
        assert!(is_transferable(&session), "kit must remain stocked");

        out_write.write_all(b"still-alive").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let row0: String = session.snapshot.lock().cells[0]
                .iter()
                .map(|c| c.c)
                .collect();
            if row0.starts_with("still-alive") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reader must still pump after the aborted move"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn failed_prepare_resumes_the_paused_source() {
        let (out_read, mut out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let session = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("live session");

        let name = format!("rikka-test-prepare-drop-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).unwrap();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            assert!(matches!(
                conn.recv_request().unwrap(),
                ipc::Request::PrepareAttach(_)
            ));
            // Drop without a readiness ACK.
        });
        let err = send_tab(
            &session,
            None,
            Destination::Window {
                id: 9,
                endpoint: name,
                drop_at: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("response"), "{err:#}");
        server.join().unwrap();
        assert!(is_transferable(&session), "kit must be restored");

        out_write.write_all(b"resumed").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let grid: String = session
                .snapshot
                .lock()
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|c| c.c))
                .collect();
            if grid.contains("resumed") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reader did not resume after failed prepare"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
