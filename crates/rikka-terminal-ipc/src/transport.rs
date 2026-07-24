//! Transport: the local-socket carrier for the framed IPC.
//!
//! `interprocess::local_socket` is one API over a Unix domain socket (Unix)
//! and a named pipe (Windows). Namespaced names avoid manual socket files and
//! cleanup. This module wraps a connected stream so consumers deal in
//! `Request`/`Response` (framing lives in `lib.rs`) and never touch
//! `interprocess` directly.

use crate::{Request, Response, read_frame, write_frame};
use interprocess::local_socket::{
    GenericNamespaced, Listener, ListenerOptions, Stream, prelude::*,
};
use std::io;
use std::time::{Duration, Instant};

// Maximum time without any forward progress in one framed operation.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(windows)]
const PIPE_WRITE_CHUNK: usize = 512;

/// The per-user endpoint name (namespaced): Windows maps it to `\\.\pipe\…`,
/// Linux to the abstract namespace, macOS to a temp path — no cleanup either way.
pub fn endpoint_name() -> String {
    let who = crate::security::user_key().unwrap_or_else(|_| "unknown".into());
    format!("rikka-terminal.{who}.sock")
}

/// A window process's own endpoint, for direct tab-move routing: every
/// window process listens here and advertises the name through
/// `register_window.endpoint`; senders resolve it via the monarch and attach
/// directly (the monarch never proxies handles).
pub fn window_endpoint_name(pid: u32) -> String {
    let who = crate::security::user_key().unwrap_or_else(|_| "unknown".into());
    format!("rikka-terminal.{who}.win.{pid}.sock")
}

/// A framed connection: `Request`/`Response` over one local-socket stream.
pub struct Conn(Stream);

impl Conn {
    pub fn new(stream: Stream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self(stream))
    }
    /// Client → monarch.
    pub fn send_request(&mut self, req: &Request) -> io::Result<()> {
        write_deadline(&mut self.0, req)
    }
    /// Client reads the monarch's reply.
    pub fn recv_response(&mut self) -> io::Result<Response> {
        read_deadline(&mut self.0).map(|(_v, r)| r)
    }
    /// Monarch reads a client request.
    pub fn recv_request(&mut self) -> io::Result<Request> {
        read_deadline(&mut self.0).map(|(_v, r)| r)
    }
    /// Monarch → client.
    pub fn send_response(&mut self, resp: &Response) -> io::Result<()> {
        write_deadline(&mut self.0, resp)
    }

    /// The connected peer's PID, when the OS can attest it (Windows:
    /// `GetNamedPipeClientProcessId` under interprocess's `peer_creds`).
    /// `None` = the OS could not attest it; a caller gating a capability on
    /// the peer's identity (the handle-transfer authorization) MUST treat
    /// `None` as untrusted and refuse.
    pub fn peer_pid(&self) -> Option<u32> {
        self.0
            .peer_creds()
            .ok()
            .and_then(|c| c.pid())
            .map(|p| p as u32)
    }
}

/// The monarch's listener. `bind` succeeds only for the first process to claim
/// the name; a later process gets an error and should become a client instead.
pub struct Monarch(Listener);

impl Monarch {
    pub fn bind(name: &str) -> io::Result<Self> {
        let _ = crate::security::capability()?;
        let ns = name.to_ns_name::<GenericNamespaced>()?;
        // Restrict the listener to the current user at the OS layer — the
        // name is only a rendezvous (see security.rs).
        let opts = crate::security::owner_only(ListenerOptions::new().name(ns))?;
        Ok(Self(opts.create_sync()?))
    }
    /// Blocking accept of the next client connection.
    pub fn accept(&self) -> io::Result<Conn> {
        match self.0.incoming().next() {
            Some(r) => {
                let conn = Conn::new(r?)?;
                verify_connected_peer(&conn, "peer")?;
                Ok(conn)
            }
            None => Err(io::Error::new(io::ErrorKind::Other, "listener closed")),
        }
    }
}

/// Connect to the running monarch. An `Err` (not-found / refused) means there
/// is no monarch — the caller should start one (cold start).
pub fn connect(name: &str) -> io::Result<Conn> {
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    let conn = Conn::new(Stream::connect(ns)?)?;
    verify_connected_peer(&conn, "server")?;
    Ok(conn)
}

fn verify_connected_peer(conn: &Conn, role: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let peer = conn.peer_pid().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("unattested IPC {role}"),
            )
        })?;
        if role == "server" {
            crate::security::verify_server_pid(peer)
        } else {
            crate::security::verify_client_pid(peer)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (conn, role);
        Ok(())
    }
}

struct DeadlineIo<'a> {
    inner: &'a mut Stream,
    deadline: Instant,
}

impl io::Read for DeadlineIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.inner.read(buf) {
                // A Windows named pipe in PIPE_NOWAIT mode can report a
                // successful zero-byte read while it is connected but has no
                // data yet. `read_exact` would mistake that for EOF.
                #[cfg(windows)]
                Ok(0) if !buf.is_empty() => {
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "IPC read timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "IPC read timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(n) if n != 0 => {
                    self.deadline = Instant::now() + IO_TIMEOUT;
                    return Ok(n);
                }
                result => return result,
            }
        }
    }
}

