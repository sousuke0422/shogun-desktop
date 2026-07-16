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

/// The per-user endpoint name (namespaced): Windows maps it to `\\.\pipe\…`,
/// Linux to the abstract namespace, macOS to a temp path — no cleanup either way.
pub fn endpoint_name() -> String {
    let who = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    format!("rikka-terminal.{who}.sock")
}

/// A window process's own endpoint, for direct tab-move routing: every
/// window process listens here and advertises the name through
/// `register_window.endpoint`; senders resolve it via the monarch and attach
/// directly (the monarch never proxies handles).
pub fn window_endpoint_name(pid: u32) -> String {
    let who = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    format!("rikka-terminal.{who}.win.{pid}.sock")
}

/// A framed connection: `Request`/`Response` over one local-socket stream.
pub struct Conn(Stream);

impl Conn {
    pub fn new(stream: Stream) -> Self {
        Self(stream)
    }
    /// Client → monarch.
    pub fn send_request(&mut self, req: &Request) -> io::Result<()> {
        write_frame(&mut self.0, req)
    }
    /// Client reads the monarch's reply.
    pub fn recv_response(&mut self) -> io::Result<Response> {
        read_frame(&mut self.0).map(|(_v, r)| r)
    }
    /// Monarch reads a client request.
    pub fn recv_request(&mut self) -> io::Result<Request> {
        read_frame(&mut self.0).map(|(_v, r)| r)
    }
    /// Monarch → client.
    pub fn send_response(&mut self, resp: &Response) -> io::Result<()> {
        write_frame(&mut self.0, resp)
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
        let ns = name.to_ns_name::<GenericNamespaced>()?;
        // Restrict the listener to the current user at the OS layer — the
        // name is only a rendezvous (see security.rs).
        let opts = crate::security::owner_only(ListenerOptions::new().name(ns))?;
        Ok(Self(opts.create_sync()?))
    }
    /// Blocking accept of the next client connection.
    pub fn accept(&self) -> io::Result<Conn> {
        match self.0.incoming().next() {
            Some(r) => r.map(Conn),
            None => Err(io::Error::new(io::ErrorKind::Other, "listener closed")),
        }
    }
}

/// Connect to the running monarch. An `Err` (not-found / refused) means there
/// is no monarch — the caller should start one (cold start).
pub fn connect(name: &str) -> io::Result<Conn> {
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    Stream::connect(ns).map(Conn)
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
        assert_eq!(
            peer,
            Some(std::process::id()),
            "server must attest the connecting PID"
        );
    }
}
