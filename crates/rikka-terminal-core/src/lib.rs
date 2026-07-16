pub mod frametime;
pub mod ime;
pub mod keys;
pub mod kitty_graphics;
pub mod notify;
pub mod pane;
pub mod progress;
pub mod pty_handoff;
pub mod pty_session;
pub mod renderer;
pub mod selection;
pub mod sixel;
pub mod winops;
pub mod xtversion;

use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::{Term, event::EventListener};
use parking_lot::FairMutex;

// ── OSC 52 clipboard listener ─────────────────────────────────────────────────

/// Events forwarded from the VTE parser to the clipboard handler thread.
pub enum ClipboardEvent {
    /// OSC 52 write: application wants to store text in the host clipboard.
    Store(String),
    /// OSC 52 read: application wants the clipboard content back via PTY write.
    /// The callback formats the OSC 52 response string when called with the
    /// clipboard text (generated internally by alacritty_terminal).
    Load(Arc<dyn Fn(&str) -> String + Sync + Send + 'static>),
    /// Generic PTY write-back (used by protocol query replies).
    PtyWrite(String),
    /// OSC 4/10/11/12 color query. The callback formats the reply for the
    /// resolved color; resolution runs on the handler thread against the live
    /// palette (see [`query_color_rgb`]).
    ColorQuery(
        usize,
        Arc<dyn Fn(alacritty_terminal::vte::ansi::Rgb) -> String + Sync + Send + 'static>,
    ),
    /// `CSI 14 t` (text-area size in pixels). The callback formats the reply
    /// from a [`alacritty_terminal::event::WindowSize`].
    TextAreaSize(
        Arc<dyn Fn(alacritty_terminal::event::WindowSize) -> String + Sync + Send + 'static>,
    ),
}

/// EventListener implementation that forwards clipboard-related events to a
/// background handler thread via a bounded channel.
///
/// Using `try_send` ensures the PTY reader thread never blocks on a slow
/// clipboard operation — events are silently dropped if the buffer is full.
pub struct ClipboardListener {
    pub tx: std::sync::mpsc::SyncSender<ClipboardEvent>,
    /// OSC 0/2 window title set by the application (`None` = no title /
    /// reset). Shared with [`TerminalSession::title`]; the UI mirrors it
    /// into the OS window title.
    pub title: Arc<FairMutex<Option<String>>>,
}

impl EventListener for ClipboardListener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        use alacritty_terminal::event::Event;
        match event {
            Event::ClipboardStore(_ty, text) => {
                let _ = self.tx.try_send(ClipboardEvent::Store(text));
            }
            Event::ClipboardLoad(_ty, callback) => {
                let _ = self.tx.try_send(ClipboardEvent::Load(callback));
            }
            Event::PtyWrite(text) => {
                let _ = self.tx.try_send(ClipboardEvent::PtyWrite(text));
            }
            // OSC 10/11 etc. — vim queries the background to pick its theme.
            Event::ColorRequest(idx, formatter) => {
                let _ = self.tx.try_send(ClipboardEvent::ColorQuery(idx, formatter));
            }
            // CSI 14 t — image tooling sizes itself from the pixel report.
            Event::TextAreaSizeRequest(formatter) => {
                let _ = self.tx.try_send(ClipboardEvent::TextAreaSize(formatter));
            }
            Event::Title(title) => *self.title.lock() = Some(title),
            Event::ResetTitle => *self.title.lock() = None,
            _ => {}
        }
    }
}

/// Trait implemented by backend-specific PTY resizers.
///
/// Implementors must be `Send + Sync` so the resizer can be stored in an `Arc`
/// and called from any thread (including GPUI's render/event thread).
pub trait PtyResizer: Send + Sync {
    /// Pixel dimensions accompany the cell grid so applications reading
    /// `TIOCGWINSZ` (yazi et al.) can derive the cell size in pixels — the
    /// kitty graphics protocol sizes images with it. `(0, 0)` when unknown.
    fn resize(
        &self,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> anyhow::Result<()>;
}

/// No-op resizer used as a fallback when the backend provides no resize channel.
#[allow(dead_code)]
pub struct NoopResizer;

impl PtyResizer for NoopResizer {
    fn resize(&self, _cols: u16, _rows: u16, _pw: u16, _ph: u16) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct TerminalSession {
    #[allow(dead_code)]
    pub term: Arc<FairMutex<Term<ClipboardListener>>>,
    pub writer: Arc<FairMutex<Box<dyn std::io::Write + Send>>>,
    pub snapshot: Arc<FairMutex<GridSnapshot>>,
    pub connected: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    /// Signalled by the reader thread whenever `generation` advances (and on
    /// disconnect). UI refresh tasks park on this instead of polling, so an
    /// idle terminal causes zero wakeups.
    pub notify: Arc<tokio::sync::Notify>,
    #[allow(dead_code)]
    pub error: Arc<FairMutex<Option<String>>>,
    /// OSC 9;4 progress reported by the running application (written by the
    /// PTY reader thread, read by the UI). See [`progress`].
    pub progress: Arc<progress::Progress>,
    /// OSC 9 / 777 desktop notifications queued by the PTY reader thread and
    /// drained by the UI watcher, which applies Ghostty-style focus
    /// suppression. See [`notify`].
    pub notifications: notify::NotificationQueue,
    /// Kitty-graphics images transmitted by applications in this session
    /// (written by the PTY reader thread, painted by the renderer for
    /// placeholder cells). See [`kitty_graphics`].
    pub images: Arc<kitty_graphics::KittyImageStore>,
    /// Last focus state reported to the application via CSI I / CSI O
    /// (focus reporting, DECSET ?1004). Sessions start focused: they are
    /// spawned into the surface the user is looking at.
    pub focused: AtomicBool,
    /// Cell size in device pixels, rounded (updated by `resize`). Shared
    /// with the PTY reader thread, which sizes sixel placements with it.
    pub cell_size_px: Arc<(AtomicU16, AtomicU16)>,
    /// OSC 0/2 application window title (written by the term event listener,
    /// mirrored into the OS window title by the shell-window UI).
    pub title: Arc<FairMutex<Option<String>>>,
    /// Current terminal width in columns (updated when a resize applies).
    pub cols: Arc<AtomicU16>,
    /// Current terminal height in rows (updated when a resize applies).
    pub rows: Arc<AtomicU16>,
    /// Backend-specific mechanism for propagating resize to the PTY / SSH channel.
    pub resizer: Arc<dyn PtyResizer>,
    /// Debounced resize lane (leading + trailing edge, see
    /// `build_terminal_session`): a window drag's burst applies as its first
    /// and its settled size only — to the local grid AND the PTY together.
    /// The two must walk the *same* step sequence: reflow is path-dependent
    /// (a shrink scrolls rows out that a conhost-anchored grow will not
    /// bring back), and ConPTY never repaints, so any step one side takes
    /// alone is permanent drift. Skipping the transient steps on both sides
    /// also keeps momentary narrow widths from wrap/unwrapping long rows in
    /// conhost's buffer, whose row accounting differs from ours.
    pub pty_resize: std::sync::mpsc::Sender<(u16, u16, f32, f32)>,
    /// ConPTY resize semantics: growth adds blank lines at the bottom
    /// instead of pulling from scrollback. ConPTY emits nothing on resize
    /// and computes later absolute cursor positions against conhost's own
    /// reflow, so a ConPTY-backed session must reflow identically or drift
    /// permanently (typed input lands mid-screen after resize storms). Set
    /// by ConPTY session builders; SSH / Unix PTYs keep Alacritty's native
    /// bottom-anchored behavior. Shared with the resize settler thread.
    pub conpty_resize_semantics: Arc<AtomicBool>,
    /// The blocking PTY reader thread ("rikka-pty-io"), held so a
    /// cross-process tab move can stop it (see
    /// [`Self::quiesce_for_transfer`]). `None` = no reader was spawned, or
    /// it was already quiesced/joined.
    pub reader_thread: FairMutex<Option<std::thread::JoinHandle<()>>>,
    /// The parse thread ("rikka-parse"), joined by
    /// [`Self::quiesce_for_transfer`] AFTER the reader: chunks the reader
    /// consumed may still sit in the channel or an open ?2026 sync buffer,
    /// and a replay serialized before they land in the Term loses them for
    /// good — the pipe no longer holds them for the receiver.
    pub parser_thread: FairMutex<Option<std::thread::JoinHandle<()>>>,
    /// Set by [`Self::quiesce_for_transfer`]: the settler thread skips every
    /// further apply, so no resize (grid or signal-pipe) can land after the
    /// receiver of a tab move took over — a straggler settling during the
    /// sender's teardown would fight the receiver's geometry, and ConPTY
    /// never repaints, so it would be permanent. Shared with the settler.
    pub pty_sealed: Arc<AtomicBool>,
    /// Session log sinks (Tera Term-style). `output_log` is fed every raw
    /// PTY byte by the reader thread; `input_log` receives what the user
    /// sends via [`Self::send_bytes`]. Set through [`Self::set_logging`].
    pub output_log: Arc<FairMutex<Option<std::fs::File>>>,
    pub input_log: Arc<FairMutex<Option<std::fs::File>>>,
    /// Windows: birth-time duplicates of the ConPTY handle set, stocked by
    /// [`pty_handoff::build_handoff_session`] for a later cross-process tab
    /// move. `None` = not transferable (SSH, legacy portable-pty) or already
    /// taken. Independent duplicates, never the live handles: the receiver's
    /// `DUPLICATE_CLOSE_SOURCE` pull consumes THESE, while the File objects
    /// the reader/writer threads hold stay valid for the teardown.
    #[cfg(windows)]
    pub transfer: FairMutex<Option<pty_handoff::TransferKit>>,
}

/// The grid's font size, settable once by the embedding app before any
/// window renders (a config value). A process-wide atomic instead of a
/// parameter: the size threads through measure_cell_metrics, render_grid
/// and the pane overlay across two applications — a signature change for
/// a value that never varies per call.
pub mod typography {
    use std::sync::atomic::{AtomicU32, Ordering};

    static FONT_SIZE_BITS: AtomicU32 = AtomicU32::new(0);

    /// Set the terminal font size in (logical) pixels. Call at startup.
    pub fn set_font_size(px_size: f32) {
        if px_size.is_finite() && px_size >= 6.0 && px_size <= 72.0 {
            FONT_SIZE_BITS.store(px_size.to_bits(), Ordering::Relaxed);
        }
    }

    /// The configured font size, defaulting to the classic 13px.
    pub fn font_size() -> gpui::Pixels {
        let bits = FONT_SIZE_BITS.load(Ordering::Relaxed);
        if bits == 0 {
            gpui::px(13.0)
        } else {
            gpui::px(f32::from_bits(bits))
        }
    }

    static LINE_HEIGHT_BITS: AtomicU32 = AtomicU32::new(0);

    /// Set the line-height multiplier (`cell_height = font_size × this`).
    pub fn set_line_height(mult: f32) {
        if mult.is_finite() && (1.0..=3.0).contains(&mult) {
            LINE_HEIGHT_BITS.store(mult.to_bits(), Ordering::Relaxed);
        }
    }

    /// The configured line-height multiplier. The 1.2 default matches what
    /// mainstream terminals (wt, alacritty, ghostty) get from their font
    /// metrics (ascent + descent + line gap ≈ 1.2 × font size) — the old
    /// fixed 1.5 read noticeably airier than any of them.
    pub fn line_height() -> f32 {
        let bits = LINE_HEIGHT_BITS.load(Ordering::Relaxed);
        if bits == 0 { 1.2 } else { f32::from_bits(bits) }
    }
}

impl TerminalSession {
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Resize this session's scrollback (history) capacity — applied to a
    /// live Term, so the embedder can set a configured value right after
    /// session assembly without threading one more parameter through
    /// every builder.
    pub fn set_scrollback(&self, lines: usize) {
        self.term.lock().set_scrolling_history(lines);
    }

