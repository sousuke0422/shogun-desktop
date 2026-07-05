//! Kitty graphics protocol (subset) — inline images for yazi and friends.
//!
//! Scope (2026-07-06): Unicode-placeholder virtual placements only (`U=1`),
//! direct base64 transmission (`t=d`), formats `f=24/32/100` (+`o=z` zlib),
//! queries (`a=q`) and deletes (`a=d`). Classic cursor-anchored placements
//! are NOT implemented: placeholder cells (U+10EEEE + row/column combining
//! diacritics, image id in the fg color) travel through alacritty's grid
//! like ordinary text, so scrollback / alt-screen / resize behave for free —
//! the same trick as OSC 8: keep state in cells, not in a side table.
//!
//! Like OSC 9;4 (see `progress.rs`), the vte stack swallows APC without
//! exposing it, so a passive [`ApcScanner`] in the PTY reader thread collects
//! `ESC _ G … ESC \` before `Processor::advance` — the bytes still flow into
//! the real parser, which ignores them.
//!
//! Protocol reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::collections::{HashMap, VecDeque};
use std::io::Read as _;
use std::sync::{Arc, OnceLock};

use alacritty_terminal::sync::FairMutex;
use base64::Engine as _;
use gpui::RenderImage;
use image::{Frame, RgbaImage};

/// The placeholder character (kitty spec): each cell showing an image tile.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Decoded-image memory the store may hold before evicting oldest entries
/// (kitty's own default quota is 320 MiB).
const MAX_STORE_BYTES: usize = 256 * 1024 * 1024;

/// One APC chunk. The spec caps payloads at 4096 base64 chars; be lenient
/// but bounded — an overflowing chunk is dropped whole.
const MAX_APC_BYTES: usize = 8 * 1024 * 1024;

/// Accumulated base64 across `m=1` chunks of one transmission.
const MAX_TRANSMISSION_BYTES: usize = 64 * 1024 * 1024;

// ─── Row/column diacritics ──────────────────────────────────────────────────

/// Combining characters marking a placeholder's (row, column) within its
/// virtual placement, in kitty's table order (index = encoded number).
/// Source: kitty gen/rowcolumn-diacritics.txt (Unicode 6.0 combining chars).
pub(crate) const ROWCOL_DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// Number encoded by a row/column diacritic, if `c` is one.
pub fn diacritic_index(c: char) -> Option<u16> {
    static MAP: OnceLock<HashMap<char, u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        ROWCOL_DIACRITICS
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i as u16))
            .collect()
    })
    .get(&c)
    .copied()
}

// ─── Placeholder cells ──────────────────────────────────────────────────────

/// A grid cell that shows one tile of a virtual image placement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlaceholderCell {
    /// Image id: fg color (lower 24 bits) + optional third diacritic (MSB).
    pub id: u32,
    /// Tile row within the placement (first diacritic).
    pub row: u16,
    /// Tile column within the placement (second diacritic).
    pub col: u16,
}

/// Decode a placeholder cell from its raw fg color (24-bit RGB or palette
/// index — NOT resolved through the palette; the value IS the id) and its
/// combining diacritics. Missing diacritics continue from the cell to the
/// left, per the spec's run-length shortcut.
pub fn decode_placeholder(
    fg24: u32,
    zerowidth: &[char],
    prev: Option<PlaceholderCell>,
) -> Option<PlaceholderCell> {
    let mut marks = zerowidth.iter().copied().map(diacritic_index);
    let row = marks.next().flatten();
    let col = marks.next().flatten();
    let id_msb = marks.next().flatten();
    let id = fg24 | u32::from(id_msb.unwrap_or(0) & 0xff) << 24;
    match (row, col) {
        (Some(r), Some(c)) => Some(PlaceholderCell { id, row: r, col: c }),
        // Diacritics omitted: continue the run from the left neighbor, which
        // must share the (pre-MSB) id.
        (Some(r), None) => {
            let p = prev.filter(|p| p.row == r)?;
            Some(PlaceholderCell {
                id: p.id,
                row: r,
                col: p.col + 1,
            })
        }
        (None, _) => {
            let p = prev?;
            Some(PlaceholderCell {
                id: p.id,
                row: p.row,
                col: p.col + 1,
            })
        }
    }
}

// ─── Image store ────────────────────────────────────────────────────────────

