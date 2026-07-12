//! Sending half of a cross-process tab move (IPC.md "One primitive, three
//! uses"): quiesce the source, then hand the session's birth-time transfer
//! duplicates to their new owner — a fresh window process (cross-process
//! detach, transfer by CreateProcess inheritance) or an existing window's
//! own socket (merge, transfer by receiver pull).
//!
//! Ordering contract: the receiver starts reading the moment its session
//! assembles, and two readers on one pipe shred the VT stream — so OUR
//! reader must be provably dead BEFORE the handles leave this process
//! (`TerminalSession::quiesce_for_transfer`). Quiesce is irreversible: when
//! a move fails afterwards, the tab stays behind reading as disconnected —
//! honest, and the console itself is still owned by SOMEONE (our session's
//! handles on an early failure, the receiver's pulls on a late one).
//!
//! v1 moves the PTY only: `state` (grid + scrollback) stays behind, and
//! since ConPTY never repaints, the moved tab starts blank until the
//! application writes again. The `state` wire slot exists for the day this
//! is carried across (IPC.md Deferred).

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
    /// through the monarch before calling).
    Window { id: u64, endpoint: String },
}

/// Move a live session out of this process. On `Ok` the caller closes the
/// tab: the session's remaining handles are independent duplicates of what
/// the new owner holds, so dropping them cannot break the pipes or kill the
/// console.
pub fn send_tab(session: &TerminalSession, dest: Destination) -> Result<()> {
    // Refuse before quiescing — a session without a kit (SSH, legacy
    // portable-pty, or already moved) must stay fully alive.
    let kit = session
        .transfer
        .lock()
        .take()
        .context("session is not transferable (no ConPTY transfer kit)")?;
    let startup = ipc::StartupInfo {
        title: session.title.lock().clone(),
        x: 0,
        y: 0,
        cols: session.cols.load(Ordering::Relaxed),
        rows: session.rows.load(Ordering::Relaxed),
    };
    session.quiesce_for_transfer()?;
    match dest {
        // Inheritance COPIES the handles into the child, so the kit keeps
        // ownership of ours throughout — plain drop semantics on every path.
        Destination::NewProcess => LocalAttach::from_transfer(kit, startup)?
            .relay_to_window_process()
            .context("relay the detached tab to its window process"),
        Destination::Window { id, endpoint } => push_to_window(kit, startup, id, &endpoint),
    }
}

/// Send the kit to a window's own socket. The receiver pulls each handle
/// with `DUPLICATE_CLOSE_SOURCE` — the values are CONSUMED over there, so
/// once the request is on the wire we must never close them ourselves:
/// after a pull the same value may already name an unrelated handle
/// (kernel reuse), and closing that corrupts the process. Leaking a few
/// pipe handles on the rare failure path is the safe price.
fn push_to_window(
    kit: TransferKit,
    startup: ipc::StartupInfo,
    id: u64,
    endpoint: &str,
) -> Result<()> {
    ensure!(
        kit.keepalive.len() <= 2,
        "transfer kit carries more keepalive handles than the wire has slots"
    );
    // Connect before surrendering ownership: a refused connection leaves the
    // kit intact and dropping it closes our duplicates normally.
    let mut conn = ipc::transport::connect(endpoint)
        .with_context(|| format!("connect window socket {endpoint}"))?;
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
        state: None,
        elevated: false,
        // Informational: an attach on a window socket always adopts there.
        target: ipc::Target::Window(id),
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
/// (IPC.md `resolve_window` — the monarch never proxies handles).
fn resolve_window(window: u64) -> Result<String> {
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
        },
    )
}
