use std::io::{Read, Write};
/// `std::thread::spawn` with a name, so panics attribute themselves in the
/// panic log (see [`crate::install_panic_log`]) — an anonymous dead parse
/// thread otherwise reports itself only as a frozen grid.
fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(f);
}

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
};

use parking_lot::FairMutex;

use alacritty_terminal::{
    Term,
    grid::Dimensions as _,
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
    xtversion_identity: &str,
) -> Result<TerminalSession> {
    build_terminal_session_with_preface(
        cols,
        rows,
        reader,
        writer,
        resizer,
        xtversion_identity,
        Vec::new(),
    )
}

/// [`build_terminal_session`] with `preface` bytes applied by the parse
/// thread as one ATOMIC first chunk, before anything from the reader — and
/// before any resize: the settler waits for it. A tab move's screen replay
/// lands here; its absolute cursor positions assume the geometry the
/// session was created with, so a resize slicing into a half-applied
/// preface would scatter the rest across the new grid (the "input jumps
/// to the bottom after merging into a differently-sized window" bug —
/// chunked application through the reader was interruptible).
pub fn build_terminal_session_with_preface(
    cols: u16,
    rows: u16,
    reader: Box<dyn Read + Send>,
    writer: Arc<FairMutex<Box<dyn Write + Send>>>,
    resizer: Arc<dyn PtyResizer>,
    xtversion_identity: &str,
    preface: Vec<u8>,
) -> Result<TerminalSession> {
    // Created ahead of the clipboard/query handler thread below. The terminal
    // itself is late-bound through `term_slot` (Weak, so the slot never keeps
    // a dead session's Term alive): the thread must exist before the Term —
    // it owns the sender side of the event channel the Term's listener uses.
    let cell_size_px = Arc::new((AtomicU16::new(0), AtomicU16::new(0)));
    let term_slot: Arc<std::sync::OnceLock<std::sync::Weak<FairMutex<Term<ClipboardListener>>>>> =
        Arc::new(std::sync::OnceLock::new());

    // ── OSC 52 clipboard / protocol-query handler ─────────────────────────────
    // Channel capacity of 16 is enough to absorb bursts without blocking the
    // PTY reader thread. Events are silently dropped when the buffer is full.
    let (cb_tx, cb_rx) = std::sync::mpsc::sync_channel::<ClipboardEvent>(16);
    let writer_for_cb = Arc::clone(&writer);
    let term_for_cb = Arc::clone(&term_slot);
    let cell_px_for_cb = Arc::clone(&cell_size_px);
    spawn_named("rikka-clipboard", move || {
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
                    // Generic write-back (kitty CSI ? u reply, CSI 18 t etc.).
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(text.as_bytes());
                }
                ClipboardEvent::ColorQuery(idx, formatter) => {
                    // OSC 10/11/4 query (vim probes the background for its
                    // theme). Resolve against the live palette; briefly
                    // blocking on the term lock here is fine — this thread
                    // holds nothing the parse thread needs.
                    let Some(term) = term_for_cb.get().and_then(|w| w.upgrade()) else {
                        continue;
                    };
                    let rgb = {
                        let term = term.lock();
                        crate::query_color_rgb(term.colors(), idx)
                    };
                    if let Some(rgb) = rgb {
                        let response = formatter(rgb);
                        let mut w = writer_for_cb.lock();
                        let _ = w.write_all(response.as_bytes());
                    }
                }
                ClipboardEvent::TextAreaSize(formatter) => {
                    // CSI 14 t: pixel size = cells × the renderer's cell size
                    // (the same numbers TIOCGWINSZ advertises via resize()).
                    let Some(term) = term_for_cb.get().and_then(|w| w.upgrade()) else {
                        continue;
                    };
                    let (lines, cols) = {
                        let term = term.lock();
                        (term.screen_lines() as u16, term.columns() as u16)
                    };
                    let response = formatter(alacritty_terminal::event::WindowSize {
                        num_lines: lines,
                        num_cols: cols,
                        cell_width: cell_px_for_cb.0.load(Ordering::Relaxed),
                        cell_height: cell_px_for_cb.1.load(Ordering::Relaxed),
                    });
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(response.as_bytes());
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
    // Late-bind the terminal into the query-handler thread (see above).
    let _ = term_slot.set(Arc::downgrade(&term));
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
    // Screen-anchored selection (see TerminalSession::screen_sel): re-pinned
    // by the parse thread after every output application.
    let screen_sel: Arc<FairMutex<crate::ScreenSel>> = Arc::new(FairMutex::new(None));
    // Shell integration: OSC 133;A prompt marks + OSC 9;9 / OSC 7 cwd.
    let prompt_marks: Arc<FairMutex<std::collections::VecDeque<u64>>> = Arc::default();
    let cwd: Arc<FairMutex<Option<String>>> = Arc::default();
    let search: Arc<FairMutex<Option<crate::SearchLive>>> = Arc::new(FairMutex::new(None));
    let xtversion = crate::xtversion::XtversionScanner::new(xtversion_identity);
    // Flipped once the preface (if any) has been applied — the resize
    // settler holds every reflow until then (see the function docs).
    let preface_done = Arc::new(AtomicBool::new(preface.is_empty()));
    // Session log sinks (Tera Term-style); attached at runtime through
    // TerminalSession::set_logging.
    let output_log: Arc<FairMutex<Option<std::fs::File>>> = Arc::default();
    let input_log: Arc<FairMutex<Option<std::fs::File>>> = Arc::default();

    let (reader_thread, parser_thread) = {
        let term2 = Arc::clone(&term);
        let snap2 = Arc::clone(&snapshot);
        let conn2 = Arc::clone(&connected);
        let gen2 = Arc::clone(&generation);
        let notify2 = Arc::clone(&notify);
        let err2 = Arc::clone(&error);
        let progress2 = Arc::clone(&progress);
        let notifications2 = Arc::clone(&notifications);
        let images2 = Arc::clone(&images);
        let screen_sel2 = Arc::clone(&screen_sel);
        let prompt_marks2 = Arc::clone(&prompt_marks);
        let cwd2 = Arc::clone(&cwd);
        let cell_px2 = Arc::clone(&cell_size_px);
        let writer2 = Arc::clone(&writer);
        let preface_done2 = Arc::clone(&preface_done);
        let output_log2 = Arc::clone(&output_log);
        // The blocking `Read` lives on its own IO thread so the parse thread
        // can wait with a deadline: synchronized updates (CSI ? 2026, DEC
        // "Synchronized Output") buffer PTY bytes inside the vte Processor
        // until ESU arrives — if the application dies mid-update, only a
        // timeout can flush the buffer, and a thread parked in `read()`
        // would never fire one.
        let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
        // The JoinHandle is kept on the session: a cross-process tab move
        // must stop this thread (CancelSynchronousIo aborts the blocked
        // read → `Err` → break) before the receiver starts reading the same
        // pipe. See `TerminalSession::quiesce_for_transfer`.
        let reader_thread = std::thread::Builder::new()
            .name("rikka-pty-io".into())
            .spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                // RIKKA_PTY_DUMP=<path>: tee the raw PTY output for protocol
                // forensics (what did the app REALLY emit, post-transport).
                let mut dump = std::env::var_os("RIKKA_PTY_DUMP").and_then(|p| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(p)
                        .ok()
                });
                loop {
                    match reader.read(&mut buf) {
                        // Dropping the sender signals EOF to the parse thread.
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Some(f) = dump.as_mut() {
                                use std::io::Write as _;
                                let _ = f.write_all(&buf[..n]);
                            }
                            // Session output log (Tera Term-style tee); the
                            // lock is uncontended unless logging just
                            // toggled.
                            if let Some(f) = &mut *output_log2.lock() {
                                use std::io::Write as _;
                                let _ = f.write_all(&buf[..n]);
                            }
                            if chunk_tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .ok();
        // Also kept on the session (like the reader): a tab move must wait
        // for this thread to DRAIN — chunks the reader consumed may still
        // sit in the channel or in an open ?2026 sync buffer, and a replay
        // serialized before they land in the Term would lose them for good
        // (the pipe no longer holds them for the receiver). Reader death
        // drops the channel sender, so after the reader is joined this
        // thread provably flushes everything and exits.
        let parser_thread = std::thread::Builder::new()
            .name("rikka-parse".into())
            .spawn(move || {
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
                let mut xtversion = xtversion;
                // XTWINOPS op 16 (cell pixel size): alacritty drops it, yazi
                // needs it to size sixel rasters on Windows. See winops.
                let mut winops = crate::winops::WinopsScanner::new();
                // A tab move's screen replay runs as one atomic first chunk
                // (one term.lock()), before anything from the pipe — its
                // absolute positions assume the birth geometry, so nothing
                // (especially not a reflow; the settler waits on
                // preface_done) may slice into it.
                let mut first_chunk = (!preface.is_empty()).then_some(preface);
                loop {
                    // While a synchronized update is pending, wait only until its
                    // deadline so an unterminated BSU can't freeze the screen.
                    let chunk = if let Some(p) = first_chunk.take() {
                        Some(p)
                    } else {
                        match parser.sync_timeout().sync_timeout() {
                            Some(deadline) => {
                                let wait =
                                    deadline.saturating_duration_since(std::time::Instant::now());
                                match chunk_rx.recv_timeout(wait) {
                                    Ok(chunk) => Some(chunk),
                                    Err(RecvTimeoutError::Timeout) => {
                                        let mut t = term2.lock();
                                        parser.stop_sync(&mut *t);
                                        crate::repin_screen_selection(&mut t, &screen_sel2);
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
                        }
                    };
                    match chunk {
                        None => {
                            // Flush any bytes still held by an open synchronized
                            // update so final output isn't lost on disconnect.
                            if parser.sync_timeout().sync_timeout().is_some() {
                                let mut t = term2.lock();
                                parser.stop_sync(&mut *t);
                                crate::repin_screen_selection(&mut t, &screen_sel2);
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
                                    Some(OscEvent::Notify(note)) => {
                                        notify::push(&notifications2, note)
                                    }
                                    Some(OscEvent::Prompt(
                                        crate::progress::PromptMark::PromptStart,
                                    )) => {
                                        // Everything before this OSC has been
                                        // parsed, so the cursor sits on the
                                        // prompt row. Absolute buffer line =
                                        // history + screen row (see
                                        // TerminalSession::prompt_marks).
                                        let abs = t.grid().history_size() as u64
                                            + t.grid().cursor.point.line.0.max(0) as u64;
                                        let mut marks = prompt_marks2.lock();
                                        if marks.back() != Some(&abs) {
                                            marks.push_back(abs);
                                            if marks.len() > 1000 {
                                                marks.pop_front();
                                            }
                                        }
                                    }
                                    Some(OscEvent::Prompt(_)) => {}
                                    Some(OscEvent::Cwd(path)) => *cwd2.lock() = Some(path),
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
                                if winops.advance(byte) {
                                    let cw = cell_px2.0.load(Ordering::Relaxed) as usize;
                                    let chp = cell_px2.1.load(Ordering::Relaxed) as usize;
                                    responses.push(crate::winops::cell_size_reply(cw, chp));
                                }
                                parser.advance(&mut *t, byte);
                                // Sixel: on a completed DCS (the parser has just
                                // consumed the terminator and discarded the DCS
                                // as unhandled), store the image and lay down
                                // placeholder cells at the cursor.
                                if let Some(seq) = sixel_scanner.advance(byte)
                                    && let Some(img) = crate::sixel::decode(&seq.data)
                                {
                                    // A ?2026 synchronized update buffers bytes
                                    // inside the Processor, so the term's cursor
                                    // still predates the frame's CUP — inject
                                    // against that and the placement lands in
                                    // stale coordinates (the yazi empty/corrupt
                                    // preview). Flush the buffered prefix first:
                                    // the frame tears once per image, but the
                                    // cursor and SGR are true.
                                    if parser.sync_timeout().sync_timeout().is_some() {
                                        parser.stop_sync(&mut *t);
                                    }
                                    let cw = cell_px2.0.load(Ordering::Relaxed).max(1) as usize;
                                    let chp = cell_px2.1.load(Ordering::Relaxed).max(1) as usize;
                                    // Before the first resize the cell size is
                                    // unknown; assume a common 10×20 device px.
                                    let (cw, chp) = if cw <= 1 { (10, 20) } else { (cw, chp) };
                                    use alacritty_terminal::grid::Dimensions as _;
                                    let grid_cols = t.columns().max(1);
                                    // Fit the placement to the space right of the
                                    // cursor (yazi draws previews mid-screen via
                                    // CUP; wrapping would shred the layout).
                                    let start_col =
                                        t.grid().cursor.point.column.0.min(grid_cols - 1);
                                    let fit_cols = grid_cols - start_col;
                                    let cols =
                                        img.width.div_ceil(cw).clamp(1, fit_cols).min(296) as u16;
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
                                        for &b in &crate::sixel::placeholder_bytes(
                                            id,
                                            cols,
                                            rows,
                                            start_col as u16,
                                        ) {
                                            parser.advance(&mut *t, b);
                                        }
                                        for &b in &restore {
                                            parser.advance(&mut *t, b);
                                        }
                                    }
                                }
                            }
                            crate::repin_screen_selection(&mut t, &screen_sel2);
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
                            // The first pass through here was the preface
                            // when one existed — reflows may proceed now.
                            preface_done2.store(true, Ordering::Relaxed);
                        }
                    }
                }
            })
            .ok();
        (reader_thread, parser_thread)
    };

    // Debounced resize settler (leading + trailing edge): the first size of
    // a burst applies immediately (single resizes stay snappy), the rest
    // coalesce until 120 ms of quiet, then the settled size applies. Term
    // reflow, snapshot republish, and the PTY notification all happen HERE,
    // so the grid and the PTY walk the same resize step sequence (see the
    // `pty_resize` field docs). Exits when the session drops.
    let cols_shared = Arc::new(AtomicU16::new(cols));
    let rows_shared = Arc::new(AtomicU16::new(rows));
    let conpty_resize_semantics = Arc::new(AtomicBool::new(false));
    let pty_sealed = Arc::new(AtomicBool::new(false));
    let pty_resize = {
        let (tx, rx) = std::sync::mpsc::channel::<(u16, u16, f32, f32)>();
        let term = Arc::clone(&term);
        let snapshot = Arc::clone(&snapshot);
        let generation = Arc::clone(&generation);
        let notify = Arc::clone(&notify);
        let cols = Arc::clone(&cols_shared);
        let rows = Arc::clone(&rows_shared);
        let cell_size_px = Arc::clone(&cell_size_px);
        let conpty = Arc::clone(&conpty_resize_semantics);
        let sealed = Arc::clone(&pty_sealed);
        let preface_done = Arc::clone(&preface_done);
        let resizer = Arc::clone(&resizer);
        std::thread::Builder::new()
            .name("rikka-pty-resize".into())
            .spawn(move || {
                use alacritty_terminal::term::test::TermSize;
                let apply = |(c, r, cw, ch): (u16, u16, f32, f32)| {
                    // Sealed for a tab move: the receiver owns the PTY now —
                    // a straggler resize (grid OR signal pipe) settling
                    // during our teardown would fight its geometry.
                    if sealed.load(Ordering::Relaxed) {
                        return;
                    }
                    // A tab-move preface must be fully in the Term before
                    // any reflow: its absolute positions assume the birth
                    // geometry, and a resize slicing into it scatters the
                    // rest across the new grid. The deadline only guards a
                    // wedged parser — a lone reflow beats a frozen tab.
                    let wait_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while !preface_done.load(Ordering::Relaxed)
                        && std::time::Instant::now() < wait_until
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    {
                        let mut t = term.lock();
                        t.resize_anchored(
                            TermSize::new(c as usize, r as usize),
                            conpty.load(Ordering::Relaxed),
                        );
                        *snapshot.lock() = crate::take_snapshot(&t);
                    }
                    generation.fetch_add(1, Ordering::Relaxed);
                    notify.notify_one();
                    cols.store(c, Ordering::Relaxed);
                    rows.store(r, Ordering::Relaxed);
                    cell_size_px
                        .0
                        .store(cw.round().max(1.0) as u16, Ordering::Relaxed);
                    cell_size_px
                        .1
                        .store(ch.round().max(1.0) as u16, Ordering::Relaxed);
                    let px_w = (f32::from(c) * cw).round() as u16;
                    let px_h = (f32::from(r) * ch).round() as u16;
                    let _ = resizer.resize(c, r, px_w, px_h);
                };
                while let Ok(first) = rx.recv() {
                    apply(first);
                    let mut pending = None;
                    loop {
                        match rx.recv_timeout(std::time::Duration::from_millis(120)) {
                            Ok(next) => pending = Some(next),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                if let Some(p) = pending {
                                    apply(p);
                                }
                                return;
                            }
                        }
                    }
                    if let Some(p) = pending {
                        apply(p);
                    }
                }
            })
            .expect("spawn pty resize settler");
        tx
    };

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
        screen_sel,
        prompt_marks,
        cwd,
        search,
        focused: AtomicBool::new(true),
        cell_size_px,
        title,
        cols: cols_shared,
        rows: rows_shared,
        resizer,
        pty_resize,
        conpty_resize_semantics,
        reader_thread: FairMutex::new(reader_thread),
        parser_thread: FairMutex::new(parser_thread),
        pty_sealed,
        output_log,
        input_log,
        #[cfg(windows)]
        transfer: FairMutex::new(None),
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
    build_terminal_session(
        cols,
        rows,
        reader,
        writer,
        Arc::new(NoopResizer),
        "test-terminal 0.0",
    )
    .unwrap()
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

    /// Session logging (Tera Term-style): the output log receives the raw
    /// PTY byte stream verbatim, the input log what `send_bytes` carries.
    /// The reader is gated on a channel so `set_logging` provably lands
    /// before the first byte flows.
    #[test]
    fn session_logging_tees_output_and_input() {
        struct Gated(std::sync::mpsc::Receiver<Vec<u8>>);
        impl Read for Gated {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.recv() {
                    Ok(b) => {
                        let n = b.len().min(buf.len());
                        buf[..n].copy_from_slice(&b[..n]);
                        Ok(n)
                    }
                    Err(_) => Ok(0), // sender dropped = EOF
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
            Arc::new(FairMutex::new(Box::new(std::io::sink())));
        let session = build_terminal_session(
            20,
            5,
            Box::new(Gated(rx)),
            writer,
            Arc::new(crate::NoopResizer),
            "test-terminal 0.0",
        )
        .unwrap();

        let dir = std::env::temp_dir().join(format!("rikka-session-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("out.log");
        let in_path = dir.join("in.log");
        session.set_logging(
            Some(std::fs::File::create(&out_path).unwrap()),
            Some(std::fs::File::create(&in_path).unwrap()),
        );
        let script = b"hello \x1b[31mred\x1b[0m".to_vec();
        tx.send(script.clone()).unwrap();
        session.send_bytes(b"typed");
        drop(tx);
        wait_for_eof(&session);
        session.set_logging(None, None); // close = flush

        assert_eq!(std::fs::read(&out_path).unwrap(), script);
        assert_eq!(std::fs::read(&in_path).unwrap(), b"typed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ConPTY-marked sessions must NOT answer the kitty keyboard query
    /// (`CSI ? u`): conhost never forwards the client's push/pop to the
    /// terminal (the protocol cannot work through it), and OpenConsole
    /// 1.24 swallows an exiting TUI's restore burst — including the
    /// `?1049l` alt-screen exit — once the client uses the protocol our
    /// answer advertised (yazi's quit left the tab stuck on the alt
    /// screen, 2026-07-16). Direct sessions (SSH) keep answering.
    #[test]
    fn conpty_sessions_do_not_advertise_kitty_keyboard() {
        struct Cap(Arc<FairMutex<Vec<u8>>>);
        impl Write for Cap {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct Gated(std::sync::mpsc::Receiver<Vec<u8>>);
        impl Read for Gated {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.recv() {
                    Ok(b) => {
                        let n = b.len().min(buf.len());
                        buf[..n].copy_from_slice(&b[..n]);
                        Ok(n)
                    }
                    Err(_) => Ok(0),
                }
            }
        }
        for (conpty, expect_reply) in [(false, true), (true, false)] {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            let out = Arc::new(FairMutex::new(Vec::new()));
            let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
                Arc::new(FairMutex::new(Box::new(Cap(Arc::clone(&out)))));
            let session = build_terminal_session(
                20,
                5,
                Box::new(Gated(rx)),
                writer,
                Arc::new(crate::NoopResizer),
                "test-terminal 0.0",
            )
            .unwrap();
            if conpty {
                session.mark_conpty();
            }
            tx.send(b"\x1b[?u".to_vec()).unwrap();
            drop(tx);
            wait_for_eof(&session);
            let replied = out.lock().windows(5).any(|w| w == b"\x1b[?0u");
            assert_eq!(replied, expect_reply, "conpty={conpty}");
        }
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

    /// `cat`-ing an ELF must not hang, panic or wedge the pipeline — the
    /// classic terminal killer is binary garbage opening a string-consuming
    /// state (DCS/OSC/APC) that swallows everything after it. Feed a
    /// deterministic megabyte of garbage seeded with the nastiest prefixes,
    /// then prove the terminal still processes output afterwards.
    #[test]
    fn binary_garbage_does_not_hang_the_pipeline() {
        let mut script = Vec::new();
        // ELF header, then unterminated string-openers with payloads.
        script.extend_from_slice(b"\x7fELF\x02\x01\x01\x00");
        script.extend_from_slice(b"\x1bP"); // DCS, never terminated…
        script.extend_from_slice(&[b'q'; 4096]);
        script.extend_from_slice(b"\x1b]"); // OSC, never terminated…
        script.extend_from_slice(&[0x41; 4096]);
        script.extend_from_slice(b"\x1b_G"); // kitty APC opener
        script.extend_from_slice(&[0x42; 4096]);
        script.extend_from_slice(b"\x1b[?2026h"); // sync update, no ESU
        // A megabyte of deterministic pseudo-random bytes (LCG), NUL to 0xFF.
        let mut x: u32 = 0x1234_5678;
        for _ in 0..1_000_000 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            script.push((x >> 24) as u8);
        }
        // Full reset, then a marker that must come out the other side.
        script.extend_from_slice(b"\x1bc GARBAGE-SURVIVED");

        let session = build_test_session_with_output(40, 5, script);
        wait_for_eof(&session); // asserts completion within 5s = no hang
        let snap = session.snapshot.lock().clone();
        let all: String = snap
            .cells
            .iter()
            .map(|row| row.iter().map(|c| c.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("GARBAGE-SURVIVED"), "terminal wedged: {all:?}");
    }

    /// The same resilience against a *real* system binary — whatever ELF/PE
    /// this machine has (`cat /usr/bin/apt` in spirit). On CI a binary must
    /// exist (the test fails otherwise); outside CI a machine with none of
    /// the candidates passes by exception (殿裁定 2026-07-06) — the
    /// deterministic garbage test above still guards those environments.
    #[test]
    fn cat_real_binary_smoke() {
        let candidates: &[&str] = if cfg!(windows) {
            &[
                "C:\\Windows\\System32\\cmd.exe",
                "C:\\Windows\\System32\\notepad.exe",
            ]
        } else {
            &["/usr/bin/apt", "/usr/bin/bash", "/bin/bash", "/bin/ls"]
        };
        let Some(mut script) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
            assert!(
                std::env::var_os("CI").is_none(),
                "on CI one of {candidates:?} must exist so the smoke really runs"
            );
            return;
        };
        // Keep debug-build runtime well inside wait_for_eof's deadline.
        script.truncate(4 * 1024 * 1024);
        script.extend_from_slice(b"\x1bc CAT-BINARY-SURVIVED");

        let session = build_test_session_with_output(40, 5, script);
        wait_for_eof(&session);
        let snap = session.snapshot.lock().clone();
        let all: String = snap
            .cells
            .iter()
            .map(|row| row.iter().map(|c| c.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains("CAT-BINARY-SURVIVED"),
            "terminal wedged after real binary: {all:?}"
        );
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
        let session = build_terminal_session(
            10,
            2,
            reader,
            writer,
            Arc::new(NoopResizer),
            "test-terminal 0.0",
        )
        .unwrap();
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
        let session = build_terminal_session(
            10,
            2,
            reader,
            writer,
            Arc::new(NoopResizer),
            "test-terminal 0.0",
        )
        .unwrap();
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
        let session = build_terminal_session(
            10,
            2,
            reader,
            writer,
            Arc::new(NoopResizer),
            "test-terminal 0.0",
        )
        .unwrap();

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