/// A transmitted image plus its virtual-placement grid size in cells.
#[derive(Clone)]
pub struct StoredImage {
    pub image: Arc<RenderImage>,
    /// Placement size in cells (`c=` / `r=`); 0 until a placement arrives.
    pub cols: u16,
    pub rows: u16,
}

#[derive(Default)]
struct StoreInner {
    map: HashMap<u32, StoredImage>,
    /// Insertion order for size-cap eviction.
    order: VecDeque<u32>,
    bytes: usize,
}

/// Images shared between the PTY reader thread (writer) and the renderer.
pub struct KittyImageStore {
    inner: FairMutex<StoreInner>,
}

impl Default for KittyImageStore {
    fn default() -> Self {
        Self {
            inner: FairMutex::new(StoreInner::default()),
        }
    }
}

impl KittyImageStore {
    pub fn get(&self, id: u32) -> Option<StoredImage> {
        self.inner.lock().map.get(&id).cloned()
    }

    /// True when no image is stored (the visibility fast path is
    /// `GridSnapshot::has_images`; this is for tests).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().map.is_empty()
    }

    fn insert(&self, id: u32, img: StoredImage) {
        let size = image_bytes(&img.image);
        let mut inner = self.inner.lock();
        if let Some(old) = inner.map.remove(&id) {
            inner.bytes -= image_bytes(&old.image);
            inner.order.retain(|&i| i != id);
        }
        while inner.bytes + size > MAX_STORE_BYTES {
            let Some(evict) = inner.order.pop_front() else {
                break;
            };
            if let Some(old) = inner.map.remove(&evict) {
                inner.bytes -= image_bytes(&old.image);
            }
        }
        inner.bytes += size;
        inner.order.push_back(id);
        inner.map.insert(id, img);
    }

    fn set_placement(&self, id: u32, cols: u16, rows: u16) -> bool {
        let mut inner = self.inner.lock();
        match inner.map.get_mut(&id) {
            Some(img) => {
                img.cols = cols;
                img.rows = rows;
                true
            }
            None => false,
        }
    }

    fn remove(&self, id: u32) {
        let mut inner = self.inner.lock();
        if let Some(old) = inner.map.remove(&id) {
            inner.bytes -= image_bytes(&old.image);
            inner.order.retain(|&i| i != id);
        }
    }

    fn clear(&self) {
        *self.inner.lock() = StoreInner::default();
    }
}

fn image_bytes(img: &RenderImage) -> usize {
    let size = img.size(0);
    u32::from(size.width) as usize * u32::from(size.height) as usize * 4
}

// ─── APC scanner ────────────────────────────────────────────────────────────

/// Passive `ESC _ … ESC \` collector, chunk-boundary safe (same discipline
/// as `progress::OscScanner`). Emits the payload of every APC that starts
/// with `G`; everything else is ignored. The bytes are NOT consumed — the
/// real vte parser sees the same stream and discards the APC itself.
pub struct ApcScanner {
    state: ApcState,
    buf: Vec<u8>,
    overflow: bool,
}

enum ApcState {
    Ground,
    Esc,
    Apc,
    ApcEsc,
}

impl ApcScanner {
    pub fn new() -> Self {
        Self {
            state: ApcState::Ground,
            buf: Vec::new(),
            overflow: false,
        }
    }

    pub fn advance(&mut self, byte: u8) -> Option<Vec<u8>> {
        match self.state {
            ApcState::Ground => {
                if byte == 0x1b {
                    self.state = ApcState::Esc;
                }
                None
            }
            ApcState::Esc => {
                self.state = if byte == b'_' {
                    self.buf.clear();
                    self.overflow = false;
                    ApcState::Apc
                } else if byte == 0x1b {
                    ApcState::Esc
                } else {
                    ApcState::Ground
                };
                None
            }
            ApcState::Apc => {
                if byte == 0x1b {
                    self.state = ApcState::ApcEsc;
                } else if self.buf.len() < MAX_APC_BYTES {
                    self.buf.push(byte);
                } else {
                    self.overflow = true;
                }
                None
            }
            ApcState::ApcEsc => {
                if byte == b'\\' {
                    // ST — APC complete.
                    self.state = ApcState::Ground;
                    let done = std::mem::take(&mut self.buf);
                    (!self.overflow && done.first() == Some(&b'G')).then_some(done)
                } else if byte == 0x1b {
                    // Stray ESC inside APC: stay armed for ST.
                    None
                } else {
                    // Not a terminator — the ESC belonged to the payload
                    // (never happens with base64 data, but stay lossless).
                    if self.buf.len() + 2 <= MAX_APC_BYTES {
                        self.buf.push(0x1b);
                        self.buf.push(byte);
                    } else {
                        self.overflow = true;
                    }
                    self.state = ApcState::Apc;
                    None
                }
            }
        }
    }
}