    /// Mark this session as ConPTY-backed. Two consequences:
    /// - width resizes reflow like conhost's `TextBuffer::Reflow`
    ///   (`conpty_resize_semantics`, see that field's docs), and
    /// - the kitty keyboard protocol is NOT advertised (`CSI ? u` goes
    ///   unanswered, like wt): conhost never forwards the client's
    ///   push/pop to us, so the protocol cannot work through it anyway —
    ///   and OpenConsole 1.24 mis-parses the pop inside an exiting TUI's
    ///   restore burst and swallows everything after it, INCLUDING the
    ///   `?1049l` alt-screen exit. Advertising it left the tab stuck on
    ///   the alt screen after quitting yazi (2026-07-16); see
    ///   `pty_local::tests::alt_exit_probe` for the live evidence.
    pub fn mark_conpty(&self) {
        self.conpty_resize_semantics.store(true, Ordering::Relaxed);
        self.term.lock().set_kitty_keyboard(false);
    }

    /// Start/replace session logging (Tera Term-style): `output` receives
    /// every raw PTY byte (VT sequences included) from the reader thread;
    /// `input` receives what the USER sends (keys, IME commits, pastes —
    /// not protocol replies), so it may contain typed secrets: the
    /// embedder should keep it opt-in. `None` closes that side.
    pub fn set_logging(&self, output: Option<std::fs::File>, input: Option<std::fs::File>) {
        *self.output_log.lock() = output;
        *self.input_log.lock() = input;
    }

    /// Whether any log sink is currently attached.
    pub fn logging_active(&self) -> bool {
        self.output_log.lock().is_some() || self.input_log.lock().is_some()
    }

    /// Quiesce this session for a cross-process tab move: stop the blocking
    /// PTY reader (two readers on one pipe shred the VT stream, so ours must
    /// be provably dead BEFORE the receiver assembles its session and starts
    /// reading) and seal outbound PTY effects (a resize settling after the
    /// receiver adopted would fight its geometry). Irreversible — on a failed
    /// move the tab reads as disconnected, honestly.
    ///
    /// `CancelSynchronousIo` only aborts a syscall the thread is blocked in
    /// RIGHT NOW; between reads it misses (`ERROR_NOT_FOUND`), so keep poking
    /// until the loop observes a cancelled read and exits.
    #[cfg(windows)]
    pub fn quiesce_for_transfer(&self) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
        self.pty_sealed.store(true, Ordering::Relaxed);
        let Some(handle) = self.reader_thread.lock().take() else {
            return Ok(()); // no reader spawned, or already quiesced
        };
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !handle.is_finished() {
            unsafe { CancelSynchronousIo(handle.as_raw_handle()) };
            if std::time::Instant::now() > deadline {
                anyhow::bail!("PTY reader did not quiesce within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = handle.join();
        // Now drain the parser: chunks the (dead) reader already consumed
        // may still sit in the channel or an open ?2026 sync buffer, and a
        // replay serialized before they reach the Term loses them for good
        // — the pipe no longer holds them for the receiver. Reader death
        // dropped the channel sender, so the parser provably flushes
        // everything (its EOF path force-closes an open sync buffer) and
        // exits; the deadline only guards a wedged thread.
        if let Some(parser) = self.parser_thread.lock().take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !parser.is_finished() {
                if std::time::Instant::now() > deadline {
                    anyhow::bail!("PTY parser did not drain within 5s");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let _ = parser.join();
        }
        Ok(())
    }

    pub fn send_bytes(&self, bytes: &[u8]) {
        // Input logging taps HERE (not the writer): protocol replies from
        // the parse/clipboard threads also hit the writer, but only what
        // the user sends belongs in an input log.
        if let Some(log) = &mut *self.input_log.lock() {
            let _ = log.write_all(bytes);
        }
        let _ = self.writer.lock().write_all(bytes);
    }

    /// Focus reporting (DECSET ?1004): tell the application when this
    /// surface gains/loses the user's attention (`CSI I` / `CSI O`).
    /// De-duplicates, and only writes when the application opted in —
    /// callers may report every window-activation or tab-switch event.
    pub fn report_focus(&self, focused: bool) {
        use alacritty_terminal::term::TermMode;
        if self.focused.swap(focused, Ordering::Relaxed) == focused {
            return;
        }
        let mode = *self.term.lock().mode();
        if !mode.contains(TermMode::FOCUS_IN_OUT) {
            return;
        }
        self.send_bytes(if focused { b"\x1b[I" } else { b"\x1b[O" });
    }

    /// Route a wheel event to the PTY when the running application asked for
    /// it. Returns `false` when the wheel should scroll the local scrollback
    /// instead (plain primary-screen shell). See [`wheel_pty_bytes`].
    pub fn wheel_to_pty(&self, lines: i32, col: usize, row: usize, mods: ReportMods) -> bool {
        let mode = *self.term.lock().mode();
        match wheel_pty_bytes(mode, lines, col, row, mods) {
            Some(buf) => {
                if !buf.is_empty() {
                    self.send_bytes(&buf);
                }
                true
            }
            None => false,
        }
    }

    /// Begin a mouse selection at viewport cell `(row, col)`; `right_side` =
    /// the pointer sat in the right half of the cell. The selection lives in
    /// alacritty's `Selection` (grid coordinates), so it stays glued to the
    /// text through scrollback scrolling and output-driven rotation — the old
    /// app-side screen-row state slid off the content on any scroll.
    pub fn selection_begin(&self, row: usize, col: usize, right_side: bool) {
        let mut term = self.term.lock();
        let point = viewport_point(&term, row, col);
        term.selection = Some(alacritty_terminal::selection::Selection::new(
            alacritty_terminal::selection::SelectionType::Simple,
            point,
            side_of(right_side),
        ));
        self.refresh_snapshot(&term);
    }

    /// Move the selection head to viewport cell `(row, col)` while dragging.
    pub fn selection_update(&self, row: usize, col: usize, right_side: bool) {
        let mut term = self.term.lock();
        let point = viewport_point(&term, row, col);
        let side = side_of(right_side);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
        self.refresh_snapshot(&term);
    }

    /// Drop any selection in this pane.
    pub fn selection_clear(&self) {
        let mut term = self.term.lock();
        term.selection = None;
        self.refresh_snapshot(&term);
    }

    /// Rebuild the shared snapshot after a selection change — the parse
    /// thread only refreshes it on PTY output, and the highlight lives in the
    /// snapshot. Same term→snapshot lock order as the parse thread.
    fn refresh_snapshot(&self, term: &Term<ClipboardListener>) {
        *self.snapshot.lock() = take_snapshot(term);
    }

    /// The selected text — extracted by the grid itself, so it spans
    /// scrollback and handles wide/wrapped lines correctly.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Route horizontal wheel ticks to the PTY when the running application
    /// asked for mouse reporting. Returns whether the PTY owns the horizontal
    /// wheel; there is no local horizontal scroll to fall back to. See
    /// [`hwheel_pty_bytes`].
    pub fn hwheel_to_pty(&self, cols: i32, col: usize, row: usize, mods: ReportMods) -> bool {
        let mode = *self.term.lock().mode();
        match hwheel_pty_bytes(mode, cols, col, row, mods) {
            Some(buf) => {
                if !buf.is_empty() {
                    self.send_bytes(&buf);
                }
                true
            }
            None => false,
        }
    }

    /// Route a click/drag/motion event to the PTY when the running
    /// application asked for mouse reporting. Returns `false` when the event
    /// should be handled locally (selection) instead. See [`mouse_pty_bytes`].
    pub fn mouse_to_pty(
        &self,
        event: MouseReport,
        mods: ReportMods,
        col: usize,
        row: usize,
    ) -> bool {
        let mode = *self.term.lock().mode();
        match mouse_pty_bytes(mode, event, mods, col, row) {
            Some(buf) => {
                self.send_bytes(&buf);
                true
            }
            None => false,
        }
    }

    /// Paste text into the terminal, honoring bracketed-paste mode. Snaps the
    /// view back to the live bottom first, like typing does.
    pub fn paste(&self, text: &str) {
        let mode = *self.term.lock().mode();
        self.scroll_display_to_bottom();
        self.send_bytes(&paste_pty_bytes(mode, text));
    }

    /// Scroll the emulator's display window into the scrollback history
    /// (positive = older content) and refresh the snapshot immediately —
    /// the reader thread only refreshes it on PTY output.
    pub fn scroll_display(&self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        if delta == 0 {
            return;
        }
        let mut t = self.term.lock();
        let before = t.grid().display_offset();
        t.scroll_display(Scroll::Delta(delta));
        if t.grid().display_offset() == before {
            return; // already clamped at top/bottom — nothing to repaint
        }
        *self.snapshot.lock() = take_snapshot(&t);
        drop(t);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Snap the display window back to the live view (bottom).
    pub fn scroll_display_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        let mut t = self.term.lock();
        if t.grid().display_offset() == 0 {
            return;
        }
        t.scroll_display(Scroll::Bottom);
        *self.snapshot.lock() = take_snapshot(&t);
        drop(t);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Resize the terminal to the given dimensions.
    ///
    /// This updates the internal `alacritty_terminal::Term` geometry **and**
    /// notifies the backing PTY / SSH channel so that remote applications
    /// (e.g. tmux) can reflow their layout accordingly.
    /// `cell_px` is the renderer's cell size in logical pixels; it rides
    /// along so PTY consumers see real `TIOCGWINSZ` pixel dimensions (kitty
    /// graphics clients size images with them).
    pub fn resize(&self, cols: u16, rows: u16, cell_px: (f32, f32)) {
        // Everything — term reflow, snapshot, PTY notification — happens on
        // the debounced settler thread (see the `pty_resize` field docs):
        // the grid and the PTY must walk the same resize step sequence, so
        // there is no synchronous local path. The leading edge applies
        // within microseconds; a drag's transients coalesce away.
        let _ = self.pty_resize.send((cols, rows, cell_px.0, cell_px.1));
    }
}

/// Cursor presentation, mirrored from the grid (DECSCUSR shape + DECTCEM
/// visibility). Blink is intentionally not modelled yet — shapes render
/// steady.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShapeKind {
    /// Reverse-video cell (also stands in for HollowBlock — the engine does
    /// not model window focus).
    #[default]
    Block,
    /// Thin vertical bar at the cell's left edge.
    Beam,
    /// Thin bar along the cell's bottom edge.
    Underline,
    /// `?25l` — the cursor is not drawn at all.
    Hidden,
}

#[derive(Clone)]
pub struct GridSnapshot {
    #[allow(dead_code)]
    pub cols: usize,
    #[allow(dead_code)]
    pub rows: usize,
    pub cells: Vec<Vec<SnapshotCell>>,
    #[allow(dead_code)]
    pub cursor: (usize, usize),
    /// How to draw the cursor cell (DECSCUSR / DECTCEM).
    pub cursor_shape: CursorShapeKind,
    /// The cursor blinks (DECSCUSR 1/3/5 or DECSET ?12) — rides the same
    /// 600 ms phase and 300 ms refresh timer as SGR blink.
    pub cursor_blink: bool,
    /// Mouse selection mapped into viewport rows (inclusive reading-order
    /// range), when any part is visible. The selection itself lives in the
    /// grid (alacritty `Selection`), so it tracks scroll and output.
    pub selection: Option<((usize, usize), (usize, usize))>,
    /// Lines scrolled back into history (0 = live view at the bottom).
    pub display_offset: usize,
    /// Any visible cell carries SGR blink — the refresh task adds a phase
    /// timer only while this is set, keeping idle wakeups at zero otherwise.
    pub has_blink: bool,
    /// Deduplicated OSC 8 hyperlink URIs; cells refer in via
    /// [`SnapshotCell::link`]. alacritty parses the sequences, we only
    /// collect what `display_iter` hands out.
    pub links: Vec<String>,
    /// Any visible cell is a kitty-graphics placeholder — lets the renderer
    /// skip the per-row image scan entirely otherwise (same pattern as
    /// `has_blink`).
    pub has_images: bool,
}

impl GridSnapshot {
    pub fn blank(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![SnapshotCell::blank(); cols]; rows],
            cursor: (0, 0),
            cursor_shape: CursorShapeKind::default(),
            cursor_blink: false,
            selection: None,
            display_offset: 0,
            has_blink: false,
            links: Vec::new(),
            has_images: false,
        }
    }
}

#[derive(Clone)]
pub struct SnapshotCell {
    pub c: char,
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    /// 0 = skip render (wide spacer), 1 = half-width, 2 = wide (Flags::WIDE_CHAR).
    pub display_width: u8,
    pub style: CellStyle,
    /// Index into [`GridSnapshot::links`] when the cell belongs to an OSC 8
    /// hyperlink. Cells sharing an index underline together on ctrl-hover.
    pub link: Option<u16>,
    /// Kitty-graphics Unicode placeholder: this cell shows one tile of a
    /// transmitted image (see [`kitty_graphics`]). The renderer paints the
    /// tile from the session's image store; `c` is blanked to keep the
    /// undefined placeholder glyph from rendering as tofu.
    pub image: Option<kitty_graphics::PlaceholderCell>,
}

/// SGR attributes of a cell, resolved from alacritty's cell flags.
///
/// Colors are NOT pre-resolved here: `display_iter` hands out raw flags, and
/// applying INVERSE / DIM to actual RGB values is the renderer's job
/// (`resolve_run_colors`), where the default fg/bg are known.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    /// SGR 2 — faint: fg is dimmed toward the background.
    pub dim: bool,
    /// SGR 7 — fg/bg swapped at render time.
    pub inverse: bool,
    /// SGR 9 — strikethrough.
    pub strikeout: bool,
    /// SGR 8 — bg painted, ink skipped.
    pub hidden: bool,
    /// SGR 5/6 — ink hidden during the off phase (slow/rapid draw the same;
    /// vendored alacritty patch, upstream drops blink entirely).
    pub blink: bool,
    pub underline: UnderlineKind,
    /// SGR 58 — underline color; `None` follows the (post-inverse/dim) fg.
    pub underline_color: Option<ResolvedColor>,
}

/// Underline variant (SGR 4, 4:0-4:5, 21). Mutually exclusive per cell.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum UnderlineKind {
    #[default]
    None,
    Single,
    Double,
    /// 4:3 — curly, drawn wavy.
    Undercurl,
    Dotted,
    Dashed,
}

impl SnapshotCell {
    pub fn blank() -> Self {
        Self {
            c: ' ',
            fg: ResolvedColor::Default,
            bg: ResolvedColor::Default,
            display_width: 1,
            style: CellStyle::default(),
            link: None,
            image: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResolvedColor {
    Default,
    Rgb(u8, u8, u8),
}

/// Encode pasted text for the PTY.
///
/// - Bracketed-paste mode (`?2004`, requested by claude code / opencode /
///   modern shells): wrap in `ESC[200~ … ESC[201~` so the app can tell a
///   paste from typing (multiline text stops auto-executing per line). Any
///   embedded end marker is stripped — otherwise clipboard content could
///   break out of the bracket and inject keystrokes.
/// - Plain mode: newlines are normalized to CR, which is what a terminal
///   sends for the Enter key.
pub fn paste_pty_bytes(mode: alacritty_terminal::term::TermMode, text: &str) -> Vec<u8> {
    use alacritty_terminal::term::TermMode;
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let sanitized = text.replace("\x1b[201~", "");
        let mut buf = Vec::with_capacity(sanitized.len() + 12);
        buf.extend_from_slice(b"\x1b[200~");
        buf.extend_from_slice(sanitized.as_bytes());
        buf.extend_from_slice(b"\x1b[201~");
        buf
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Encode a wheel event for the PTY according to the terminal's modes, or
/// `None` when the wheel should scroll the local scrollback instead.
///
/// - Mouse-reporting apps (btop, htop, tmux `mouse on`): mouse buttons 64/65
///   at the pointed-at cell, SGR (`?1006`) or X10 flavor.
/// - Alt-screen apps with alternate-scroll (`?1007`): arrow keys, honoring
///   DECCKM application-cursor mode (less, vim without mouse).
///
/// `lines > 0` = wheel up. `col`/`row` are 0-based grid coordinates.
/// `mods` follows Ghostty/xterm: ctrl/alt bits are ORed into the wheel
/// button code so apps see ctrl+scroll (zoom) etc.
pub fn wheel_pty_bytes(
    mode: alacritty_terminal::term::TermMode,
    lines: i32,
    col: usize,
    row: usize,
    mods: ReportMods,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let btn: u32 = if lines > 0 { 64 } else { 65 } + mods.bits();
        let mut buf = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            if let Some(seq) = press_bytes(mode, btn, col, row) {
                buf.extend_from_slice(&seq);
            }
        }
        Some(buf)
    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        let mut buf = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            buf.extend_from_slice(seq);
        }
        Some(buf)
    } else {
        None
    }
}

/// Encode horizontal wheel ticks (xterm buttons 66 = left / 67 = right;
/// positive `cols` = wheel left, matching gpui's sign convention) for the
/// PTY, or `None` when the application did not ask for mouse reporting.
/// Unlike the vertical wheel there is no alternate-scroll fallback — no
/// arrow-key mapping is defined for horizontal scroll.
pub fn hwheel_pty_bytes(
    mode: alacritty_terminal::term::TermMode,
    cols: i32,
    col: usize,
    row: usize,
    mods: ReportMods,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let btn: u32 = if cols > 0 { 66 } else { 67 } + mods.bits();
    let mut buf = Vec::new();
    for _ in 0..cols.unsigned_abs() {
        if let Some(seq) = press_bytes(mode, btn, col, row) {
            buf.extend_from_slice(&seq);
        }
    }
    Some(buf)
}

/// X10 mouse encoding: `ESC [ M cb+32 x+32 y+32` with 1-based coordinates.
/// Coordinates past 223 (255 − 32) cannot be represented; like Ghostty and
/// xterm the event is dropped rather than clamped to a fabricated cell.
/// Viewport cell → grid coordinates at the current scroll position (screen
/// row r maps to grid `Line(r - display_offset)`).
fn viewport_point<L>(term: &Term<L>, row: usize, col: usize) -> alacritty_terminal::index::Point {
    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Line, Point};
    let offset = term.grid().display_offset() as i32;
    let col = col.min(term.columns().saturating_sub(1));
    Point::new(Line(row as i32 - offset), Column(col))
}

fn side_of(right: bool) -> alacritty_terminal::index::Side {
    if right {
        alacritty_terminal::index::Side::Right
    } else {
        alacritty_terminal::index::Side::Left
    }
}

/// A log record that is pure teardown noise, dropped by the file logger.
/// gpui's platform callbacks (frame request, activation, hover, input)
/// race the window's removal: every callback still in flight when a
/// window closes reports `window not found` at ERROR through `log_err()`.
/// Expected on every single close, actionable never — 72 lines in one
/// day's log. Matched exactly (target + full message) so any OTHER gpui
/// error still lands.
fn is_benign_log_noise(target: &str, message: &str) -> bool {
    target.starts_with("gpui") && message == "window not found"
}

/// Minimal warn+ file logger: `%TEMP%/shogun-tsf/{app}.log`. The GUI shells
/// never initialize a `log` logger, so everything gpui reports through
/// `log_err()` / `log::error!` has been silently discarded since day one —
/// the stowaway console (now gone) was empty for the same reason. Truncates
/// past 5 MB at startup; no-op if a logger is already set.
pub fn install_file_logger(app: &str) {
    use std::io::Write as _;

    struct FileLogger {
        path: std::path::PathBuf,
        lock: std::sync::Mutex<()>,
    }
    impl log::Log for FileLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            if is_benign_log_noise(record.target(), &record.args().to_string()) {
                return;
            }
            let _guard = self.lock.lock();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                let _ = writeln!(
                    f,
                    "[{:?} {} {}] {}",
                    std::time::SystemTime::now(),
                    record.level(),
                    record.target(),
                    record.args()
                );
            }
        }
        fn flush(&self) {}
    }

    let dir = std::env::temp_dir().join("shogun-tsf");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{app}.log"));
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > 5 * 1024 * 1024
    {
        let _ = std::fs::File::create(&path);
    }
    let _ = log::set_boxed_logger(Box::new(FileLogger {
        path,
        lock: std::sync::Mutex::new(()),
    }))
    .map(|()| log::set_max_level(log::LevelFilter::Warn));
}

