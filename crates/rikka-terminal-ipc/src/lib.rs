//! Shared local-IPC wire contract for RikkaTerminal.
//!
//! Used by three binaries — `rikka-terminal` (window process + monarch), `rt`
//! (launcher), and `rikka-handoff` (the Windows default-terminal shim). This
//! crate is the wire contract ONLY: message types, JSON (de)serialization, and
//! length-prefixed framing. The transport (an `interprocess` local socket) and
//! the platform handle transfer (`DuplicateHandle` / `SCM_RIGHTS`) live in the
//! consumers. Design: `crates/rikka-terminal/IPC.md`.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub mod security;
pub mod transport;

/// Protocol version, carried in every frame's envelope (`{ "v": … }`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Reject frames larger than this — guards against a corrupt length prefix.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Where a new session should land.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A brand-new window — its own OS process (the crash-isolation default).
    New,
    /// A tab in the window with this id (explicit `-w <id>`, or a drag-merge).
    Window(u64),
}

impl Default for Target {
    fn default() -> Self {
        Target::New
    }
}

/// `spawn` payload — launcher path, strings only, all platforms.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnArgs {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub argv: Vec<String>,
    /// `rt -w`: open the tabs in an EXISTING window instead of a new one.
    /// `Some(0)` = any window ("last"); `Some(id)` = that window. The monarch
    /// rewrites 0/pid forms to the concrete per-window id before forwarding
    /// to the window's own socket. Absent = a fresh window (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub hold: bool,
    #[serde(default)]
    pub target: Target,
}

/// Handle values valid in the *sender's* process. The receiver pulls them
/// across out-of-band (`DuplicateHandle` on Windows, `SCM_RIGHTS` on Unix).
/// `i64` so a Windows HANDLE or a Unix fd both fit; `0` means "not present".
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Handles {
    pub input: i64,
    pub output: i64,
    #[serde(default)]
    pub signal: i64,
    #[serde(default)]
    pub reference: i64,
    #[serde(default)]
    pub server: i64,
    #[serde(default)]
    pub client: i64,
    /// A local ConPTY tab-move also carries the pseudoconsole + shell process.
    #[serde(default)]
    pub hpcon: i64,
    #[serde(default)]
    pub shell: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupInfo {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

/// `attach` payload — an OS handoff, a new window from a PTY, or a cross-window
/// tab move. `pid` lets the receiver open the sender for the handle pull.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct AttachArgs {
    pub pid: u32,
    pub handles: Handles,
    #[serde(default)]
    pub startup: StartupInfo,
    /// Serialized grid + scrollback for a tab-move; absent for an OS handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
    #[serde(default)]
    pub elevated: bool,
    #[serde(default)]
    pub target: Target,
    /// Screen-pixel cursor position of a drag-merge drop: the receiver
    /// inserts the tab at the strip position under it. Absent = append
    /// (Ctrl+Shift+X moves, CLI attaches, OS handoffs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_at: Option<(i32, i32)>,
    /// The tab's per-profile color palette, riding the move so the tab keeps
    /// its colors on the far side: 19 packed `0xRRGGBB` values (background,
    /// foreground, selection, then the 16 ANSI colors) — see the engine's
    /// `theme::Palette::to_wire`. Absent = the tab wears the receiver's
    /// default theme (old senders / unthemed tabs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<u32>>,
}

/// Wrap a tab-move screen replay (VT bytes, see core's `replay_bytes`) as
/// the `attach.state` value: `{ "vt_b64": … }`.
pub fn state_from_vt(vt: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({
        "vt_b64": base64::engine::general_purpose::STANDARD.encode(vt)
    })
}

/// The replayable VT bytes out of an `attach.state`, when it carries any.
/// `None` for an absent state, an unknown shape, or corrupt base64 — the
/// receiver then simply starts blank, which is the v1 behavior.
pub fn vt_from_state(state: &Option<serde_json::Value>) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let b64 = state.as_ref()?.get("vt_b64")?.as_str()?;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// One carried image of a tab move: `(id, cols, rows, png_bytes)` — the
/// engine's `pty_handoff::image_payloads` shape.
pub type ImagePayload = (u32, u16, u16, Vec<u8>);

/// The full `attach.state` value for a tab move: the screen replay plus the
/// image store (`{ "vt_b64": …, "images": [{id,c,r,png_b64}…] }`). The
/// `images` key is omitted when empty, so old receivers see exactly the
/// `state_from_vt` shape they already understand.
pub fn state_from_parts(vt: &[u8], images: &[ImagePayload]) -> serde_json::Value {
    use base64::Engine as _;
    let mut v = state_from_vt(vt);
    if !images.is_empty() {
        let arr: Vec<serde_json::Value> = images
            .iter()
            .map(|(id, c, r, png)| {
                serde_json::json!({
                    "id": id,
                    "c": c,
                    "r": r,
                    "png_b64": base64::engine::general_purpose::STANDARD.encode(png),
                })
            })
            .collect();
        v["images"] = serde_json::Value::Array(arr);
    }
    v
}

/// The carried images out of an `attach.state`. Malformed entries are
/// skipped (fail open — that image simply does not survive, like any image
/// that missed the sender's budget).
pub fn images_from_state(state: &Option<serde_json::Value>) -> Vec<ImagePayload> {
    use base64::Engine as _;
    let Some(arr) = state.as_ref().and_then(|s| s.get("images")?.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            let id = u32::try_from(e.get("id")?.as_u64()?).ok()?;
            let c = u16::try_from(e.get("c")?.as_u64()?).ok()?;
            let r = u16::try_from(e.get("r")?.as_u64()?).ok()?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(e.get("png_b64")?.as_str()?)
                .ok()?;
            Some((id, c, r, png))
        })
        .collect()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RegisterWindow {
    pub pid: u32,
    pub window_id: u64,
    pub endpoint: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u64,
    #[serde(default)]
    pub title: Option<String>,
    /// Owning process — distinguishes "another window of MY process" (use
    /// the in-process merge) from a real cross-process target, now that ids
    /// are per-window rather than per-pid.
    #[serde(default)]
    pub pid: u32,
}

/// A request from a client (`rt` / `rikka-handoff` / a window process) to the
/// monarch. Serializes to `{ "op": "<snake>", … }`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Spawn(SpawnArgs),
    Attach(AttachArgs),
    RegisterWindow(RegisterWindow),
    /// Replace ALL of `pid`'s directory entries with `windows` — the
    /// heartbeat form for per-window addressing: one process, every live
    /// window, in one atomic swap (closed windows disappear with it).
    RegisterWindows {
        pid: u32,
        windows: Vec<RegisterWindow>,
    },
    ListWindows,
    /// Ask the monarch for a window's own socket endpoint — the sender then
    /// connects there directly and `attach`es (direct tab-move routing; the
    /// monarch never proxies handles).
    ResolveWindow {
        window: u64,
    },
}