// ─── Command parsing ────────────────────────────────────────────────────────

/// One parsed `ESC_G` control set. Unlisted keys are ignored.
#[derive(Default, Clone, Copy, Debug)]
struct GCmd {
    /// `a=` action (byte; 0 = absent = transmit per spec).
    action: u8,
    /// `f=` pixel format: 24 (RGB), 32 (RGBA), 100 (PNG). Default 32.
    format: u32,
    /// `t=` transmission medium (only `d`, direct, is supported).
    medium: u8,
    /// `s=`/`v=` width/height in px (required for f=24/32).
    width: u32,
    height: u32,
    /// `i=` image id / `I=` client-chosen number (echoed in responses).
    id: u32,
    number: u32,
    /// `m=1` — more chunks follow.
    more: bool,
    /// `q=` 1: suppress OK, 2: suppress errors too.
    quiet: u8,
    /// `U=1` — create a virtual (Unicode placeholder) placement.
    unicode: bool,
    /// `c=`/`r=` placement size in cells.
    cols: u32,
    rows: u32,
    /// `d=` delete target selector.
    delete: u8,
    /// `o=z` — payload is zlib-compressed.
    zlib: bool,
}

fn parse_controls(controls: &str) -> GCmd {
    let mut cmd = GCmd {
        format: 32,
        medium: b'd',
        ..GCmd::default()
    };
    for kv in controls.split(',') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let int = || v.parse::<u32>().unwrap_or(0);
        let byte = || v.bytes().next().unwrap_or(0);
        match k {
            "a" => cmd.action = byte(),
            "f" => cmd.format = int(),
            "t" => cmd.medium = byte(),
            "s" => cmd.width = int(),
            "v" => cmd.height = int(),
            "i" => cmd.id = int(),
            "I" => cmd.number = int(),
            "m" => cmd.more = int() == 1,
            "q" => cmd.quiet = int() as u8,
            "U" => cmd.unicode = int() == 1,
            "c" => cmd.cols = int(),
            "r" => cmd.rows = int(),
            "d" => cmd.delete = byte(),
            "o" => cmd.zlib = byte() == b'z',
            _ => {}
        }
    }
    cmd
}

// ─── Protocol driver ────────────────────────────────────────────────────────

struct Pending {
    cmd: GCmd,
    b64: Vec<u8>,
}

/// Per-session protocol state, owned by the PTY reader thread. `apply` maps
/// each APC payload to an optional response to write back to the PTY.
pub struct KittyGraphics {
    store: Arc<KittyImageStore>,
    pending: Option<Pending>,
}

impl KittyGraphics {
    pub fn new(store: Arc<KittyImageStore>) -> Self {
        Self {
            store,
            pending: None,
        }
    }

    pub fn apply(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let resp = self.apply_inner(payload);
        debug_log(payload, resp.as_deref());
        resp
    }

