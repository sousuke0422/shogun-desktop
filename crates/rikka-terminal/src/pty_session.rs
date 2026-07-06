use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
};

use parking_lot::FairMutex;

use alacritty_terminal::{
    Term,
    term::{Config, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};
use anyhow::Result;

use crate::notify;
use crate::progress::{OscEvent, OscScanner, Progress};
use crate::{
    ClipboardEvent, ClipboardListener, GridSnapshot, PtyResizer, TerminalSession, kitty_graphics,
    take_snapshot,
};

/// Assemble a live terminal session over any PTY-like byte transport.
///
/// The transport is just a `Read` end, a shared `Write` end and a
/// [`PtyResizer`]; SSH/ConPTY specifics live in the embedding application
/// (shogun-desktop: `pty_spawn`).
pub fn build_terminal_session(
    cols: u16,
    rows: u16,
    reader: Box<dyn Read + Send>,
    writer: Arc<FairMutex<Box<dyn Write + Send>>>,
    resizer: Arc<dyn PtyResizer>,
) -> Result<TerminalSession> {
    // ── OSC 52 clipboard handler ──────────────────────────────────────────────
    // Channel capacity of 16 is enough to absorb bursts without blocking the
    // PTY reader thread. Events are silently dropped when the buffer is full.
    let (cb_tx, cb_rx) = std::sync::mpsc::sync_channel::<ClipboardEvent>(16);
    let writer_for_cb = Arc::clone(&writer);
    std::thread::spawn(move || {
        while let Ok(event) = cb_rx.recv() {
            match event {
                ClipboardEvent::Store(text) => {
                    // Write application text → host clipboard.
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(&text);
                    }
                }
                ClipboardEvent::Load(callback) => {
                    // Read host clipboard → generate OSC 52 response → write to PTY.
                    let content = arboard::Clipboard::new()
                        .and_then(|mut cb| cb.get_text())
                        .unwrap_or_default();
                    let response = callback(&content);
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(response.as_bytes());
                }
                ClipboardEvent::PtyWrite(text) => {
                    // Generic write-back (OSC color queries etc.).
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(text.as_bytes());
                }
            }
        }
    });

    let title: Arc<FairMutex<Option<String>>> = Arc::default();
    let listener = ClipboardListener {
        tx: cb_tx,
        title: Arc::clone(&title),
    };
    // OSC 52: alacritty's default is OnlyCopy — store works but load (the
    // clipboard *read* query, e.g. vim's "+p over SSH) is silently denied,
    // so the ClipboardEvent::Load path never fired. CopyPaste enables both.
    // Tradeoff: a remote app can read the host clipboard; acceptable for a
    // tool that only connects to our own infrastructure.
    let config = Config {
        osc52: alacritty_terminal::term::Osc52::CopyPaste,
        // Track kitty keyboard-protocol pushes/pops and answer the CSI ? u
        // query (reply arrives via Event::PtyWrite → ClipboardEvent). The
        // resulting TermMode bits drive the encoder in terminal::keys.
        kitty_keyboard: true,
        ..Config::default()
    };
    let term = Arc::new(FairMutex::new(Term::new(
        config,
        &TermSize::new(cols as usize, rows as usize),
        listener,
    )));
    let snapshot = Arc::new(FairMutex::new(GridSnapshot::blank(
        cols as usize,
        rows as usize,
    )));
    let connected = Arc::new(AtomicBool::new(true));
    let generation = Arc::new(AtomicU64::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let error: Arc<FairMutex<Option<String>>> = Arc::new(FairMutex::new(None));
    let progress = Arc::new(Progress::default());
    let notifications: notify::NotificationQueue = Default::default();
    let images = Arc::new(kitty_graphics::KittyImageStore::default());
    let cell_size_px = Arc::new((AtomicU16::new(0), AtomicU16::new(0)));

    {
        let term2 = Arc::clone(&term);
        let snap2 = Arc::clone(&snapshot);
        let conn2 = Arc::clone(&connected);
        let gen2 = Arc::clone(&generation);
        let notify2 = Arc::clone(&notify);
        let err2 = Arc::clone(&error);
        let progress2 = Arc::clone(&progress);
        let notifications2 = Arc::clone(&notifications);
        let images2 = Arc::clone(&images);
        let cell_px2 = Arc::clone(&cell_size_px);
        let writer2 = Arc::clone(&writer);
        // The blocking `Read` lives on its own IO thread so the parse thread
        // can wait with a deadline: synchronized updates (CSI ? 2026, DEC
        // "Synchronized Output") buffer PTY bytes inside the vte Processor
        // until ESU arrives — if the application dies mid-update, only a
        // timeout can flush the buffer, and a thread parked in `read()`
        // would never fire one.
        let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    // Dropping the sender signals EOF to the parse thread.
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if chunk_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        std::thread::spawn(move || {
            use std::sync::mpsc::RecvTimeoutError;
            let mut parser = Processor::<StdSyncHandler>::new();
            // OSC 9 / 9;4 / 777 observer — the vte stack drops these
            // sequences, so a passive side-scanner extracts progress and
            // desktop notifications (see terminal::progress / notify).
            let mut osc = OscScanner::new();
            // Kitty graphics: APC observer (vte swallows APC too) + protocol
            // driver. Responses (query replies, transmission ACKs) go back
            // to the PTY writer. See terminal::kitty_graphics.
            let mut apc = kitty_graphics::ApcScanner::new();
            let mut kitty = kitty_graphics::KittyGraphics::new(Arc::clone(&images2));
            // Sixel (DCS q): third passive observer. Decoded images join the
            // kitty store and are laid down as Unicode placeholder cells fed
            // straight into the parser — grid/scrollback/renderer handling
            // is shared with kitty graphics. See terminal::sixel.
            let mut sixel_scanner = crate::sixel::SixelScanner::new();
            let mut sixel_ids = crate::sixel::SixelIdAllocator::new();
            // XTVERSION (CSI > 0 q): vte has no hook for it — answer from a
            // passive scanner so applications can identify the terminal.
            let mut xtversion = crate::xtversion::XtversionScanner::new();
            loop {
                // While a synchronized update is pending, wait only until its
                // deadline so an unterminated BSU can't freeze the screen.
                let chunk = match parser.sync_timeout().sync_timeout() {
                    Some(deadline) => {
                        let wait = deadline.saturating_duration_since(std::time::Instant::now());
                        match chunk_rx.recv_timeout(wait) {
                            Ok(chunk) => Some(chunk),
                            Err(RecvTimeoutError::Timeout) => {
                                let mut t = term2.lock();
                                parser.stop_sync(&mut *t);
                                *snap2.lock() = take_snapshot(&t);
                                drop(t);
                                gen2.fetch_add(1, Ordering::Relaxed);
                                notify2.notify_one();
                                continue;
                            }
                            Err(RecvTimeoutError::Disconnected) => None,
                        }
                    }
                    None => chunk_rx.recv().ok(),
                };
                match chunk {
                    None => {
                        // Flush any bytes still held by an open synchronized
                        // update so final output isn't lost on disconnect.
                        if parser.sync_timeout().sync_timeout().is_some() {
                            let mut t = term2.lock();
                            parser.stop_sync(&mut *t);
                            *snap2.lock() = take_snapshot(&t);
                        }
                        conn2.store(false, Ordering::Relaxed);
                        *err2.lock() = Some("PTY接続が切断されました".into());
                        // Bump + signal so the UI repaints the disconnected
                        // state promptly instead of waiting for other traffic.
                        gen2.fetch_add(1, Ordering::Relaxed);
                        notify2.notify_one();
                        break;
                    }
                    Some(chunk) => {
                        let mut t = term2.lock();
                        let mut responses: Vec<Vec<u8>> = Vec::new();
                        for &byte in &chunk {
                            match osc.advance(byte) {
                                Some(OscEvent::Progress(update)) => progress2.apply(update),
                                Some(OscEvent::Notify(note)) => notify::push(&notifications2, note),
                                None => {}
                            }
                            if let Some(payload) = apc.advance(byte)
                                && let Some(resp) = kitty.apply(&payload)
                            {
                                responses.push(resp);
                            }
                            if let Some(reply) = xtversion.advance(byte) {
                                responses.push(reply);
                            }
                            parser.advance(&mut *t, byte);
                            // Sixel: on a completed DCS (the parser has just
                            // consumed the terminator and discarded the DCS
                            // as unhandled), store the image and lay down
                            // placeholder cells at the cursor.
                            if let Some(seq) = sixel_scanner.advance(byte)
                                && let Some(img) = crate::sixel::decode(&seq.data)
                            {
                                let cw = cell_px2.0.load(Ordering::Relaxed).max(1) as usize;
                                let chp = cell_px2.1.load(Ordering::Relaxed).max(1) as usize;
                                // Before the first resize the cell size is
                                // unknown; assume a common 10×20 device px.
                                let (cw, chp) = if cw <= 1 { (10, 20) } else { (cw, chp) };
                                use alacritty_terminal::grid::Dimensions as _;
                                let grid_cols = t.columns().max(1);
                                let cols =
                                    img.width.div_ceil(cw).clamp(1, grid_cols).min(296) as u16;
                                let rows = img.height.div_ceil(chp).clamp(1, 296) as u16;
                                let id = sixel_ids.next_id();
                                if images2.insert_rgba(
                                    id,
                                    img.width as u32,
                                    img.height as u32,
                                    img.rgba,
                                    cols,
                                    rows,
                                ) {
                                    // The injection changes the fg color to
                                    // carry the image id; save and restore
                                    // the application's SGR foreground.
                                    let restore =
                                        crate::sixel::sgr_fg_bytes(t.grid().cursor.template.fg);
                                    for &b in &crate::sixel::placeholder_bytes(id, cols, rows) {
                                        parser.advance(&mut *t, b);
                                    }
                                    for &b in &restore {
                                        parser.advance(&mut *t, b);
                                    }
                                }
                            }
                        }
                        *snap2.lock() = take_snapshot(&t);
                        drop(t);
                        // Written only after releasing the term lock, so the
                        // UI input path (which also takes the writer lock)
                        // never waits behind parsing + snapshotting.
                        if !responses.is_empty() {
                            let mut w = writer2.lock();
                            for resp in responses {
                                let _ = w.write_all(&resp);
                            }
                            let _ = w.flush();
                        }
                        gen2.fetch_add(1, Ordering::Relaxed);
                        notify2.notify_one();
                    }
                }
            }
        });
    }

    Ok(TerminalSession {
        term,
        writer,
        snapshot,
        connected,
        generation,
        notify,
        error,
        progress,
        notifications,
        images,
        focused: AtomicBool::new(true),
        cell_size_px,
        title,
        cols: AtomicU16::new(cols),
        rows: AtomicU16::new(rows),
        resizer,
    })
}

/// Test session whose "PTY" plays back a canned output byte stream, then
/// EOFs — exercises the real reader thread (scanners, parser, snapshots).
#[cfg(test)]
pub fn build_test_session_with_output(cols: u16, rows: u16, output: Vec<u8>) -> TerminalSession {
    use crate::NoopResizer;
    use std::io::Cursor;
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
        Arc::new(FairMutex::new(Box::new(std::io::sink())));
    let reader: Box<dyn Read + Send> = Box::new(Cursor::new(output));
    build_terminal_session(cols, rows, reader, writer, Arc::new(NoopResizer)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full kitty-graphics pipeline through the real reader thread:
    /// APC scanner → protocol driver → image store, and placeholder cells
    /// through vte → snapshot. Guards the integration seams the unit tests
    /// in kitty_graphics.rs / mod.rs cannot see.
    #[test]
    fn kitty_graphics_end_to_end_through_reader_thread() {
        use base64::Engine as _;

        // 1x1 red PNG, transmitted as a virtual placement (U=1, 1x1 cells).
        let mut png = Vec::new();
        let img = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

        let mut script = Vec::new();
        script.extend(format!("\x1b_Ga=T,U=1,i=3,f=100,c=1,r=1;{b64}\x1b\\").into_bytes());
        // Placeholder cell: fg = 256-color index 3 (image id), row/col 0.
        script.extend("\x1b[38;5;3m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".bytes());

        let session = build_test_session_with_output(10, 2, script);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader thread must reach EOF");

        let stored = session.images.get(3).expect("image must land in the store");
        assert_eq!((stored.cols, stored.rows), (1, 1));
        let snap = session.snapshot.lock().clone();
        assert!(snap.has_images);
        assert_eq!(
            snap.cells[0][0].image,
            Some(kitty_graphics::PlaceholderCell {
                id: 3,
                row: 0,
                col: 0
            })
        );
    }

    fn wait_for_eof(session: &TerminalSession) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader thread must reach EOF");
    }

    fn row0_text(session: &TerminalSession) -> String {
        let snap = session.snapshot.lock();
        snap.cells[0].iter().map(|c| c.c).collect::<String>()
    }

    /// CSI ? 2026: bytes inside BSU/ESU are buffered by the vte Processor and
    /// applied atomically when ESU arrives.
    #[test]
    fn sync_update_esu_applies_buffered_bytes() {
        let script = b"\x1b[?2026hhi\x1b[?2026l".to_vec();
        let session = build_test_session_with_output(10, 2, script);
        wait_for_eof(&session);
        assert!(row0_text(&session).starts_with("hi"));
    }

    /// A synchronized update left open at EOF must still be flushed so the
    /// application's final output isn't lost.
    #[test]
    fn sync_update_open_at_eof_is_flushed() {
        let script = b"\x1b[?2026hhi".to_vec();
        let session = build_test_session_with_output(10, 2, script);
        wait_for_eof(&session);
        assert!(row0_text(&session).starts_with("hi"));
    }

    /// The full sixel pipeline through the real reader thread: DCS scanner →
    /// decoder → kitty store under a synthetic id → placeholder cells
    /// injected into the parser → snapshot, cursor left below the image.
    #[test]
    fn sixel_end_to_end_through_reader_thread() {
        use crate::sixel::SIXEL_ID_BASE;

        // Red 1×6 column ('~' = all six bits), then ordinary text.
        let mut script = Vec::new();
        script.extend_from_slice(b"\x1bPq#0;2;100;0;0~\x1b\\");
        script.extend_from_slice(b"done");

        let session = build_test_session_with_output(10, 4, script);
        wait_for_eof(&session);

        let stored = session
            .images
            .get(SIXEL_ID_BASE)
            .expect("sixel image must land in the kitty store");
        // Cell size unknown pre-resize → 10×20 fallback → 1×1 cells.
        assert_eq!((stored.cols, stored.rows), (1, 1));
        assert_eq!(u32::from(stored.image.size(0).width), 1);
        assert_eq!(u32::from(stored.image.size(0).height), 6);

        let snap = session.snapshot.lock().clone();
        assert!(snap.has_images);
        assert_eq!(
            snap.cells[0][0].image,
            Some(kitty_graphics::PlaceholderCell {
                id: SIXEL_ID_BASE,
                row: 0,
                col: 0
            })
        );
        // The injected trailing newline puts the following text on row 1.
        let row1: String = snap.cells[1].iter().map(|c| c.c).collect();
        assert!(row1.starts_with("done"), "got {row1:?}");
    }

    /// OSC 0/2 lands in the shared title slot (mirrored to the OS window
    /// title by the UI).
    #[test]
    fn osc_title_reaches_session() {
        let session = build_test_session_with_output(10, 2, b"\x1b]2;hello title\x07".to_vec());
        wait_for_eof(&session);
        assert_eq!(session.title.lock().as_deref(), Some("hello title"));
    }

    /// Focus reporting (?1004): CSI I/O only when the app opted in, only on
    /// actual changes.
    #[test]
    fn focus_reporting_gated_and_deduplicated() {
        use crate::NoopResizer;

        #[derive(Clone, Default)]
        struct CaptureWriter(Arc<FairMutex<Vec<u8>>>);
        impl Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = CaptureWriter::default();
        let sink = capture.clone();
        let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
            Arc::new(FairMutex::new(Box::new(sink)));

        // App enables focus reporting, then the stream EOFs.
        let reader: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(b"\x1b[?1004h".to_vec()));
        let session = build_terminal_session(10, 2, reader, writer, Arc::new(NoopResizer)).unwrap();
        wait_for_eof(&session);

        session.report_focus(true); // already focused — no output
        assert!(capture.0.lock().is_empty());
        session.report_focus(false);
        session.report_focus(false); // duplicate — swallowed
        session.report_focus(true);
        assert_eq!(capture.0.lock().as_slice(), b"\x1b[O\x1b[I");
    }

    /// Without ?1004 no focus bytes may reach the PTY (vim would see garbage).
    #[test]
    fn focus_reporting_silent_when_mode_off() {
        use crate::NoopResizer;

        let bytes: Arc<FairMutex<Vec<u8>>> = Arc::default();
        struct W(Arc<FairMutex<Vec<u8>>>);
        impl Write for W {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
            Arc::new(FairMutex::new(Box::new(W(Arc::clone(&bytes)))));
        let reader: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(Vec::new()));
        let session = build_terminal_session(10, 2, reader, writer, Arc::new(NoopResizer)).unwrap();
        wait_for_eof(&session);

        session.report_focus(false);
        session.report_focus(true);
        assert!(bytes.lock().is_empty());
    }

    /// An unterminated BSU on a silent-but-alive PTY must be flushed by the
    /// sync deadline (the parse thread's recv_timeout path), not frozen.
    #[test]
    fn sync_update_timeout_flushes_buffer() {
        use crate::NoopResizer;

        /// Yields one canned chunk, then blocks forever (PTY stays open).
        struct StallingReader(Option<Vec<u8>>);
        impl Read for StallingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.take() {
                    Some(bytes) => {
                        buf[..bytes.len()].copy_from_slice(&bytes);
                        Ok(bytes.len())
                    }
                    None => loop {
                        std::thread::park();
                    },
                }
            }
        }

        let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
            Arc::new(FairMutex::new(Box::new(std::io::sink())));
        let reader: Box<dyn Read + Send> =
            Box::new(StallingReader(Some(b"\x1b[?2026hhi".to_vec())));
        let session = build_terminal_session(10, 2, reader, writer, Arc::new(NoopResizer)).unwrap();

        // vte's sync deadline is 150ms; poll well past it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if row0_text(&session).starts_with("hi") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("buffered sync output was never flushed by the deadline");
    }
}
