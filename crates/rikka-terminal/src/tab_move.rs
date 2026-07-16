//! Sending half of a cross-process tab move (IPC.md "One primitive, three
//! uses"): quiesce the source, then hand the session's birth-time transfer
//! duplicates to their new owner — a fresh window process (cross-process
//! detach, transfer by CreateProcess inheritance) or an existing window's
//! own socket (merge, transfer by receiver pull).
//!
//! Ordering contract: connect to the destination FIRST (a stale directory
//! entry must abort while the tab is fully alive), then quiesce — the
//! receiver starts reading the moment its session assembles, and two
//! readers on one pipe shred the VT stream, so OUR reader must be provably
//! dead and drained BEFORE the handles go on the wire
//! (`TerminalSession::quiesce_for_transfer`). Quiesce is irreversible: a
//! failure past it (spawn/wire errors, rare and local) leaves the tab
//! honestly disconnected — the console itself is still owned by SOMEONE
//! (our session's handles on an early failure, the receiver's pulls on a
//! late one).
//!
//! The move carries the screen: scrollback + visible grid + cursor + mode
//! bits ride `state` as replayable VT (core `replay_bytes`), so the tab
//! arrives wearing what it showed. See IPC.md "Screen carry".

#![cfg(windows)]

use std::os::windows::io::{IntoRawHandle as _, OwnedHandle};
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
/// Failure ordering: everything that can be checked is checked BEFORE the
/// irreversible quiesce — no kit, too many keepalives, and (for a window
/// move) the destination connection itself. A stale directory entry or a
/// closed window then fails while the tab is still fully alive; only a
/// failure past the quiesce (spawn/wire errors, both rare and local) costs
/// the tab.
pub fn send_tab(session: &TerminalSession, dest: Destination) -> Result<()> {
    // Open the destination first: the receiver starts reading only once
    // the attach frame arrives, so connecting early cannot race our still-
    // running reader — and a dead endpoint aborts a fully-alive tab.
    let conn = match &dest {
        Destination::NewProcess => None,
        Destination::Window { endpoint, .. } => Some(
            ipc::transport::connect(endpoint)
                .with_context(|| format!("connect window socket {endpoint}"))?,
        ),
    };
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
    session.quiesce_for_transfer()?;
    // Serialize AFTER quiesce — the reader is dead, the Term is finally
    // still. The receiver replays this as its parser preface, so the tab
    // arrives wearing the sender's screen instead of blank.
    let vt = rikka_terminal_core::pty_handoff::replay_bytes(session);
    match (dest, conn) {
        // Inheritance COPIES the handles into the child, so the kit keeps
        // ownership of ours throughout — plain drop semantics on every path.
        (Destination::NewProcess, _) => LocalAttach::from_transfer(kit, startup, Some(vt))?
            .relay_to_window_process()
            .context("relay the detached tab to its window process"),
        (Destination::Window { id, drop_at, .. }, Some(conn)) => {
            push_to_window(conn, kit, startup, vt, id, drop_at)
        }
        (Destination::Window { .. }, None) => unreachable!("window move always connects first"),
    }
}

/// Send the kit over an already-open window socket. The receiver pulls
/// each handle with `DUPLICATE_CLOSE_SOURCE` — the values are CONSUMED
/// over there, so once the request is on the wire we must never close
/// them ourselves: after a pull the same value may already name an
/// unrelated handle (kernel reuse), and closing that corrupts the
/// process. Leaking a few pipe handles on the rare failure path is the
/// safe price.
fn push_to_window(
    mut conn: ipc::transport::Conn,
    kit: TransferKit,
    startup: ipc::StartupInfo,
    state_vt: Vec<u8>,
    id: u64,
    drop_at: Option<(i32, i32)>,
) -> Result<()> {
    let raw = |h: OwnedHandle| h.into_raw_handle() as isize as i64;
    let mut keepalive = kit.keepalive.into_iter();
    let args = ipc::AttachArgs {
        pid: std::process::id(),
        handles: ipc::Handles {
            input: raw(kit.input),
            output: raw(kit.output),
            signal: kit.signal.map(&raw).unwrap_or(0),
            reference: 0,
            server: keepalive.next().map(&raw).unwrap_or(0),
            client: keepalive.next().map(&raw).unwrap_or(0),
            ..Default::default()
        },
        startup,
        state: Some(ipc::state_from_vt(&state_vt)),
        elevated: false,
        // Informational: an attach on a window socket always adopts there.
        target: ipc::Target::Window(id),
        drop_at,
    };
    conn.send_request(&ipc::Request::Attach(args))
        .context("send tab-move attach")?;
    let resp = conn.recv_response().context("await tab-move response")?;
    ensure!(
        resp.ok,
        "window refused the tab: {}",
        resp.error.unwrap_or_default()
    );
    Ok(())
}

/// Windows the monarch knows about, minus this process's own.
fn other_windows() -> Result<Vec<ipc::WindowInfo>> {
    let mut conn = ipc::transport::connect(&ipc::transport::endpoint_name())
        .context("connect the monarch (is another window running?)")?;
    conn.send_request(&ipc::Request::ListWindows)?;
    let resp = conn.recv_response()?;
    ensure!(resp.ok, "list_windows: {}", resp.error.unwrap_or_default());
    let me = u64::from(std::process::id());
    Ok(resp
        .windows
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.id != me)
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
pub fn move_to_any_other_window(session: &TerminalSession) -> Result<()> {
    let others = other_windows()?;
    let target = others
        .first()
        .context("no other window process to move the tab to")?;
    let endpoint = resolve_window(target.id)?;
    send_tab(
        session,
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

    /// A dead destination must fail BEFORE the irreversible quiesce: the
    /// tab stays fully alive — kit still stocked, reader still pumping
    /// output into the grid. This is the transactionality of a move: a
    /// stale directory entry may abort it, but must never cost the tab.
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
}