    fn apply_inner(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let payload = payload.strip_prefix(b"G")?;
        let split = payload.iter().position(|&b| b == b';');
        let (controls, data) = match split {
            Some(i) => (&payload[..i], &payload[i + 1..]),
            None => (payload, [].as_slice()),
        };
        let cmd = parse_controls(std::str::from_utf8(controls).ok()?);

        // Continuation chunk of an in-flight chunked transmission. The spec
        // says later chunks SHOULD carry only `m=`, but real clients also
        // repeat i/q/a — so accept any chunk that doesn't contradict the
        // pending transmission (different action or different image id).
        if let Some(pending) = &mut self.pending
            && (cmd.action == 0 || cmd.action == pending.cmd.action)
            && (cmd.id == 0 || cmd.id == pending.cmd.id)
        {
            if pending.b64.len() + data.len() > MAX_TRANSMISSION_BYTES {
                self.pending = None;
                return respond(&cmd, "ENOMEM:transmission too large");
            }
            pending.b64.extend_from_slice(data);
            if cmd.more {
                return None;
            }
            let done = self.pending.take().unwrap();
            return self.finish_transmission(done);
        }

        match cmd.action {
            b'q' => query_response(&cmd),
            // a=t transmit / a=T transmit+display; 0 = absent = transmit.
            0 | b't' | b'T' => {
                if cmd.medium != b'd' {
                    return respond(&cmd, "ENOTSUPPORTED:only t=d");
                }
                let pending = Pending {
                    cmd,
                    b64: data.to_vec(),
                };
                if cmd.more {
                    self.pending = Some(pending);
                    None
                } else {
                    self.finish_transmission(pending)
                }
            }
            b'p' => {
                if !cmd.unicode {
                    return respond(&cmd, "ENOTSUPPORTED:only virtual placements");
                }
                if self
                    .store
                    .set_placement(cmd.id, cmd.cols as u16, cmd.rows as u16)
                {
                    respond(&cmd, "OK")
                } else {
                    respond(&cmd, "ENOENT:no such image")
                }
            }
            b'd' => {
                // Lower/upper case selectors behave the same here — we keep
                // no cursor placements, only image data.
                match cmd.delete.to_ascii_lowercase() {
                    b'i' | b'n' if cmd.id != 0 => self.store.remove(cmd.id),
                    _ => self.store.clear(),
                }
                None
            }
            _ => respond(&cmd, "ENOTSUPPORTED:unknown action"),
        }
    }

    fn finish_transmission(&mut self, pending: Pending) -> Option<Vec<u8>> {
        let cmd = pending.cmd;
        match decode_transmission(&cmd, &pending.b64) {
            Ok(image) => {
                self.store.insert(
                    cmd.id,
                    StoredImage {
                        image: Arc::new(image),
                        cols: cmd.cols as u16,
                        rows: cmd.rows as u16,
                    },
                );
                respond(&cmd, "OK")
            }
            Err(msg) => respond(&cmd, &msg),
        }
    }
}

/// `a=q` — a capability probe: answer OK iff we could store the described
/// image. yazi & co. treat any non-OK (or no) response as "no kitty support".
fn query_response(cmd: &GCmd) -> Option<Vec<u8>> {
    let msg = if cmd.medium != b'd' {
        "ENOTSUPPORTED:only t=d"
    } else if !matches!(cmd.format, 24 | 32 | 100) {
        "ENOTSUPPORTED:only f=24/32/100"
    } else {
        "OK"
    };
    respond(cmd, msg)
}

/// Build `ESC_G i=…[,I=…];MSG ESC\`, honoring the quiet level.
fn respond(cmd: &GCmd, msg: &str) -> Option<Vec<u8>> {
    let suppress = if msg == "OK" {
        cmd.quiet >= 1
    } else {
        cmd.quiet >= 2
    };
    if suppress {
        return None;
    }
    let mut out = format!("\x1b_Gi={}", cmd.id);
    if cmd.number != 0 {
        out.push_str(&format!(",I={}", cmd.number));
    }
    out.push(';');
    out.push_str(msg);
    out.push_str("\x1b\\");
    Some(out.into_bytes())
}