impl io::Write for DeadlineIo<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // interprocess creates Windows local-socket pipe buffers with a
        // 512-byte hint. In PIPE_NOWAIT mode, one WriteFileEx request larger
        // than the available buffer can make no partial progress at all, so
        // write_all must feed it requests that can actually fit.
        #[cfg(windows)]
        let buf = &buf[..buf.len().min(PIPE_WRITE_CHUNK)];
        loop {
            match self.inner.write(buf) {
                #[cfg(windows)]
                Ok(0) if !buf.is_empty() => {
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "IPC write timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "IPC write timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(n) if n != 0 => {
                    self.deadline = Instant::now() + IO_TIMEOUT;
                    return Ok(n);
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn read_deadline<T: serde::de::DeserializeOwned>(stream: &mut Stream) -> io::Result<(u32, T)> {
    read_frame(&mut DeadlineIo {
        inner: stream,
        deadline: Instant::now() + IO_TIMEOUT,
    })
}

fn write_deadline<T: serde::Serialize>(stream: &mut Stream, body: &T) -> io::Result<()> {
    write_frame(
        &mut DeadlineIo {
            inner: stream,
            deadline: Instant::now() + IO_TIMEOUT,
        },
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;
    use std::time::Duration;

    #[test]
    fn socket_request_response_roundtrip() {
        let name = format!("rikka-terminal-test-{}.sock", std::process::id());
        let server_name = name.clone();
        let server = std::thread::spawn(move || {
            let m = Monarch::bind(&server_name).expect("bind");
            let mut conn = m.accept().expect("accept");
            assert!(matches!(conn.recv_request().unwrap(), Request::Ping));
            conn.send_response(&Response::with_window(5)).unwrap();
        });

        // Retry the connect until the server has bound.
        let mut client = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            if let Ok(c) = connect(&name) {
                client = Some(c);
                break;
            }
        }
        let mut client = client.expect("connect to test monarch");
        client.send_request(&Request::Ping).unwrap();
        assert_eq!(client.recv_response().unwrap().window_id, Some(5));
        server.join().unwrap();
    }

    /// The hardened listener still accepts THIS process (same user) and
    /// attests its PID — the DACL restriction and peer attestation don't
    /// break the legitimate same-user path. A cross-user rejection can't be
    /// exercised from a single-user test, so this guards the "didn't lock
    /// ourselves out" direction; the boundary itself is the OS's job.
    #[test]
    fn hardened_listener_admits_same_user_and_attests_peer() {
        let name = format!("rikka-terminal-sec-{}.sock", std::process::id());
        let server_name = name.clone();
        let server = std::thread::spawn(move || {
            let m = Monarch::bind(&server_name).expect("bind hardened");
            let conn = m.accept().expect("accept");
            // The server sees the client's PID; same process here.
            conn.peer_pid()
        });
        let mut client = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            if let Ok(c) = connect(&name) {
                client = Some(c);
                break;
            }
        }
        let _client = client.expect("same-user connect must still succeed");
        let peer = server.join().unwrap();
        // PID attestation exists where the OS provides it — Windows
        // (GetNamedPipeClientProcessId) and Linux (SO_PEERCRED). macOS's
        // getpeereid attests uid/gid but no pid, so `peer_pid` correctly
        // answers None there (and the P3 Unix security gate keys on euid
        // instead — see TODO.md); same-user admission above is the
        // load-bearing assertion on that platform.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            peer,
            Some(std::process::id()),
            "server must attest the connecting PID"
        );
        #[cfg(target_os = "macos")]
        let _ = peer;
    }

    #[cfg(windows)]
    #[test]
    fn delayed_first_frame_is_not_mistaken_for_eof() {
        let name = format!("rikka-terminal-delay-{}.sock", std::process::id());
        let server_name = name.clone();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let m = Monarch::bind(&server_name).expect("bind");
            let mut conn = m.accept().expect("accept");
            accepted_tx.send(()).unwrap();
            assert!(matches!(conn.recv_request().unwrap(), Request::Ping));
            conn.send_response(&Response::ok()).unwrap();
        });

        let mut client = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            if let Ok(c) = connect(&name) {
                client = Some(c);
                break;
            }
        }
        let mut client = client.expect("connect to test monarch");
        accepted_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        client.send_request(&Request::Ping).unwrap();
        assert!(client.recv_response().unwrap().ok);
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn large_prepare_frame_reaches_reader_without_a_response() {
        const REPLAY_BYTES: usize = 96 * 1024;

        let name = format!("rikka-terminal-large-{}.sock", std::process::id());
        let server_name = name.clone();
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let (drop_tx, drop_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let m = Monarch::bind(&server_name).expect("bind");
            let mut conn = m.accept().expect("accept");
            let Request::PrepareAttach(args) = conn.recv_request().unwrap() else {
                panic!("expected PrepareAttach");
            };
            assert_eq!(
                crate::vt_from_state(&args.state).unwrap().len(),
                REPLAY_BYTES
            );
            read_tx.send(()).unwrap();
            drop_rx.recv().unwrap();
            // Deliberately drop without an IPC response: this test covers
            // delivery of the large first frame, not request/response.
        });

        let mut client = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            if let Ok(c) = connect(&name) {
                client = Some(c);
                break;
            }
        }
        let mut client = client.expect("connect to test monarch");
        let args = crate::AttachArgs {
            state: Some(crate::state_from_vt(&vec![b'x'; REPLAY_BYTES])),
            ..Default::default()
        };
        client
            .send_request(&Request::PrepareAttach(args))
            .expect("write large authenticated frame");
        read_rx.recv().expect("server read the complete frame");
        drop_tx.send(()).unwrap();
        server.join().unwrap();
    }
}
