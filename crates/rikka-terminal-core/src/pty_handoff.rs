//! Session assembly over an *inherited* ConPTY — the Windows default-terminal
//! handoff (`ITerminalHandoff3`) and, later, cross-window tab moves.
//!
//! Unlike [`crate::pty_session::build_terminal_session`]'s usual callers, the
//! embedder spawns nothing here: the console session already exists, and this
//! module merely wraps the handles it was given —
//!
//! - `input`: our write end of the console's input pipe (keystrokes go in),
//! - `output`: our read end of the console's VT output pipe,
//! - `signal`: the ConPTY signal pipe; resizes are written to it using
//!   winconpty's wire format (see [`resize_signal_packet`]),
//! - `keepalive`: the server/client *process* handles from the handoff, held
//!   for later bookkeeping (exit codes, client names); process handles keep
//!   nothing alive. They ride in the resizer, whose `Arc` drops with the
//!   [`TerminalSession`]. The ConDrv *reference* handle must NOT ride here:
//!   as long as it exists conhost keeps serving even after its last client
//!   left (winconpty.h), so an exited shell would never break our output
//!   pipe and the tab would linger forever. Upstream drops it the moment the
//!   connection starts (`ConptyConnection::Start` →
//!   `ConptyReleasePseudoConsole`); callers here do the same.
//!
//! Who created the handles is the caller's business (the monarch pulls them
//! from the handoff shim with `DuplicateHandle`); by the time they reach this
//! module they are plain owned handles in this process.
#![cfg(windows)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::OwnedHandle;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::FairMutex;

use crate::{PtyResizer, TerminalSession, pty_session::build_terminal_session};

/// The handles a default-terminal handoff (or a tab move) delivers, already
/// owned by this process. `keepalive` is closed when the session drops.
pub struct HandoffPty {
    /// Write end: terminal → console input.
    pub input: OwnedHandle,
    /// Read end: console VT output → terminal.
    pub output: OwnedHandle,
    /// ConPTY signal pipe (write end). `None` = resize requests are dropped.
    pub signal: Option<OwnedHandle>,
    /// Server/client process handles held for the session's lifetime —
    /// never the ConDrv reference handle (see module docs).
    pub keepalive: Vec<OwnedHandle>,
}

/// winconpty's `PTY_SIGNAL_RESIZE_WINDOW` message: three little-endian u16s —
/// the signal id (8), then the new column and row counts. ConPTY's signal
/// pipe carries no pixel sizes.
pub fn resize_signal_packet(cols: u16, rows: u16) -> [u8; 6] {
    const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;
    let mut buf = [0u8; 6];
    buf[0..2].copy_from_slice(&PTY_SIGNAL_RESIZE_WINDOW.to_le_bytes());
    buf[2..4].copy_from_slice(&cols.to_le_bytes());
    buf[4..6].copy_from_slice(&rows.to_le_bytes());
    buf
}

/// Resizer over the ConPTY signal pipe. Doubles as the keepalive anchor (see
/// module docs): the handoff's lifetime handles drop with the session.
struct SignalResizer {
    signal: Option<FairMutex<File>>,
    _keepalive: Vec<OwnedHandle>,
}

impl PtyResizer for SignalResizer {
    fn resize(&self, cols: u16, rows: u16, _pw: u16, _ph: u16) -> Result<()> {
        if let Some(signal) = &self.signal {
            let mut f = signal.lock();
            f.write_all(&resize_signal_packet(cols, rows))?;
            f.flush()?;
        }
        Ok(())
    }
}

/// Build a live [`TerminalSession`] over an inherited ConPTY. The console
/// closing its output end reads as EOF and flips the session disconnected,
/// exactly like a spawned shell exiting.
pub fn build_handoff_session(
    cols: u16,
    rows: u16,
    pty: HandoffPty,
    xtversion_identity: &str,
) -> Result<TerminalSession> {
    let reader: Box<dyn Read + Send> = Box::new(File::from(pty.output));
    let writer: Box<dyn Write + Send> = Box::new(File::from(pty.input));
    let resizer: Arc<dyn PtyResizer> = Arc::new(SignalResizer {
        signal: pty.signal.map(|h| FairMutex::new(File::from(h))),
        _keepalive: pty.keepalive,
    });
    build_terminal_session(
        cols,
        rows,
        reader,
        Arc::new(FairMutex::new(writer)),
        resizer,
        xtversion_identity,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_eof(session: &TerminalSession) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader thread must reach EOF");
    }

    #[test]
    fn resize_packet_is_winconpty_wire_format() {
        assert_eq!(
            resize_signal_packet(120, 40),
            [8, 0, 120, 0, 40, 0],
            "u16 LE triple: signal id 8, cols, rows"
        );
        assert_eq!(resize_signal_packet(0x1234, 0x00FF)[2..4], [0x34, 0x12]);
    }

    /// Console-side output flows into the grid; terminal-side input lands on
    /// the console's end of the input pipe; EOF disconnects — the same
    /// lifecycle a spawned PTY has, but over inherited pipe handles.
    #[test]
    fn handoff_session_round_trips_both_pipes() {
        let (out_read, mut out_write) = std::io::pipe().expect("output pipe");
        let (mut in_read, in_write) = std::io::pipe().expect("input pipe");
        let session = build_handoff_session(
            10,
            2,
            HandoffPty {
                input: in_write.into(),
                output: out_read.into(),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-terminal 0.0",
        )
        .expect("session over inherited pipes");

        out_write.write_all(b"hi").unwrap();
        drop(out_write);
        wait_for_eof(&session);
        let row0: String = session.snapshot.lock().cells[0]
            .iter()
            .map(|c| c.c)
            .collect();
        assert!(row0.starts_with("hi"), "got {row0:?}");

        session.send_bytes(b"ls\r");
        let mut buf = [0u8; 3];
        in_read.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ls\r");
    }

    /// `TerminalSession::resize` reaches the signal pipe as a winconpty
    /// resize packet; a session without a signal pipe swallows resizes.
    #[test]
    fn resize_goes_out_over_the_signal_pipe() {
        let (out_read, out_write) = std::io::pipe().expect("output pipe");
        let (_in_read, in_write) = std::io::pipe().expect("input pipe");
        let (mut sig_read, sig_write) = std::io::pipe().expect("signal pipe");
        let session = build_handoff_session(
            10,
            2,
            HandoffPty {
                input: in_write.into(),
                output: out_read.into(),
                signal: Some(sig_write.into()),
                keepalive: Vec::new(),
            },
            "test-terminal 0.0",
        )
        .expect("session with signal pipe");

        session.resize(120, 40, (8.0, 16.0));
        let mut buf = [0u8; 6];
        sig_read.read_exact(&mut buf).unwrap();
        assert_eq!(buf, resize_signal_packet(120, 40));
        drop(out_write);
    }
}