/// Append every panic (any thread) to `%TEMP%/shogun-tsf/panic.log` — the
/// GUI shells have no visible stderr, and a dead parse thread otherwise
/// reports itself only as a frozen grid (or, when it dies right after a
/// frame-opening ED2, a permanently black one). Chains the default hook.
/// Call once at startup, from every product shell.
pub fn install_panic_log() {
    let dir = std::env::temp_dir().join("shogun-tsf");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("panic.log");
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let thread = std::thread::current();
            let _ = writeln!(
                f,
                "==== panic thread={} at {:?} ====\n{info}\n{}\n",
                thread.name().unwrap_or("<unnamed>"),
                std::time::SystemTime::now(),
                std::backtrace::Backtrace::force_capture(),
            );
        }
        prev(info);
    }));
}

/// Encode a press-type report (wheel tick, button press) in the negotiated
/// coordinate encoding, by precedence: SGR (`?1006`) > UTF-8 (`?1005`) > X10.
/// `None` when the coordinates do not fit the encoding's range.
fn press_bytes(
    mode: alacritty_terminal::term::TermMode,
    cb: u32,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    if mode.contains(TermMode::SGR_MOUSE) {
        Some(format!("\x1b[<{cb};{};{}M", col + 1, row + 1).into_bytes())
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        utf8_bytes(cb, col, row)
    } else {
        x10_bytes(cb, col, row).map(|b| b.to_vec())
    }
}

/// UTF-8 flavored X10 (`?1005`): `CSI M` with Cb/Cx/Cy each encoded as a
/// UTF-8 character, which extends the coordinate range from X10's 223 to
/// 2015 (the xterm-documented limit).
fn utf8_bytes(cb: u32, col: usize, row: usize) -> Option<Vec<u8>> {
    let (x, y) = (col + 1, row + 1);
    if x > 2015 || y > 2015 {
        return None;
    }
    let mut buf = b"\x1b[M".to_vec();
    for v in [32 + cb, (32 + x) as u32, (32 + y) as u32] {
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(char::from_u32(v)?.encode_utf8(&mut tmp).as_bytes());
    }
    Some(buf)
}

fn x10_bytes(cb: u32, col: usize, row: usize) -> Option<[u8; 6]> {
    let (x, y) = (col + 1, row + 1);
    if x > 223 || y > 223 {
        return None;
    }
    Some([
        0x1b,
        b'[',
        b'M',
        (32 + cb).min(255) as u8,
        (32 + x) as u8,
        (32 + y) as u8,
    ])
}

/// Mouse buttons that participate in click/drag reporting. The numeric value
/// is the xterm button code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportButton {
    Left = 0,
    Middle = 1,
    /// Part of the wire protocol but never forwarded today: right-click
    /// keeps the local context menu (see `selection::report_button`).
    #[allow(dead_code)]
    Right = 2,
}

/// A pointer event to (maybe) report to the application.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseReport {
    Press(ReportButton),
    Release(ReportButton),
    /// Pointer moved. `Some(btn)` while a button is held (drag), `None` for
    /// hover motion (only reported under `?1003`).
    Motion(Option<ReportButton>),
}