/// The monarch's reply.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<WindowInfo>>,
    /// `resolve_window` answer: the window's own socket endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Default::default()
        }
    }
    pub fn with_window(id: u64) -> Self {
        Self {
            ok: true,
            window_id: Some(id),
            ..Default::default()
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

// ── framing: [u32 LE length][JSON of { "v": …, …body }] ──────────────────────

#[derive(Deserialize)]
struct Envelope<T> {
    v: u32,
    #[serde(flatten)]
    body: T,
}

#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    v: u32,
    #[serde(flatten)]
    body: &'a T,
}

fn json_err(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Write `body` inside a `{ "v": PROTOCOL_VERSION, … }` envelope as one
/// length-prefixed frame.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, body: &T) -> io::Result<()> {
    let json = serde_json::to_vec(&EnvelopeRef {
        v: PROTOCOL_VERSION,
        body,
    })
    .map_err(json_err)?;
    let len = u32::try_from(json.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&json)?;
    w.flush()
}

/// Read one length-prefixed frame; returns `(envelope version, body)`.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<(u32, T)> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME_BYTES",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let env: Envelope<T> = serde_json::from_slice(&buf).map_err(json_err)?;
    Ok((env.v, env.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn spawn_json_is_flat_and_versioned() {
        let req = Request::Spawn(SpawnArgs {
            cwd: Some("/x".into()),
            argv: vec!["pwsh".into()],
            ..Default::default()
        });
        let s = serde_json::to_string(&EnvelopeRef {
            v: PROTOCOL_VERSION,
            body: &req,
        })
        .unwrap();
        assert!(s.contains("\"v\":1"), "{s}");
        assert!(s.contains("\"op\":\"spawn\""), "{s}");
        assert!(s.contains("\"target\":\"new\""), "{s}");
    }

    #[test]
    fn attach_target_window_frame_roundtrips() {
        let req = Request::Attach(AttachArgs {
            pid: 42,
            handles: Handles {
                input: 3,
                output: 4,
                ..Default::default()
            },
            target: Target::Window(7),
            ..Default::default()
        });
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let (v, got): (u32, Request) = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(v, PROTOCOL_VERSION);
        assert_eq!(got, req);
    }

    #[test]
    fn response_roundtrips() {
        let resp = Response::with_window(9);
        let mut buf = Vec::new();
        write_frame(&mut buf, &resp).unwrap();
        let (_, got): (u32, Response) = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(got, resp);
        assert_eq!(got.window_id, Some(9));
    }

    #[test]
    fn unit_requests_roundtrip() {
        for req in [Request::Ping, Request::ListWindows] {
            let mut buf = Vec::new();
            write_frame(&mut buf, &req).unwrap();
            let (_, got): (u32, Request) = read_frame(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(got, req);
        }
    }

    #[test]
    fn resolve_window_roundtrips_with_endpoint_reply() {
        let req = Request::ResolveWindow { window: 7 };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let (_, got): (u32, Request) = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(got, req);
        let json = String::from_utf8(buf[4..].to_vec()).unwrap();
        assert!(json.contains("\"op\":\"resolve_window\""), "{json}");

        let resp = Response {
            ok: true,
            endpoint: Some("rikka-terminal.u.win.42.sock".into()),
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &resp).unwrap();
        let (_, got): (u32, Response) = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(
            got.endpoint.as_deref(),
            Some("rikka-terminal.u.win.42.sock")
        );
    }

    #[test]
    fn state_vt_roundtrips_through_a_frame() {
        let vt = b"\x1b[?1049h\x1b[1;31mRED \xe5\xad\x97".to_vec();
        let req = Request::Attach(AttachArgs {
            pid: 1,
            handles: Handles {
                input: 3,
                output: 4,
                ..Default::default()
            },
            state: Some(state_from_vt(&vt)),
            ..Default::default()
        });
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let (_, got): (u32, Request) = read_frame(&mut Cursor::new(&buf)).unwrap();
        let Request::Attach(a) = got else {
            panic!("attach expected")
        };
        assert_eq!(vt_from_state(&a.state), Some(vt));
        assert_eq!(vt_from_state(&None), None, "absent state stays blank");
        assert_eq!(
            vt_from_state(&Some(serde_json::json!({ "vt_b64": "!!!" }))),
            None,
            "corrupt base64 degrades to blank, never errors"
        );
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let e = read_frame::<_, Request>(&mut Cursor::new(&buf)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}