/// Field diagnostics: set `SHOGUN_KITTY_LOG=<path>` before launching to
/// append every APC command (controls only, payload elided) and our response
/// to that file. Off (and cost-free past one env lookup) otherwise.
fn debug_log(payload: &[u8], resp: Option<&[u8]>) {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var("SHOGUN_KITTY_LOG").ok()) else {
        return;
    };
    let controls_len = payload
        .iter()
        .position(|&b| b == b';')
        .unwrap_or(payload.len().min(120));
    let line = format!(
        "apc {} (+{}B payload) -> {}\n",
        String::from_utf8_lossy(&payload[..controls_len]),
        payload.len().saturating_sub(controls_len),
        resp.map(|r| String::from_utf8_lossy(r).into_owned())
            .unwrap_or_else(|| "(no response)".into()),
    );
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn decode_transmission(cmd: &GCmd, b64: &[u8]) -> Result<RenderImage, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "EINVAL:bad base64".to_string())?;
    let raw = if cmd.zlib {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(raw.as_slice())
            .read_to_end(&mut out)
            .map_err(|_| "EINVAL:bad zlib stream".to_string())?;
        out
    } else {
        raw
    };

    let mut rgba: RgbaImage = match cmd.format {
        100 => image::load_from_memory(&raw)
            .map_err(|e| format!("EINVAL:{e}"))?
            .into_rgba8(),
        32 => {
            let (w, h) = (cmd.width, cmd.height);
            if (w * h * 4) as usize != raw.len() {
                return Err("EINVAL:f=32 size mismatch".into());
            }
            RgbaImage::from_raw(w, h, raw).ok_or("EINVAL:bad dimensions")?
        }
        24 => {
            let (w, h) = (cmd.width, cmd.height);
            if (w * h * 3) as usize != raw.len() {
                return Err("EINVAL:f=24 size mismatch".into());
            }
            let mut buf = Vec::with_capacity((w * h * 4) as usize);
            for px in raw.chunks_exact(3) {
                buf.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
            }
            RgbaImage::from_raw(w, h, buf).ok_or("EINVAL:bad dimensions")?
        }
        _ => return Err("ENOTSUPPORTED:only f=24/32/100".into()),
    };

    // RGBA → BGRA, matching what gpui's atlas expects (same swizzle the img
    // element performs in crates/gpui/src/elements/img.rs).
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(RenderImage::new(vec![Frame::new(rgba)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(scanner: &mut ApcScanner, bytes: &[u8]) -> Vec<Vec<u8>> {
        bytes.iter().filter_map(|&b| scanner.advance(b)).collect()
    }

    #[test]
    fn apc_scanner_extracts_graphics_payload() {
        let mut s = ApcScanner::new();
        let got = scan(&mut s, b"ab\x1b_Ga=q,i=3;AAAA\x1b\\cd");
        assert_eq!(got, vec![b"Ga=q,i=3;AAAA".to_vec()]);
    }

    #[test]
    fn apc_scanner_survives_chunk_split_and_ignores_osc() {
        let mut s = ApcScanner::new();
        assert!(scan(&mut s, b"\x1b]0;title\x07\x1b_Ga=q,i=1;A").is_empty());
        let got = scan(&mut s, b"BCD\x1b\\");
        assert_eq!(got, vec![b"Ga=q,i=1;ABCD".to_vec()]);
    }

    #[test]
    fn query_gets_ok_for_supported_formats() {
        let mut kg = KittyGraphics::new(Arc::new(KittyImageStore::default()));
        let resp = kg.apply(b"Gi=31,s=1,v=1,a=q,t=d,f=32;AAAA").unwrap();
        assert_eq!(resp, b"\x1b_Gi=31;OK\x1b\\".to_vec());
        let resp = kg.apply(b"Gi=32,a=q,t=f,f=32;AAAA").unwrap();
        assert!(resp.starts_with(b"\x1b_Gi=32;ENOTSUPPORTED"));
    }

    #[test]
    fn chunked_rgba_transmission_lands_in_store_as_bgra() {
        let store = Arc::new(KittyImageStore::default());
        let mut kg = KittyGraphics::new(Arc::clone(&store));
        // 2x1 RGBA: red, then green.
        let data = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let (head, tail) = b64.split_at(4);
        assert!(
            kg.apply(format!("Ga=T,U=1,i=7,f=32,s=2,v=1,c=4,r=2,m=1;{head}").as_bytes())
                .is_none()
        );
        let resp = kg.apply(format!("Gm=0;{tail}").as_bytes()).unwrap();
        assert_eq!(resp, b"\x1b_Gi=7;OK\x1b\\".to_vec());

        let img = store.get(7).expect("image stored");
        assert_eq!((img.cols, img.rows), (4, 2));
        // BGRA: red pixel becomes 00 00 FF FF.
        assert_eq!(
            img.image.as_bytes(0).unwrap(),
            &[0, 0, 255, 255, 0, 255, 0, 255]
        );
    }

    #[test]
    fn continuation_chunks_may_repeat_keys() {
        // The spec says later chunks SHOULD carry only m=, but real clients
        // repeat a/i/q — must still be treated as continuations.
        let store = Arc::new(KittyImageStore::default());
        let mut kg = KittyGraphics::new(Arc::clone(&store));
        let data = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let (head, tail) = b64.split_at(4);
        assert!(
            kg.apply(format!("Ga=T,i=7,f=32,s=1,v=1,m=1;{head}").as_bytes())
                .is_none()
        );
        let resp = kg.apply(format!("Ga=T,i=7,m=0;{tail}").as_bytes()).unwrap();
        assert_eq!(resp, b"\x1b_Gi=7;OK\x1b\\".to_vec());
        assert!(store.get(7).is_some());
    }

    #[test]
    fn png_transmission_decodes() {
        // 1x1 red PNG.
        let mut png = Vec::new();
        let img = RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

        let store = Arc::new(KittyImageStore::default());
        let mut kg = KittyGraphics::new(Arc::clone(&store));
        let resp = kg
            .apply(format!("Ga=T,i=9,f=100,U=1,c=1,r=1;{b64}").as_bytes())
            .unwrap();
        assert_eq!(resp, b"\x1b_Gi=9;OK\x1b\\".to_vec());
        assert_eq!(
            store.get(9).unwrap().image.as_bytes(0).unwrap(),
            &[0, 0, 255, 255]
        );
    }

    #[test]
    fn delete_clears_images() {
        let store = Arc::new(KittyImageStore::default());
        let mut kg = KittyGraphics::new(Arc::clone(&store));
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 255]);
        kg.apply(format!("Ga=T,i=5,f=32,s=1,v=1;{b64}").as_bytes());
        assert!(store.get(5).is_some());
        assert!(kg.apply(b"Ga=d,d=i,i=5;").is_none());
        assert!(store.get(5).is_none());

        kg.apply(format!("Ga=T,i=6,f=32,s=1,v=1;{b64}").as_bytes());
        assert!(kg.apply(b"Ga=d,d=A;").is_none());
        assert!(store.is_empty());
    }

    #[test]
    fn quiet_suppresses_responses() {
        let mut kg = KittyGraphics::new(Arc::new(KittyImageStore::default()));
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 255]);
        assert!(
            kg.apply(format!("Ga=T,i=5,q=1,f=32,s=1,v=1;{b64}").as_bytes())
                .is_none()
        );
        // q=1 still reports errors…
        assert!(kg.apply(b"Ga=T,i=5,q=1,f=32,s=9,v=9;AAAA").is_some());
        // …q=2 does not.
        assert!(kg.apply(b"Ga=T,i=5,q=2,f=32,s=9,v=9;AAAA").is_none());
    }

    #[test]
    fn placeholder_decode_reads_diacritics_and_continues_runs() {
        assert_eq!(diacritic_index('\u{0305}'), Some(0));
        assert_eq!(diacritic_index('\u{030D}'), Some(1));
        assert_eq!(diacritic_index('\u{1D244}'), Some(296));
        assert_eq!(diacritic_index('a'), None);

        // Full form: row 1, col 2, id MSB 1 over fg 0x000030.
        let ph = decode_placeholder(0x30, &['\u{030D}', '\u{030E}', '\u{030D}'], None).unwrap();
        assert_eq!(
            ph,
            PlaceholderCell {
                id: 0x0100_0030,
                row: 1,
                col: 2
            }
        );

        // Omitted col: continues left neighbor on the same row.
        let next = decode_placeholder(0x30, &['\u{030D}'], Some(ph)).unwrap();
        assert_eq!(
            next,
            PlaceholderCell {
                id: ph.id,
                row: 1,
                col: 3
            }
        );

        // No diacritics at all: pure run-length continuation.
        let next2 = decode_placeholder(0x30, &[], Some(next)).unwrap();
        assert_eq!(
            next2,
            PlaceholderCell {
                id: ph.id,
                row: 1,
                col: 4
            }
        );

        // No diacritics and no neighbor: undecodable.
        assert!(decode_placeholder(0x30, &[], None).is_none());
    }

    #[test]
    fn store_evicts_oldest_when_over_cap() {
        let store = KittyImageStore::default();
        let big = |_id: u32| StoredImage {
            image: Arc::new(RenderImage::new(vec![Frame::new(
                RgbaImage::new(4096, 4096), // 64 MiB
            )])),
            cols: 1,
            rows: 1,
        };
        for id in 1..=5 {
            store.insert(id, big(id));
        }
        // 5 × 64 MiB > 256 MiB cap → id 1 evicted, newest retained.
        assert!(store.get(1).is_none());
        assert!(store.get(5).is_some());
    }
}