/// Modifier bits added to the xterm button code (shift is intentionally
/// absent: shift+click is the universal "bypass reporting, select locally"
/// escape hatch and never reaches the encoder).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ReportMods {
    pub alt: bool,
    pub ctrl: bool,
}

impl ReportMods {
    fn bits(self) -> u32 {
        (if self.alt { 8 } else { 0 }) + (if self.ctrl { 16 } else { 0 })
    }
}

/// Encode a click/drag/motion event for the PTY, or `None` when the running
/// application did not ask for that class of event.
///
/// Reporting tiers (each includes the previous):
/// - `?1000` (MOUSE_REPORT_CLICK): press + release
/// - `?1002` (MOUSE_DRAG): + motion while a button is held
/// - `?1003` (MOUSE_MOTION): + all motion
///
/// Encoding is SGR (`?1006`, `CSI < Cb;col;row M|m`) when negotiated, X10
/// otherwise (coordinates capped at 223; release loses the button identity —
/// that is the protocol, not a bug). Wheel has its own path
/// ([`wheel_pty_bytes`]).
pub fn mouse_pty_bytes(
    mode: alacritty_terminal::term::TermMode,
    event: MouseReport,
    mods: ReportMods,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    let wanted = match event {
        MouseReport::Press(_) | MouseReport::Release(_) => TermMode::MOUSE_MODE,
        MouseReport::Motion(Some(_)) => TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        MouseReport::Motion(None) => TermMode::MOUSE_MOTION,
    };
    if !mode.intersects(wanted) {
        return None;
    }
    // Cb: button id + 32 for motion + modifier bits. Motion without a button
    // reports the "released" id 3.
    let (btn, motion, release) = match event {
        MouseReport::Press(b) => (b as u32, false, false),
        MouseReport::Release(b) => (b as u32, false, true),
        MouseReport::Motion(b) => (b.map_or(3, |b| b as u32), true, false),
    };
    let cb = btn + if motion { 32 } else { 0 } + mods.bits();
    if mode.contains(TermMode::SGR_MOUSE) {
        let m = if release { 'm' } else { 'M' };
        Some(format!("\x1b[<{cb};{};{}{m}", col + 1, row + 1).into_bytes())
    } else {
        // Outside SGR a release is always button 3 — that is the protocol,
        // not a bug. ?1005 only widens the coordinate range.
        let cb = if release { 3 + mods.bits() } else { cb };
        if mode.contains(TermMode::UTF8_MOUSE) {
            utf8_bytes(cb, col, row)
        } else {
            // X10: coordinates cap at 223 (255 - 32).
            x10_bytes(cb, col, row).map(|b| b.to_vec())
        }
    }
}

/// alacritty_terminal の Cell/Color → SnapshotCell に変換
///
/// Generic over any `EventListener` so tests can use `VoidListener` while
/// production code uses `ClipboardListener`.
pub fn take_snapshot<L: EventListener>(term: &Term<L>) -> GridSnapshot {
    use alacritty_terminal::term::cell::Flags;

    let content = term.renderable_content();
    let cols = term.columns();
    let rows = term.screen_lines();
    // display_iter yields the viewport in GRID coordinates: with the display
    // scrolled back by `display_offset`, visible lines run from
    // -display_offset (oldest history row on screen) to
    // rows - 1 - display_offset. Screen row = grid line + display_offset.
    // Casting the raw line to usize instead silently dropped every history
    // line (negative → huge) and drew the rest shifted up — scrollback
    // appeared to move the wrong way (bug found 2026-07-05).
    let display_offset = content.display_offset as i32;
    let mut cells = vec![vec![SnapshotCell::blank(); cols]; rows];
    let mut has_blink = false;
    let mut has_images = false;
    // OSC 8 hyperlinks, deduplicated by URI. u16 is plenty for one viewport;
    // links past the cap keep the last slot rather than panicking.
    let mut links: Vec<String> = Vec::new();
    let mut link_ids: std::collections::HashMap<String, u16> = std::collections::HashMap::new();
    // Soft-wrap flags per screen row (WRAPLINE sits on the row's last cell),
    // so bare-URL detection can join continuation rows.
    let mut wrapped = vec![false; rows];

    for indexed in content.display_iter {
        let row = (indexed.point.line.0 + display_offset) as usize;
        let col = indexed.point.column.0;
        if row < rows && col < cols {
            let is_spacer = indexed
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let display_width = if is_spacer {
                0
            } else if indexed.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let flags = indexed.flags;
            let blink = flags.contains(Flags::BLINK);
            has_blink |= blink;
            wrapped[row] |= flags.contains(Flags::WRAPLINE);
            let link = indexed
                .hyperlink()
                .map(|h| intern_link(&mut links, &mut link_ids, h.uri()));
            // Kitty-graphics placeholder: tile coordinates ride the cell's
            // combining diacritics, the image id rides the RAW fg color
            // (a palette index IS an id — resolving it through the palette
            // would corrupt it). Runs with omitted diacritics continue from
            // the left neighbor, already decoded (display_iter is row-major).
            let image = if indexed.c == kitty_graphics::PLACEHOLDER {
                let prev = col.checked_sub(1).and_then(|pc| cells[row][pc].image);
                kitty_graphics::decode_placeholder(
                    raw_color24(indexed.fg),
                    indexed.zerowidth().unwrap_or(&[]),
                    prev,
                )
            } else {
                None
            };
            has_images |= image.is_some();
            cells[row][col] = SnapshotCell {
                // Blank the undefined placeholder glyph (tofu); the image
                // tile is painted by the renderer instead.
                c: if image.is_some() { ' ' } else { indexed.c },
                fg: resolve_color(indexed.fg, content.colors),
                bg: resolve_color(indexed.bg, content.colors),
                display_width,
                style: CellStyle {
                    bold: flags.contains(Flags::BOLD),
                    italic: flags.contains(Flags::ITALIC),
                    dim: flags.contains(Flags::DIM),
                    inverse: flags.contains(Flags::INVERSE),
                    strikeout: flags.contains(Flags::STRIKEOUT),
                    hidden: flags.contains(Flags::HIDDEN),
                    blink,
                    underline: if flags.contains(Flags::UNDERCURL) {
                        UnderlineKind::Undercurl
                    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
                        UnderlineKind::Double
                    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
                        UnderlineKind::Dotted
                    } else if flags.contains(Flags::DASHED_UNDERLINE) {
                        UnderlineKind::Dashed
                    } else if flags.contains(Flags::UNDERLINE) {
                        UnderlineKind::Single
                    } else {
                        UnderlineKind::None
                    },
                    underline_color: indexed
                        .underline_color()
                        .map(|c| resolve_color(c, content.colors)),
                },
                link,
                image,
            };
        }
    }

    detect_implicit_links(&mut cells, &wrapped, &mut links, &mut link_ids);
    let links = assign_link_occurrences(&mut cells, &wrapped, &links);

    // SGR-applied affordance: every linked cell without an underline of its
    // own gets a dotted accent underline, so click targets are visible
    // before any hover. Rides the existing run machinery (underline kind +
    // SGR 58 color are already per-cell style); ctrl-hover then paints the
    // solid line on top.
    for row in &mut cells {
        for cell in row {
            if cell.link.is_some() && cell.style.underline == UnderlineKind::None {
                cell.style.underline = UnderlineKind::Dotted;
                cell.style.underline_color = Some(LINK_UNDERLINE_COLOR);
            }
        }
    }

    // Selection mapped into viewport rows. alacritty keeps it in grid
    // coordinates (rotating with scroll and output); only the visible part is
    // clamped here — `None` when it is entirely off-screen.
    let selection = term
        .selection
        .as_ref()
        .and_then(|s| s.to_range(term))
        .and_then(|r| {
            let to_view = |l: alacritty_terminal::index::Line| l.0 + display_offset;
            let (sr, er) = (to_view(r.start.line), to_view(r.end.line));
            if er < 0 || sr >= rows as i32 {
                return None;
            }
            let start = if sr < 0 {
                (0, 0)
            } else {
                (sr as usize, r.start.column.0)
            };
            let end = if er >= rows as i32 {
                (rows - 1, cols.saturating_sub(1))
            } else {
                (er as usize, r.end.column.0.min(cols.saturating_sub(1)))
            };
            Some((start, end))
        });

    let cur = content.cursor.point;
    // RenderableCursor folds DECTCEM in for us: `?25l` arrives as Hidden.
    let cursor_shape = {
        use alacritty_terminal::vte::ansi::CursorShape as Shape;
        match content.cursor.shape {
            Shape::Beam => CursorShapeKind::Beam,
            Shape::Underline => CursorShapeKind::Underline,
            Shape::Hidden => CursorShapeKind::Hidden,
            // HollowBlock only differs for unfocused windows, which the
            // engine does not model — draw it solid.
            _ => CursorShapeKind::Block,
        }
    };
    GridSnapshot {
        cols,
        rows,
        cells,
        // Same grid→screen shift; scrolled back far enough the cursor row
        // passes `rows` and simply stops matching any painted row.
        cursor: ((cur.line.0 + display_offset) as usize, cur.column.0),
        cursor_shape,
        // Hidden gates the blink flag so `?25l` apps don't arm the refresh
        // timer for a cursor that never draws.
        cursor_blink: term.cursor_style().blinking && cursor_shape != CursorShapeKind::Hidden,
        selection,
        display_offset: term.grid().display_offset(),
        has_blink,
        links,
        has_images,
    }
}

/// Raw 24-bit value of a cell fg color, for kitty-graphics placeholder ids:
/// `Spec` packs to RGB, `Indexed(n)` IS the id `n` (SGR 38;5;n for ids
/// < 256), named colors carry no id.
fn raw_color24(color: alacritty_terminal::vte::ansi::Color) -> u32 {
    use alacritty_terminal::vte::ansi::Color;
    match color {
        Color::Spec(rgb) => (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b),
        Color::Indexed(n) => u32::from(n),
        Color::Named(_) => 0,
    }
}

/// Accent for the always-on dotted underline under hyperlink cells.
const LINK_UNDERLINE_COLOR: ResolvedColor = ResolvedColor::Rgb(0x58, 0xa6, 0xff);

/// Intern `uri` into the snapshot's link table, deduplicated.
fn intern_link(
    links: &mut Vec<String>,
    ids: &mut std::collections::HashMap<String, u16>,
    uri: &str,
) -> u16 {
    *ids.entry(uri.to_string()).or_insert_with(|| {
        let idx = links.len().min(u16::MAX as usize) as u16;
        if links.len() <= u16::MAX as usize {
            links.push(uri.to_string());
        }
        idx
    })
}

/// Detect bare `http(s)://` URLs in the viewport text and link their cells,
/// like Ghostty/wt do without any OSC 8 markup. Soft-wrapped rows are joined
/// (via the WRAPLINE flags) so a URL broken across lines stays one link.
/// Explicit OSC 8 always wins: spans touching an already-linked cell are
/// skipped.
fn detect_implicit_links(
    cells: &mut [Vec<SnapshotCell>],
    wrapped: &[bool],
    links: &mut Vec<String>,
    link_ids: &mut std::collections::HashMap<String, u16>,
) {
    let rows = cells.len();
    let mut r = 0;
    while r < rows {
        let mut last = r;
        while last + 1 < rows && wrapped[last] {
            last += 1;
        }
        // The joined row group as chars, with each char's home cell.
        let mut chars: Vec<char> = Vec::new();
        let mut pos: Vec<(usize, usize, u8)> = Vec::new();
        for row in r..=last {
            for (col, cell) in cells[row].iter().enumerate() {
                if cell.display_width == 0 {
                    continue;
                }
                chars.push(cell.c);
                pos.push((row, col, cell.display_width));
            }
        }
        for (start, end, uri) in detect_urls(&chars) {
            if pos[start..end]
                .iter()
                .any(|&(row, col, _)| cells[row][col].link.is_some())
            {
                continue;
            }
            let idx = intern_link(links, link_ids, &uri);
            for &(row, col, width) in &pos[start..end] {
                cells[row][col].link = Some(idx);
                // Wide glyph: link the spacer half too, so hit-testing on
                // either half of the character finds the link.
                if width == 2 && col + 1 < cells[row].len() {
                    cells[row][col + 1].link = Some(idx);
                }
            }
        }
        r = last + 1;
    }
}

/// Split the URI-deduplicated link indices into per-OCCURRENCE indices:
/// hovering one occurrence of a URL must not underline every other place the
/// same URL appears on screen (殿 feedback 2026-07-06). An occurrence is a
/// contiguous cell run — wide-glyph spacers never break one, and it continues
/// onto the next row only across a soft wrap. Returns the occurrence-indexed
/// URI table (duplicates now allowed) and remaps every cell in place.
fn assign_link_occurrences(
    cells: &mut [Vec<SnapshotCell>],
    wrapped: &[bool],
    uris: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The open occurrence: (uri index it continues, its new index).
    let mut current: Option<(u16, u16)> = None;
    for (r, row) in cells.iter_mut().enumerate() {
        for cell in row.iter_mut() {
            if cell.display_width == 0 {
                // Spacer half of a wide glyph: rides its base cell's
                // occurrence, never opens or closes one.
                if cell.link.is_some()
                    && let Some((_, new)) = current
                {
                    cell.link = Some(new);
                }
                continue;
            }
            match (cell.link, current) {
                (Some(old), Some((cur_old, new))) if old == cur_old => cell.link = Some(new),
                (Some(old), _) => {
                    let new = out.len().min(u16::MAX as usize) as u16;
                    if out.len() <= u16::MAX as usize {
                        out.push(uris[old as usize].clone());
                    }
                    current = Some((old, new));
                    cell.link = Some(new);
                }
                (None, _) => current = None,
            }
        }
        if !wrapped.get(r).copied().unwrap_or(false) {
            current = None;
        }
    }
    out
}

/// `[start, end)` char spans of bare URLs in `chars`, with the URI text.
fn detect_urls(chars: &[char]) -> Vec<(usize, usize, String)> {
    fn is_url_char(c: char) -> bool {
        c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '`')
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let lower_eq = |off: usize, pat: &str| {
            chars.len() >= i + off + pat.len()
                && pat
                    .chars()
                    .zip(&chars[i + off..])
                    .all(|(p, &c)| c.to_ascii_lowercase() == p)
        };
        let scheme = if lower_eq(0, "https://") {
            8
        } else if lower_eq(0, "http://") {
            7
        } else {
            i += 1;
            continue;
        };
        let mut j = i + scheme;
        while j < chars.len() && is_url_char(chars[j]) {
            j += 1;
        }
        // Trim trailing prose punctuation, and a closing paren/bracket only
        // when unbalanced — "https://en.wikipedia.org/wiki/Rust_(language)"
        // keeps its paren, "(see https://example.com)" loses it.
        let mut end = j;
        while end > i + scheme {
            match chars[end - 1] {
                '.' | ',' | ';' | ':' | '!' | '?' => end -= 1,
                ')' | ']' => {
                    let (open, close) = match chars[end - 1] {
                        ')' => ('(', ')'),
                        _ => ('[', ']'),
                    };
                    let span = &chars[i..end];
                    let opens = span.iter().filter(|&&c| c == open).count();
                    let closes = span.iter().filter(|&&c| c == close).count();
                    if closes > opens {
                        end -= 1;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        if end > i + scheme {
            out.push((i, end, chars[i..end].iter().collect()));
        }
        i = j.max(i + 1);
    }
    out
}

fn resolve_color(
    color: alacritty_terminal::vte::ansi::Color,
    table: &alacritty_terminal::term::color::Colors,
) -> ResolvedColor {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    match color {
        Color::Named(NamedColor::Foreground) | Color::Named(NamedColor::Background) => {
            ResolvedColor::Default
        }
        Color::Named(named) => table[named]
            .map(|rgb| ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b))
            .or_else(|| fallback_named_color(named))
            .unwrap_or(ResolvedColor::Default),
        Color::Indexed(idx) => table[idx as usize]
            .map(|rgb| ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b))
            .or_else(|| fallback_indexed_color(idx))
            .unwrap_or(ResolvedColor::Default),
        Color::Spec(rgb) => ResolvedColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// xterm palette fallback when the term color table has no entry yet.
fn fallback_named_color(named: alacritty_terminal::vte::ansi::NamedColor) -> Option<ResolvedColor> {
    use alacritty_terminal::vte::ansi::NamedColor;
    let (r, g, b) = match named {
        NamedColor::Black => (0x1e, 0x1e, 0x1e),
        NamedColor::Red => (0xcc, 0x00, 0x00),
        NamedColor::Green => (0x4e, 0x9a, 0x06),
        NamedColor::Yellow => (0xc4, 0xa0, 0x00),
        NamedColor::Blue => (0x34, 0x65, 0xa4),
        NamedColor::Magenta => (0x75, 0x50, 0x7b),
        NamedColor::Cyan => (0x06, 0x98, 0x9a),
        NamedColor::White => (0xd3, 0xd7, 0xcf),
        NamedColor::BrightBlack => (0x55, 0x57, 0x53),
        NamedColor::BrightRed => (0xef, 0x29, 0x29),
        NamedColor::BrightGreen => (0x8a, 0xe2, 0x34),
        NamedColor::BrightYellow => (0xfc, 0xe9, 0x4f),
        NamedColor::BrightBlue => (0x72, 0x9f, 0xcf),
        NamedColor::BrightMagenta => (0xad, 0x7f, 0xa8),
        NamedColor::BrightCyan => (0x34, 0xe2, 0xe2),
        NamedColor::BrightWhite => (0xee, 0xee, 0xec),
        _ => return None,
    };
    Some(ResolvedColor::Rgb(r, g, b))
}

fn fallback_indexed_color(idx: u8) -> Option<ResolvedColor> {
    let (r, g, b) = match idx {
        0..=7 => {
            let base = fallback_named_color(match idx {
                0 => alacritty_terminal::vte::ansi::NamedColor::Black,
                1 => alacritty_terminal::vte::ansi::NamedColor::Red,
                2 => alacritty_terminal::vte::ansi::NamedColor::Green,
                3 => alacritty_terminal::vte::ansi::NamedColor::Yellow,
                4 => alacritty_terminal::vte::ansi::NamedColor::Blue,
                5 => alacritty_terminal::vte::ansi::NamedColor::Magenta,
                6 => alacritty_terminal::vte::ansi::NamedColor::Cyan,
                _ => alacritty_terminal::vte::ansi::NamedColor::White,
            })?;
            let ResolvedColor::Rgb(r, g, b) = base else {
                return None;
            };
            (r, g, b)
        }
        8..=15 => {
            let base = fallback_named_color(match idx - 8 {
                0 => alacritty_terminal::vte::ansi::NamedColor::BrightBlack,
                1 => alacritty_terminal::vte::ansi::NamedColor::BrightRed,
                2 => alacritty_terminal::vte::ansi::NamedColor::BrightGreen,
                3 => alacritty_terminal::vte::ansi::NamedColor::BrightYellow,
                4 => alacritty_terminal::vte::ansi::NamedColor::BrightBlue,
                5 => alacritty_terminal::vte::ansi::NamedColor::BrightMagenta,
                6 => alacritty_terminal::vte::ansi::NamedColor::BrightCyan,
                _ => alacritty_terminal::vte::ansi::NamedColor::BrightWhite,
            })?;
            let ResolvedColor::Rgb(r, g, b) = base else {
                return None;
            };
            (r, g, b)
        }
        16..=231 => {
            let n = idx - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let v = |x: u8| if x == 0 { 0u8 } else { x * 40 + 55 };
            (v(r), v(g), v(b))
        }
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            (v, v, v)
        }
    };
    Some(ResolvedColor::Rgb(r, g, b))
}

/// Resolve an OSC color-query index the way the renderer would paint it: an
/// explicitly set palette entry (OSC 4/10/11 set) wins; otherwise indices
/// 0–255 use the engine's standard palette, and the dynamic specials mirror
/// the renderer defaults (`renderer::default_fg` #E8DCC8 / `default_bg`
/// #1A1A1A; the cursor is drawn in fg). `None` = stay silent, matching
/// xterm's behavior for unset specials.
pub(crate) fn query_color_rgb(
    colors: &alacritty_terminal::term::color::Colors,
    idx: usize,
) -> Option<alacritty_terminal::vte::ansi::Rgb> {
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
    if idx >= alacritty_terminal::term::color::COUNT {
        return None;
    }
    if let Some(rgb) = colors[idx] {
        return Some(rgb);
    }
    if idx < 256 {
        return match fallback_indexed_color(idx as u8) {
            Some(ResolvedColor::Rgb(r, g, b)) => Some(Rgb { r, g, b }),
            _ => None,
        };
    }
    if idx == NamedColor::Foreground as usize || idx == NamedColor::Cursor as usize {
        Some(Rgb {
            r: 0xE8,
            g: 0xDC,
            b: 0xC8,
        })
    } else if idx == NamedColor::Background as usize {
        Some(Rgb {
            r: 0x1A,
            g: 0x1A,
            b: 0x1A,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::{Processor, StdSyncHandler},
    };

    fn make_term(cols: usize, rows: usize) -> Term<VoidListener> {
        Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener)
    }

    fn advance_bytes(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser = Processor::<StdSyncHandler>::new();
        for &byte in bytes {
            parser.advance(term, byte);
        }
    }

    #[test]
    fn snapshot_scrollback_shows_history_rows() {
        let mut term = make_term(4, 2);
        // Five lines through a 2-row screen: 1..=3 scroll into history.
        advance_bytes(&mut term, b"1\r\n2\r\n3\r\n4\r\n5");
        let snap = take_snapshot(&term);
        assert_eq!(snap.cells[0][0].c, '4');
        assert_eq!(snap.cells[1][0].c, '5');

        // One step back: viewport shifts one row into history. The raw
        // grid-line cast used to drop the history row and draw '4','5'
        // shifted up — scrollback looked like it moved the wrong way.
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(1));
        let snap = take_snapshot(&term);
        assert_eq!(snap.display_offset, 1);
        assert_eq!(snap.cells[0][0].c, '3');
        assert_eq!(snap.cells[1][0].c, '4');
        // The cursor ('5' row) scrolls off the bottom and must not match
        // any painted row.
        assert!(snap.cursor.0 >= snap.rows);

        term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cells[0][0].c, '1');
        assert_eq!(snap.cells[1][0].c, '2');
    }

    #[test]
    fn snapshot_decodes_kitty_placeholder_cells() {
        let mut term = make_term(10, 2);
        // fg = 256-color index 42 (the image id), then two placeholder cells:
        // explicit (row 0, col 0) diacritics, then a bare continuation.
        // U+0305 = diacritic #0.
        advance_bytes(
            &mut term,
            "\x1b[38;5;42m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\x1b[0m".as_bytes(),
        );
        let snap = take_snapshot(&term);
        assert!(snap.has_images);
        use kitty_graphics::PlaceholderCell;
        assert_eq!(
            snap.cells[0][0].image,
            Some(PlaceholderCell {
                id: 42,
                row: 0,
                col: 0
            })
        );
        assert_eq!(
            snap.cells[0][1].image,
            Some(PlaceholderCell {
                id: 42,
                row: 0,
                col: 1
            })
        );
        // The undefined placeholder glyph must not reach the shaper (tofu).
        assert_eq!(snap.cells[0][0].c, ' ');
        assert!(snap.cells[0][2].image.is_none());
    }

    #[test]
    fn snapshot_kitty_placeholder_rgb_fg_carries_24bit_id() {
        let mut term = make_term(10, 2);
        // fg = direct RGB 0x010203 → image id 0x010203; row diacritic #1
        // (U+030D), col diacritic #2 (U+030E).
        advance_bytes(
            &mut term,
            "\x1b[38;2;1;2;3m\u{10EEEE}\u{030D}\u{030E}\x1b[0m".as_bytes(),
        );
        let snap = take_snapshot(&term);
        assert_eq!(
            snap.cells[0][0].image,
            Some(kitty_graphics::PlaceholderCell {
                id: 0x010203,
                row: 1,
                col: 2
            })
        );
    }

    #[test]
    fn snapshot_collects_osc8_hyperlinks() {
        let mut term = make_term(20, 2);
        // "click me" wrapped in OSC 8, then a plain word, then a second link
        // with the SAME uri — separate occurrences get separate indices, so
        // ctrl-hovering one does not underline the other (殿 feedback).
        advance_bytes(
            &mut term,
            b"\x1b]8;;https://example.com/\x1b\\click\x1b]8;;\x1b\\ x \x1b]8;;https://example.com/\x1b\\me\x1b]8;;\x1b\\",
        );
        let snap = take_snapshot(&term);
        assert_eq!(
            snap.links,
            vec![
                "https://example.com/".to_string(),
                "https://example.com/".to_string()
            ]
        );
        for col in 0..5 {
            assert_eq!(snap.cells[0][col].link, Some(0), "col {col}");
        }
        assert_eq!(snap.cells[0][5].link, None); // the " x " gap
        assert_eq!(snap.cells[0][8].link, Some(1)); // "me": same uri, own occurrence
        assert_eq!(snap.cells[1][0].link, None);
    }

    #[test]
    fn snapshot_distinct_uris_get_distinct_indices() {
        let mut term = make_term(30, 2);
        advance_bytes(
            &mut term,
            b"\x1b]8;;https://a.example/\x1b\\A\x1b]8;;https://b.example/\x1b\\B\x1b]8;;\x1b\\",
        );
        let snap = take_snapshot(&term);
        assert_eq!(snap.links.len(), 2);
        let (a, b) = (snap.cells[0][0].link, snap.cells[0][1].link);
        assert_eq!(a, Some(0));
        assert_eq!(b, Some(1));
        assert_eq!(snap.links[0], "https://a.example/");
        assert_eq!(snap.links[1], "https://b.example/");
    }

    #[test]
    fn implicit_bare_url_is_detected_and_styled() {
        let mut term = make_term(40, 2);
        advance_bytes(&mut term, b"see https://example.com/x now");
        let snap = take_snapshot(&term);
        assert_eq!(snap.links, vec!["https://example.com/x".to_string()]);
        // "see " unlinked, the URL linked, " now" unlinked.
        assert_eq!(snap.cells[0][3].link, None);
        for col in 4..25 {
            assert_eq!(snap.cells[0][col].link, Some(0), "col {col}");
        }
        assert_eq!(snap.cells[0][25].link, None);
        // The SGR affordance: dotted accent underline on link cells only.
        assert_eq!(snap.cells[0][4].style.underline, UnderlineKind::Dotted);
        assert_eq!(
            snap.cells[0][4].style.underline_color,
            Some(LINK_UNDERLINE_COLOR)
        );
        assert_eq!(snap.cells[0][3].style.underline, UnderlineKind::None);
    }

    #[test]
    fn implicit_url_joins_soft_wrapped_rows() {
        let mut term = make_term(10, 3);
        // 16 chars into a 10-col row: soft-wraps onto row 1.
        advance_bytes(&mut term, b"https://e.com/ab");
        let snap = take_snapshot(&term);
        assert_eq!(snap.links, vec!["https://e.com/ab".to_string()]);
        assert_eq!(snap.cells[0][9].link, Some(0));
        assert_eq!(snap.cells[1][5].link, Some(0));
        assert_eq!(snap.cells[1][6].link, None);
    }

    #[test]
    fn wrapped_url_reconstructs_full_uri_on_both_rows() {
        // A realistic long URL (query string) soft-wrapped by a narrow term:
        // every linked cell, on either row, must resolve to the WHOLE URL,
        // which is what a click hands to open_url.
        let mut term = make_term(38, 5);
        let full = "https://example.com/some/really/long/path/that/wraps?q=value&more=1";
        advance_bytes(&mut term, full.as_bytes());
        let snap = take_snapshot(&term);
        assert_eq!(snap.links, vec![full.to_string()]);
        let idx0 = snap.cells[0][5].link.expect("row 0 should be linked");
        let idx1 = snap.cells[1][5]
            .link
            .expect("row 1 (wrap) should be linked");
        assert_eq!(snap.links[idx0 as usize], full);
        assert_eq!(
            snap.links[idx1 as usize], full,
            "wrap continuation truncated"
        );
    }

    #[test]
    fn osc8_wins_over_implicit_detection() {
        let mut term = make_term(40, 2);
        // The visible text looks like a URL, but OSC 8 says it points
        // elsewhere — the explicit target must win.
        advance_bytes(
            &mut term,
            b"\x1b]8;;https://real.example/\x1b\\https://shown.example/\x1b]8;;\x1b\\",
        );
        let snap = take_snapshot(&term);
        assert_eq!(snap.links, vec!["https://real.example/".to_string()]);
        assert_eq!(snap.cells[0][0].link, Some(0));
    }

    #[test]
    fn detect_urls_trims_prose_punctuation_and_balances_parens() {
        let find = |s: &str| {
            let chars: Vec<char> = s.chars().collect();
            detect_urls(&chars)
                .into_iter()
                .map(|(_, _, uri)| uri)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            find("go to https://a.example/x."),
            vec!["https://a.example/x"]
        );
        assert_eq!(find("(https://a.example/x)"), vec!["https://a.example/x"]);
        assert_eq!(
            find("https://en.wikipedia.org/wiki/Rust_(language)"),
            vec!["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            find("HTTPS://UPPER.example/ and http://b.example/,"),
            vec!["HTTPS://UPPER.example/", "http://b.example/"]
        );
        // Bare scheme with nothing after it is not a link.
        assert_eq!(find("https:// nothing"), Vec::<String>::new());
        assert_eq!(find("no urls here"), Vec::<String>::new());
        // Quotes terminate.
        assert_eq!(
            find("\"https://a.example/x\" q"),
            vec!["https://a.example/x"]
        );
    }

    #[test]
    fn sgr_attributes_are_captured() {
        let mut term = make_term(20, 2);
        // bold, italic, dim, inverse, hidden, strikeout, underline, undercurl
        advance_bytes(
            &mut term,
            b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[2mD\x1b[0m\x1b[7mR\x1b[0m\x1b[8mH\x1b[0m\x1b[9mS\x1b[0m\x1b[4mU\x1b[0m\x1b[4:3mC\x1b[0m N",
        );
        let snap = take_snapshot(&term);
        let row = &snap.cells[0];
        assert!(row[0].style.bold);
        assert!(row[1].style.italic);
        assert!(row[2].style.dim);
        assert!(row[3].style.inverse);
        assert!(row[4].style.hidden);
        assert!(row[5].style.strikeout);
        assert_eq!(row[6].style.underline, UnderlineKind::Single);
        assert_eq!(row[7].style.underline, UnderlineKind::Undercurl);
        // Plain cell after resets carries no attributes.
        assert_eq!(row[9].style, CellStyle::default());
    }

    #[test]
    fn sgr_underline_variants_are_distinguished() {
        let mut term = make_term(10, 1);
        // double, dotted, dashed
        advance_bytes(&mut term, b"\x1b[4:2mD\x1b[0m\x1b[4:4mO\x1b[0m\x1b[4:5mA");
        let snap = take_snapshot(&term);
        let row = &snap.cells[0];
        assert_eq!(row[0].style.underline, UnderlineKind::Double);
        assert_eq!(row[1].style.underline, UnderlineKind::Dotted);
        assert_eq!(row[2].style.underline, UnderlineKind::Dashed);
    }

    #[test]
    fn sgr_underline_color_is_captured() {
        let mut term = make_term(10, 1);
        // SGR 58;2;r;g;b = direct-color underline, 59 = reset to fg-follow.
        advance_bytes(&mut term, b"\x1b[4;58;2;10;20;30mU\x1b[59mV");
        let snap = take_snapshot(&term);
        let row = &snap.cells[0];
        assert_eq!(
            row[0].style.underline_color,
            Some(ResolvedColor::Rgb(10, 20, 30))
        );
        assert_eq!(row[1].style.underline_color, None);
    }

    #[test]
    fn sgr_blink_is_captured_and_flagged() {
        let mut term = make_term(10, 1);
        advance_bytes(&mut term, b"\x1b[5mB\x1b[25m \x1b[6mR");
        let snap = take_snapshot(&term);
        assert!(snap.cells[0][0].style.blink);
        assert!(!snap.cells[0][1].style.blink);
        assert!(snap.cells[0][2].style.blink); // rapid maps to the same flag
        assert!(snap.has_blink);

        let plain = make_term(10, 1);
        assert!(!take_snapshot(&plain).has_blink);
    }

    #[test]
    fn paste_plain_normalizes_newlines_to_cr() {
        let term = make_term(80, 24);
        assert_eq!(
            paste_pty_bytes(*term.mode(), "a\r\nb\nc"),
            b"a\rb\rc".to_vec()
        );
    }

    #[test]
    fn paste_bracketed_wraps_and_strips_end_marker() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?2004h");
        // Newlines pass through untouched; an embedded end marker is stripped
        // so clipboard content cannot break out of the bracket.
        assert_eq!(
            paste_pty_bytes(*term.mode(), "a\nb\x1b[201~echo pwned\n"),
            b"\x1b[200~a\nbecho pwned\n\x1b[201~".to_vec()
        );
    }

    #[test]
    fn wheel_plain_shell_scrolls_local_history() {
        let term = make_term(80, 24);
        assert_eq!(
            wheel_pty_bytes(*term.mode(), 1, 0, 0, ReportMods::default()),
            None
        );
    }

    #[test]
    fn wheel_sgr_mouse_reporting_encodes_buttons_at_cell() {
        let mut term = make_term(80, 24);
        // ?1002h = mouse drag reporting, ?1006h = SGR encoding (btop's setup).
        advance_bytes(&mut term, b"\x1b[?1002h\x1b[?1006h");
        let up =
            wheel_pty_bytes(*term.mode(), 2, 5, 3, ReportMods::default()).expect("routed to pty");
        assert_eq!(up, b"\x1b[<64;6;4M\x1b[<64;6;4M");
        let down =
            wheel_pty_bytes(*term.mode(), -1, 0, 0, ReportMods::default()).expect("routed to pty");
        assert_eq!(down, b"\x1b[<65;1;1M");
    }

    #[test]
    fn wheel_x10_mouse_reporting_without_sgr() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h");
        let up =
            wheel_pty_bytes(*term.mode(), 1, 5, 3, ReportMods::default()).expect("routed to pty");
        assert_eq!(up, &[0x1b, b'[', b'M', 32 + 64, 32 + 6, 32 + 4]);
    }

    #[test]
    fn wheel_alternate_scroll_sends_arrows() {
        let mut term = make_term(80, 24);
        // ?1049h = alt screen, ?1007h = alternate scroll (less-style).
        advance_bytes(&mut term, b"\x1b[?1049h\x1b[?1007h");
        let up =
            wheel_pty_bytes(*term.mode(), 2, 0, 0, ReportMods::default()).expect("routed to pty");
        assert_eq!(up, b"\x1b[A\x1b[A");
        // DECCKM application cursor mode switches to SS3 arrows.
        advance_bytes(&mut term, b"\x1b[?1h");
        let down =
            wheel_pty_bytes(*term.mode(), -1, 0, 0, ReportMods::default()).expect("routed to pty");
        assert_eq!(down, b"\x1bOB");
    }

    #[test]
    fn wheel_alt_screen_defaults_to_arrows_and_1007l_disables() {
        let mut term = make_term(80, 24);
        // ALTERNATE_SCROLL is ON by default in alacritty, so entering the
        // alt screen alone already routes the wheel as arrow keys…
        advance_bytes(&mut term, b"\x1b[?1049h");
        assert_eq!(
            wheel_pty_bytes(*term.mode(), 1, 0, 0, ReportMods::default()),
            Some(b"\x1b[A".to_vec())
        );
        // …until the app opts out with ?1007l.
        advance_bytes(&mut term, b"\x1b[?1007l");
        assert_eq!(
            wheel_pty_bytes(*term.mode(), 1, 0, 0, ReportMods::default()),
            None
        );
    }

    #[test]
    fn mouse_plain_shell_stays_local() {
        let term = make_term(80, 24);
        let ev = MouseReport::Press(ReportButton::Left);
        assert_eq!(
            mouse_pty_bytes(*term.mode(), ev, ReportMods::default(), 0, 0),
            None
        );
    }

    #[test]
    fn mouse_sgr_press_release_encode_button_and_suffix() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1006h");
        let mods = ReportMods::default();
        let press = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Left),
            mods,
            5,
            3,
        );
        assert_eq!(press.unwrap(), b"\x1b[<0;6;4M");
        // SGR releases keep the button identity, with a lowercase suffix.
        let release = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Release(ReportButton::Left),
            mods,
            5,
            3,
        );
        assert_eq!(release.unwrap(), b"\x1b[<0;6;4m");
        let middle = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Middle),
            mods,
            0,
            0,
        );
        assert_eq!(middle.unwrap(), b"\x1b[<1;1;1M");
    }

    #[test]
    fn mouse_utf8_1005_widens_coordinates_past_x10() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1005h");
        let mods = ReportMods::default();
        // col 300 exceeds the X10 cap (223): plain X10 would drop it, ?1005
        // encodes 32+301 = 333 = U+014D as two UTF-8 bytes.
        let press = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Left),
            mods,
            300,
            0,
        );
        assert_eq!(press.unwrap(), b"\x1b[M\x20\xc5\x8d\x21");
        // Release still collapses to button 3 outside SGR.
        let release = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Release(ReportButton::Left),
            mods,
            0,
            0,
        );
        assert_eq!(release.unwrap(), b"\x1b[M\x23\x21\x21");
        // Beyond the ?1005 limit (2015) the event is dropped, like X10's 223.
        assert_eq!(
            mouse_pty_bytes(
                *term.mode(),
                MouseReport::Press(ReportButton::Left),
                mods,
                2100,
                0,
            ),
            None
        );
        // SGR still wins when both are negotiated.
        advance_bytes(&mut term, b"\x1b[?1006h");
        let sgr = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Left),
            mods,
            300,
            0,
        );
        assert_eq!(sgr.unwrap(), b"\x1b[<0;301;1M");
    }

    #[test]
    fn wheel_utf8_1005_encodes_ticks() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1005h");
        let bytes = wheel_pty_bytes(*term.mode(), 1, 300, 0, ReportMods::default());
        // 32+64 = 96 = '`', col 333 → U+014D, row 33 = '!'.
        assert_eq!(bytes.unwrap(), b"\x1b[M\x60\xc5\x8d\x21");
    }

    #[test]
    fn hwheel_buttons_66_67_and_ownership() {
        let mut term = make_term(80, 24);
        // Plain shell: no mouse reporting → not owned (and nothing to do
        // locally either; horizontal just dies).
        assert_eq!(
            hwheel_pty_bytes(*term.mode(), 1, 0, 0, ReportMods::default()),
            None
        );
        // Alternate scroll alone still doesn't own horizontal (no arrow-key
        // mapping is defined for it).
        advance_bytes(&mut term, b"\x1b[?1049h");
        assert_eq!(
            hwheel_pty_bytes(*term.mode(), 1, 0, 0, ReportMods::default()),
            None
        );
        // With mouse reporting: 66 = left (positive), 67 = right, one report
        // per tick.
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1006h");
        let left = hwheel_pty_bytes(*term.mode(), 1, 5, 3, ReportMods::default());
        assert_eq!(left.unwrap(), b"\x1b[<66;6;4M");
        let right = hwheel_pty_bytes(*term.mode(), -2, 0, 0, ReportMods::default());
        assert_eq!(right.unwrap(), b"\x1b[<67;1;1M\x1b[<67;1;1M");
    }

    #[test]
    fn cursor_shape_tracks_decscusr_and_visibility() {
        let mut term = make_term(80, 24);
        assert_eq!(take_snapshot(&term).cursor_shape, CursorShapeKind::Block);
        // DECSCUSR: 5 = blinking beam, 3 = blinking underline (blink phase is
        // not modelled; only the shape is mirrored).
        advance_bytes(&mut term, b"\x1b[5 q");
        assert_eq!(take_snapshot(&term).cursor_shape, CursorShapeKind::Beam);
        advance_bytes(&mut term, b"\x1b[3 q");
        assert_eq!(
            take_snapshot(&term).cursor_shape,
            CursorShapeKind::Underline
        );
        // DECTCEM hide/show arrives through RenderableCursor as Hidden.
        advance_bytes(&mut term, b"\x1b[?25l");
        assert_eq!(take_snapshot(&term).cursor_shape, CursorShapeKind::Hidden);
        advance_bytes(&mut term, b"\x1b[?25h\x1b[2 q");
        assert_eq!(take_snapshot(&term).cursor_shape, CursorShapeKind::Block);
    }

    #[test]
    fn cursor_blink_tracks_decscusr_and_decset_12() {
        let mut term = make_term(80, 24);
        assert!(!take_snapshot(&term).cursor_blink);
        // DECSCUSR 1 = blinking block, 2 = steady block.
        advance_bytes(&mut term, b"\x1b[1 q");
        assert!(take_snapshot(&term).cursor_blink);
        advance_bytes(&mut term, b"\x1b[2 q");
        assert!(!take_snapshot(&term).cursor_blink);
        // DECSET ?12 folds into the same cursor style.
        advance_bytes(&mut term, b"\x1b[?12h");
        assert!(take_snapshot(&term).cursor_blink);
        // Hidden gates the flag: no refresh-timer churn for `?25l` apps.
        advance_bytes(&mut term, b"\x1b[?25l");
        assert!(!take_snapshot(&term).cursor_blink);
    }

    #[test]
    fn selection_tracks_scrollback_and_output() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};
        let mut term = make_term(10, 4);
        // 8 numbered lines through a 4-row screen: history 1-4, screen 5-8.
        advance_bytes(&mut term, b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        // Anchor on the '5' cell (viewport row 0): a fresh anchor is empty
        // until the head moves (so a plain click never flashes a highlight)…
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        assert_eq!(take_snapshot(&term).selection, None);
        // …dragging to the cell's right side selects it.
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(0), Column(0)), Side::Right);
        assert_eq!(take_snapshot(&term).selection, Some(((0, 0), (0, 0))));
        assert_eq!(term.selection_to_string().as_deref(), Some("5"));
        // Scroll one line into history: the highlight must follow the '5'
        // line down to viewport row 1 — this was the visible bug (the old
        // screen-row state stayed at row 0, off the text).
        term.scroll_display(Scroll::Delta(1));
        assert_eq!(take_snapshot(&term).selection, Some(((1, 0), (1, 0))));
        // Back to the bottom, then new output rotates the grid: the selection
        // rides the content out of the viewport but still copies correctly.
        term.scroll_display(Scroll::Bottom);
        advance_bytes(&mut term, b"\r\n9");
        assert_eq!(take_snapshot(&term).selection, None);
        assert_eq!(term.selection_to_string().as_deref(), Some("5"));
    }

    #[test]
    fn color_query_and_winops_events_reach_the_channel() {
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let listener = ClipboardListener {
            tx,
            title: Arc::default(),
        };
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), listener);
        let mut parser = Processor::<StdSyncHandler>::new();
        // OSC 11 background query — vim's theme probe.
        for &b in b"\x1b]11;?\x1b\\".iter() {
            parser.advance(&mut term, b);
        }
        let Ok(ClipboardEvent::ColorQuery(idx, formatter)) = rx.try_recv() else {
            panic!("expected ColorQuery");
        };
        {
            use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
            assert_eq!(idx, NamedColor::Background as usize);
            let reply = formatter(Rgb {
                r: 0x1a,
                g: 0x1a,
                b: 0x1a,
            });
            assert!(reply.starts_with("\x1b]11;rgb:1a1a/1a1a/1a1a"), "{reply:?}");
        }
        // CSI 14 t — text-area size in pixels.
        for &b in b"\x1b[14t".iter() {
            parser.advance(&mut term, b);
        }
        let Ok(ClipboardEvent::TextAreaSize(formatter)) = rx.try_recv() else {
            panic!("expected TextAreaSize");
        };
        let reply = formatter(alacritty_terminal::event::WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        assert_eq!(reply, "\x1b[4;384;640t");
    }

    #[test]
    fn query_color_resolution_order() {
        use alacritty_terminal::term::color::Colors;
        use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
        let mut colors = Colors::default();
        // Unset specials fall back to the renderer defaults…
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Background as usize),
            Some(Rgb {
                r: 0x1A,
                g: 0x1A,
                b: 0x1A
            })
        );
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Foreground as usize),
            Some(Rgb {
                r: 0xE8,
                g: 0xDC,
                b: 0xC8
            })
        );
        // …indexed colors to the standard palette (196 = pure red in the
        // 6×6×6 cube)…
        assert_eq!(
            query_color_rgb(&colors, 196),
            Some(Rgb { r: 255, g: 0, b: 0 })
        );
        // …and an explicitly set entry wins over every fallback.
        colors[NamedColor::Background as usize] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Background as usize),
            Some(Rgb { r: 1, g: 2, b: 3 })
        );
    }

    #[test]
    fn mouse_x10_release_is_button_three() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h");
        let mods = ReportMods::default();
        let press = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Left),
            mods,
            5,
            3,
        );
        assert_eq!(press.unwrap(), &[0x1b, b'[', b'M', 32, 32 + 6, 32 + 4]);
        let release = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Release(ReportButton::Left),
            mods,
            5,
            3,
        );
        assert_eq!(
            release.unwrap(),
            &[0x1b, b'[', b'M', 32 + 3, 32 + 6, 32 + 4]
        );
    }

    #[test]
    fn mouse_drag_motion_needs_1002_hover_needs_1003() {
        let mut term = make_term(80, 24);
        let mods = ReportMods::default();
        let drag = MouseReport::Motion(Some(ReportButton::Left));
        let hover = MouseReport::Motion(None);
        // ?1000: clicks only — no motion of either kind.
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(mouse_pty_bytes(*term.mode(), drag, mods, 0, 0), None);
        assert_eq!(mouse_pty_bytes(*term.mode(), hover, mods, 0, 0), None);
        // ?1002: drag motion carries the button + 32.
        advance_bytes(&mut term, b"\x1b[?1002h");
        assert_eq!(
            mouse_pty_bytes(*term.mode(), drag, mods, 2, 1).unwrap(),
            b"\x1b[<32;3;2M"
        );
        assert_eq!(mouse_pty_bytes(*term.mode(), hover, mods, 0, 0), None);
        // ?1003: hover motion reports the released id 3 + 32 = 35.
        advance_bytes(&mut term, b"\x1b[?1003h");
        assert_eq!(
            mouse_pty_bytes(*term.mode(), hover, mods, 2, 1).unwrap(),
            b"\x1b[<35;3;2M"
        );
    }

    #[test]
    fn wheel_carries_modifier_bits() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1006h");
        let mods = ReportMods {
            alt: false,
            ctrl: true,
        };
        // 64 (wheel up) + 16 (ctrl) = 80, like Ghostty/xterm.
        assert_eq!(
            wheel_pty_bytes(*term.mode(), 1, 0, 0, mods).unwrap(),
            b"\x1b[<80;1;1M"
        );
    }

    #[test]
    fn x10_coordinates_past_223_are_dropped_not_clamped() {
        let mut term = make_term(300, 24);
        advance_bytes(&mut term, b"\x1b[?1000h");
        let mods = ReportMods::default();
        // Ghostty/xterm emit nothing for unrepresentable coordinates.
        assert_eq!(
            mouse_pty_bytes(
                *term.mode(),
                MouseReport::Press(ReportButton::Left),
                mods,
                250,
                3
            ),
            None
        );
        // Wheel: the event is consumed (app owns the mouse) but no bytes go out.
        assert_eq!(wheel_pty_bytes(*term.mode(), 1, 250, 3, mods).unwrap(), b"");
    }

    #[test]
    fn mouse_modifier_bits_add_to_button_code() {
        let mut term = make_term(80, 24);
        advance_bytes(&mut term, b"\x1b[?1000h\x1b[?1006h");
        let mods = ReportMods {
            alt: true,
            ctrl: true,
        };
        let press = mouse_pty_bytes(
            *term.mode(),
            MouseReport::Press(ReportButton::Left),
            mods,
            0,
            0,
        );
        // 0 (left) + 8 (alt) + 16 (ctrl) = 24.
        assert_eq!(press.unwrap(), b"\x1b[<24;1;1M");
    }

    #[test]
    fn empty_terminal_snapshot_dimensions_and_blank_cells() {
        let term = make_term(10, 5);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.rows, 5);
        assert!(snap.cells.iter().flatten().all(|c| c.c == ' '));
    }

    #[test]
    fn written_text_appears_in_snapshot() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"H");
        let snap = take_snapshot(&term);
        assert!(snap.cells.iter().flatten().any(|c| c.c == 'H'));
    }

    #[test]
    fn ansi_rgb_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[38;2;255;0;0mR");
        let snap = take_snapshot(&term);
        assert!(
            snap.cells
                .iter()
                .flatten()
                .any(|c| c.c == 'R' && c.fg == ResolvedColor::Rgb(255, 0, 0))
        );
    }

    #[test]
    fn ansi_named_red_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[31mR\x1b[0m");
        let snap = take_snapshot(&term);
        let r_cell = snap
            .cells
            .iter()
            .flatten()
            .find(|c| c.c == 'R')
            .expect("R cell");
        assert!(
            matches!(r_cell.fg, ResolvedColor::Rgb(r, _, _) if r > 0),
            "named red should resolve to non-zero RGB, got {:?}",
            r_cell.fg
        );
    }

    #[test]
    fn ansi_indexed_foreground_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[38;5;46mG");
        let snap = take_snapshot(&term);
        assert!(
            snap.cells
                .iter()
                .flatten()
                .any(|c| c.c == 'G' && matches!(c.fg, ResolvedColor::Rgb(_, _, _)))
        );
    }

    #[test]
    fn ansi_background_is_captured() {
        let mut term = make_term(10, 5);
        advance_bytes(&mut term, b"\x1b[42mB\x1b[0m");
        let snap = take_snapshot(&term);
        let b_cell = snap
            .cells
            .iter()
            .flatten()
            .find(|c| c.c == 'B')
            .expect("B cell");
        assert!(
            matches!(b_cell.bg, ResolvedColor::Rgb(_, _, _)),
            "green background should resolve to RGB, got {:?}",
            b_cell.bg
        );
    }

    #[test]
    fn initial_cursor_is_at_origin() {
        let term = make_term(10, 5);
        let snap = take_snapshot(&term);
        assert_eq!(snap.cursor, (0, 0));
    }

    // ── field bug hunt (2026-07-10): "alt screen + 右クリ/選択で内容消失" ──
    // Pin every engine-side defense on the selection × alt-screen ×
    // scrollback axes, so if the field bug lives in this layer one of these
    // trips, and if they all hold the engine is exonerated headlessly.

    fn snap_row(snap: &GridSnapshot, row: usize) -> String {
        snap.cells[row].iter().map(|c| c.c).collect()
    }

    fn simple_selection(term: &mut Term<VoidListener>, from: (i32, usize), to: (i32, usize)) {
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};
        let mut sel = Selection::new(
            SelectionType::Simple,
            Point::new(Line(from.0), Column(from.1)),
            Side::Left,
        );
        sel.update(Point::new(Line(to.0), Column(to.1)), Side::Right);
        term.selection = Some(sel);
    }

    #[test]
    fn sixel_placeholder_injection_respects_start_column() {
        // The yazi scenario: a preview drawn mid-screen via CUP. Every
        // placeholder row must stay at the image's start column — the old
        // CR/LF row movement dropped rows 2+ to column 0, shredding the
        // file list into a staircase (field report 2026-07-10).
        let mut term = make_term(20, 8);
        for i in 0..8 {
            advance_bytes(&mut term, format!("\x1b[{};1HF{i}", i + 1).as_bytes());
        }
        advance_bytes(&mut term, b"\x1b[2;11H");
        let bytes = crate::sixel::placeholder_bytes(crate::sixel::SIXEL_ID_BASE + 1, 6, 3, 10);
        advance_bytes(&mut term, &bytes);
        let snap = take_snapshot(&term);
        for r in 1..4 {
            assert!(
                snap.cells[r][10].image.is_some(),
                "row {r} col 10 should be an image cell"
            );
            assert!(snap.cells[r][15].image.is_some());
            assert!(
                snap.cells[r][9].image.is_none(),
                "row {r} bled left of the start column"
            );
            assert_eq!(snap.cells[r][0].c, 'F', "file list shredded at row {r}");
        }
    }

    #[test]
    fn sixel_placeholder_at_bottom_scrolls_like_text() {
        // IND at the bottom margin scrolls (sixel scrolling-mode semantics);
        // rows keep their column instead of wrapping to 0.
        let mut term = make_term(20, 4);
        advance_bytes(&mut term, b"\x1b[4;5H");
        let bytes = crate::sixel::placeholder_bytes(crate::sixel::SIXEL_ID_BASE + 2, 4, 2, 4);
        advance_bytes(&mut term, &bytes);
        let snap = take_snapshot(&term);
        assert!(snap.cells[1][4].image.is_some());
        assert!(snap.cells[2][4].image.is_some());
        assert!(snap.cells[1][3].image.is_none());
    }

    #[test]
    fn selection_left_in_scrollback_never_highlights_whole_screen() {
        use alacritty_terminal::grid::Scroll;
        let mut term = make_term(20, 4);
        for i in 0..12 {
            advance_bytes(&mut term, format!("line{i}\r\n").as_bytes());
        }
        // Scroll back and select what is on screen (history lines) — the
        // shift+drag flow, via the same viewport mapping the app uses.
        term.scroll_display(Scroll::Delta(6));
        let a = viewport_point(&term, 0, 0);
        let b = viewport_point(&term, 1, 5);
        simple_selection(&mut term, (a.line.0, a.column.0), (b.line.0, b.column.0));
        // Jump back to the live bottom: the selected lines are now above the
        // viewport. The snapshot must clamp (start pinned to (0,0)) or drop
        // the selection — a sign-wrapped row that paints the entire screen
        // as selected is exactly the "content vanished" failure.
        term.scroll_display(Scroll::Bottom);
        let snap = take_snapshot(&term);
        if let Some((start, end)) = snap.selection {
            assert!(end.0 < snap.rows, "selection end wrapped: {end:?}");
            assert!(start.0 <= end.0, "inverted selection: {start:?}..{end:?}");
        }
        assert_eq!(&snap_row(&snap, 0)[..5], "line9");
    }

    #[test]
    fn alt_swap_drops_selection_and_shows_alt_content() {
        let mut term = make_term(20, 4);
        advance_bytes(&mut term, b"primary text\r\n");
        simple_selection(&mut term, (0, 0), (0, 6));
        assert!(take_snapshot(&term).selection.is_some());
        // Entering the alt screen swaps grids; a selection anchored in the
        // primary grid must not survive into alt coordinates.
        advance_bytes(&mut term, b"\x1b[?1049h");
        assert!(term.selection.is_none(), "swap_alt must clear selection");
        // The alt cursor inherits the primary position — home it first so the
        // content lands on row 0.
        advance_bytes(&mut term, b"\x1b[HALTVIEW");
        let snap = take_snapshot(&term);
        assert!(snap.selection.is_none());
        assert_eq!(&snap_row(&snap, 0)[..7], "ALTVIEW");
        // And leaving restores the primary content untouched.
        advance_bytes(&mut term, b"\x1b[?1049l");
        let snap = take_snapshot(&term);
        assert_eq!(&snap_row(&snap, 0)[..12], "primary text");
    }

    #[test]
    fn alt_screen_redraw_under_live_selection_never_blanks() {
        let mut term = make_term(20, 4);
        advance_bytes(&mut term, b"\x1b[?1049h");
        advance_bytes(&mut term, b"SCREEN-A");
        // Local selection over alt-screen text (the shift+drag flow).
        simple_selection(&mut term, (0, 0), (0, 7));
        let snap = take_snapshot(&term);
        assert_eq!(&snap_row(&snap, 0)[..8], "SCREEN-A");
        assert!(snap.selection.is_some());
        // A full TUI repaint (claude/btop style: clear, home, redraw) with
        // the selection still held: content must show the new frame — never
        // a blank grid, never a screen-wide highlight.
        advance_bytes(&mut term, b"\x1b[2J\x1b[HSCREEN-B");
        let snap = take_snapshot(&term);
        assert_eq!(&snap_row(&snap, 0)[..8], "SCREEN-B");
        if let Some((start, end)) = snap.selection {
            assert!(end.0 < snap.rows && start.0 <= end.0);
        }
        // Selection ops arriving mid-frame (the right-click/copy path calls
        // these on the UI thread) must stay panic-free on the alt grid.
        let _ = term.selection_to_string();
        term.selection = None;
        let snap = take_snapshot(&term);
        assert_eq!(&snap_row(&snap, 0)[..8], "SCREEN-B");
    }

    #[test]
    fn selection_ops_after_alt_resize_stay_in_bounds() {
        use alacritty_terminal::index::{Column, Line, Point, Side};
        let mut term = make_term(20, 6);
        advance_bytes(&mut term, b"\x1b[?1049h");
        advance_bytes(&mut term, b"WIDE-ALT-CONTENT");
        // Selection anchored near the bottom, then the pane shrinks (the
        // context-menu/resize suspicion): to_range must clamp, snapshot must
        // stay renderable.
        simple_selection(&mut term, (5, 0), (5, 10));
        term.resize(TermSize::new(10, 3));
        let snap = take_snapshot(&term);
        assert_eq!(snap.rows, 3);
        if let Some((start, end)) = snap.selection {
            assert!(end.0 < 3, "selection outlived the shrink: {end:?}");
            assert!(start.0 <= end.0);
        }
        if let Some(sel) = term.selection.as_mut() {
            sel.update(Point::new(Line(2), Column(9)), Side::Right);
        }
        let _ = term.selection_to_string();
    }

    /// The file logger drops exactly gpui's post-close callback noise and
    /// nothing else — a different message or another crate's identical
    /// message must still land in the log.
    #[test]
    fn log_noise_filter_is_exact() {
        assert!(is_benign_log_noise("gpui", "window not found"));
        assert!(is_benign_log_noise("gpui::window", "window not found"));
        assert!(!is_benign_log_noise("gpui", "window not found: extra"));
        assert!(!is_benign_log_noise("gpui", "root view not found"));
        assert!(!is_benign_log_noise("rikka_terminal", "window not found"));
    }
}
