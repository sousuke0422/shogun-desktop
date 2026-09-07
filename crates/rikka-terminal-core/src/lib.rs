pub mod emoji_shape;
pub mod frametime;
pub mod graphics;
pub mod ime;
pub mod keys;
pub mod kitty_graphics;
pub mod notify;
pub mod pane;
pub mod progress;
pub mod pty_handoff;
pub mod pty_session;
pub mod renderer;
pub mod search_bar;
pub mod selection;
pub mod sixel;
pub mod taskbar;
pub mod theme;
pub mod winops;
pub mod xtversion;

use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering},
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
    /// Engine-generated protocol replies (DA1/DA2, DSR, DECRQM, …), handed
    /// to the parse loop so they leave in STREAM ORDER with the replies the
    /// loop produces itself (XTVERSION, kitty graphics, XTWINOPS).
    pub replies: Arc<PendingReplies>,
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
            Event::PtyWrite(text) => self.replies.push(text),
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

/// Protocol replies the engine emits while a chunk is being parsed, parked
/// until the parse loop reaches the same point in the byte stream.
///
/// Why this exists (measured 2026-09-07, `docs/compat/README.md`, graphics
/// section addendum): the engine's DA1 reply used to go out through the
/// clipboard thread — an immediate write — while XTVERSION, kitty `a=q`
/// and XTWINOPS replies were batched in the parse loop and written after
/// the chunk. A client sending `XTVERSION, a=q, DA1` in one write therefore
/// received `DA1, XTVERSION, OK`: the DA1 fence that kitty-style detection
/// waits on arrived BEFORE the answer it was fencing, and every such
/// client concluded "no kitty graphics". That reordering was misread for a
/// month as the ConPTY host answering DA1 locally; the host relays the
/// terminal's own reply, and the inversion was ours. The parse loop drains
/// this buffer after every byte it feeds the engine, so a reply lands in
/// the response queue exactly where its query sat in the input.
#[derive(Default)]
pub struct PendingReplies {
    pending: AtomicBool,
    buf: parking_lot::Mutex<Vec<String>>,
}

impl PendingReplies {
    pub fn push(&self, text: String) {
        self.buf.lock().push(text);
        self.pending.store(true, Ordering::Release);
    }

    /// Move parked replies onto `out` in emission order. The atomic check
    /// keeps the per-byte cost to one relaxed load while nothing is parked.
    #[inline]
    pub fn drain_into(&self, out: &mut Vec<Vec<u8>>) {
        if !self.pending.load(Ordering::Acquire) {
            return;
        }
        let mut buf = self.buf.lock();
        self.pending.store(false, Ordering::Release);
        out.extend(buf.drain(..).map(String::into_bytes));
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

/// What a selection selects per cell under the cursor: a plain cell run, the
/// whole word (double-click; alacritty's semantic boundaries), or whole
/// lines (triple-click).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}

impl SelectionKind {
    fn to_alacritty(self) -> alacritty_terminal::selection::SelectionType {
        use alacritty_terminal::selection::SelectionType as T;
        match self {
            SelectionKind::Simple => T::Simple,
            SelectionKind::Word => T::Semantic,
            SelectionKind::Line => T::Lines,
        }
    }
}

/// A live screen-anchored selection: its kind plus viewport `(anchor, head)`
/// cells, each `(row, col, right_side)`. See
/// [`TerminalSession::selection_drag`].
pub type ScreenSel = Option<(SelectionKind, (usize, usize, bool), (usize, usize, bool))>;

/// A live scrollback search: the compiled query, every match (capped) and
/// the current one, all owned by the session so stepping, the match counter
/// and highlighting survive scrolling and output. See
/// [`TerminalSession::search_set`].
pub struct SearchLive {
    regex: alacritty_terminal::term::search::RegexSearch,
    /// The query as typed (for the host's search bar display).
    pub query: String,
    current: Option<alacritty_terminal::term::search::Match>,
    /// Every match in the buffer (history top → viewport bottom), collected
    /// on set/step and capped at [`SEARCH_MATCH_CAP`] — the "3/12" counter
    /// and the pale all-match highlight. Stale while output streams past an
    /// open bar; the next step re-collects.
    matches: Vec<alacritty_terminal::term::search::Match>,
    /// The collection hit the cap ("N/999+").
    truncated: bool,
    /// 1-based position of `current` in `matches`; 0 = none / unknown.
    index: usize,
}

/// Cap on collected search matches (counter shows "999+" beyond it).
const SEARCH_MATCH_CAP: usize = 1000;

/// Search-bar numbers: current match position (1-based; 0 = none), total
/// matches, and whether the total got capped.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct SearchStatus {
    pub index: usize,
    pub total: usize,
    pub truncated: bool,
}

/// Everything the renderer paints for a live search, in grid coordinates
/// (`(line, col)` pairs; the renderer maps them into the viewport with the
/// snapshot's display offset): the current match plus every other
/// viewport-visible match for the pale highlight.
#[derive(Clone, Default)]
pub struct SearchRender {
    pub current: Option<((i32, usize), (i32, usize))>,
    pub others: Vec<((i32, usize), (i32, usize))>,
}

/// Collect every match in the whole buffer, top of history to the viewport
/// bottom, capped at [`SEARCH_MATCH_CAP`].
fn collect_search_matches(
    term: &Term<ClipboardListener>,
    regex: &mut alacritty_terminal::term::search::RegexSearch,
) -> (Vec<alacritty_terminal::term::search::Match>, bool) {
    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Direction, Point};
    use alacritty_terminal::term::search::RegexIter;
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let mut matches = Vec::new();
    for m in RegexIter::new(start, end, Direction::Right, term, regex) {
        if matches.len() >= SEARCH_MATCH_CAP {
            return (matches, true);
        }
        matches.push(m);
    }
    (matches, false)
}

pub struct TerminalSession {
    #[allow(dead_code)]
    pub term: Arc<FairMutex<Term<ClipboardListener>>>,
    /// The screen-anchored selection, while one is live at the tail: the
    /// parse thread re-pins the grid selection from these viewport cells
    /// after every output application, so streaming redraw loops (Codex's
    /// timer-driven "Working…", Claude Code re-rendering on mouse reports)
    /// cannot walk a made selection off the screen. Dropped — freezing the
    /// selection to the grid — on scroll-back.
    pub screen_sel: Arc<FairMutex<ScreenSel>>,
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
    /// Shell-integration prompt marks (OSC 133;A), as absolute buffer lines
    /// (`history_size + screen row` at mark time — exact until scrollback
    /// caps, after which the oldest marks fall off with their rows). Written
    /// by the PTY reader thread; [`Self::jump_prompt`] scrolls between them.
    pub prompt_marks: Arc<FairMutex<std::collections::VecDeque<u64>>>,
    /// Working directory last reported by the shell (OSC 9;9 / OSC 7) —
    /// new tabs inherit it. See [`Self::current_cwd`].
    pub cwd: Arc<FairMutex<Option<String>>>,
    /// Live scrollback search (compiled query + current match), when the
    /// host has a search bar open on this session. See [`Self::search_set`].
    pub search: Arc<FairMutex<Option<SearchLive>>>,
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
    #[cfg(windows)]
    pub(crate) transfer_pause: Arc<TransferPause>,
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

#[cfg(windows)]
pub(crate) struct TransferPause {
    state: AtomicU8,
    drained: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}

#[cfg(windows)]
impl TransferPause {
    const RUNNING: u8 = 0;
    const PAUSED: u8 = 1;
    const STOPPED: u8 = 2;

    pub(crate) fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::RUNNING),
            drained: std::sync::Mutex::new(false),
            wake: std::sync::Condvar::new(),
        }
    }

    pub(crate) fn reader_pause_point(&self, signal: impl FnOnce() -> bool) -> bool {
        if self.state.load(Ordering::Acquire) != Self::PAUSED {
            return self.state.load(Ordering::Acquire) == Self::STOPPED;
        }
        if !signal() {
            return true;
        }
        let mut drained = self.drained.lock().unwrap_or_else(|e| e.into_inner());
        while self.state.load(Ordering::Acquire) == Self::PAUSED {
            drained = self.wake.wait(drained).unwrap_or_else(|e| e.into_inner());
        }
        self.state.load(Ordering::Acquire) == Self::STOPPED
    }

    pub(crate) fn pause_requested(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::PAUSED
    }

    pub(crate) fn mark_drained(&self) {
        *self.drained.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }
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

    /// Pause this session for a cross-process tab move while retaining a
    /// resumable source reader. The parser drains everything consumed before
    /// acknowledging the pause, so replay serialization sees a stable Term.
    #[cfg(windows)]
    pub fn pause_for_transfer(&self) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
        self.pty_sealed.store(true, Ordering::Release);
        *self
            .transfer_pause
            .drained
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = false;
        self.transfer_pause
            .state
            .store(TransferPause::PAUSED, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if *self
                .transfer_pause
                .drained
                .lock()
                .unwrap_or_else(|e| e.into_inner())
            {
                return Ok(());
            }
            if let Some(handle) = self.reader_thread.lock().as_ref() {
                #[link(name = "kernel32")]
                unsafe extern "system" {
                    fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
                }
                unsafe {
                    CancelSynchronousIo(handle.as_raw_handle());
                }
            } else {
                anyhow::bail!("PTY reader is not running");
            }
            if std::time::Instant::now() >= deadline {
                self.resume_after_failed_transfer();
                anyhow::bail!("PTY reader did not pause within 5s");
            }
            let guard = self
                .transfer_pause
                .drained
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _ = self
                .transfer_pause
                .wake
                .wait_timeout(guard, std::time::Duration::from_millis(2));
        }
    }

    #[cfg(windows)]
    pub fn resume_after_failed_transfer(&self) {
        self.transfer_pause
            .state
            .store(TransferPause::RUNNING, Ordering::Release);
        self.pty_sealed.store(false, Ordering::Release);
        self.transfer_pause.wake.notify_all();
    }

    /// Commit a prepared transfer by permanently stopping the source reader.
    #[cfg(windows)]
    pub fn commit_transfer(&self) -> anyhow::Result<()> {
        self.transfer_pause
            .state
            .store(TransferPause::STOPPED, Ordering::Release);
        self.transfer_pause.wake.notify_all();
        self.finish_transfer_threads()
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
        self.pause_for_transfer()?;
        self.commit_transfer()
    }

    #[cfg(windows)]
    fn finish_transfer_threads(&self) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
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
    /// the pointer sat in the right half of the cell, `kind` = plain drag /
    /// double-click word / triple-click line. The selection lives in
    /// alacritty's `Selection` (grid coordinates), so it stays glued to the
    /// text through scrollback scrolling and output-driven rotation — the old
    /// app-side screen-row state slid off the content on any scroll.
    pub fn selection_begin(&self, row: usize, col: usize, right_side: bool, kind: SelectionKind) {
        let mut term = self.term.lock();
        let point = viewport_point(&term, row, col);
        term.selection = Some(alacritty_terminal::selection::Selection::new(
            kind.to_alacritty(),
            point,
            side_of(right_side),
        ));
        // Arm the screen anchor only at the live tail — a selection begun
        // in scrollback is grid-glued from the start (the view is still).
        let cell = (row, col, right_side);
        *self.screen_sel.lock() = (term.grid().display_offset() == 0).then_some((kind, cell, cell));
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

    /// Drag update, screen-anchored while the view rides the live tail.
    ///
    /// With `display_offset == 0`, a streaming application can rotate the
    /// grid between updates — Ink-style status redraws (Claude Code's
    /// shimmer, Codex's "Working…") with a wrapped line miscount their
    /// cursor-up and scroll on every repaint — and a grid-glued anchor then
    /// slides off what the pointer actually covered. So the WHOLE selection
    /// is rebuilt from the current viewport cells each update: what you
    /// point at is what you select. Scrolled back (offset > 0) the view is
    /// still, and the plain grid-glued head update keeps working — including
    /// wheel-while-dragging, where the anchor MUST stay with the text.
    pub fn selection_drag(
        &self,
        anchor: (usize, usize, bool),
        head: (usize, usize, bool),
        kind: SelectionKind,
    ) {
        let mut term = self.term.lock();
        apply_selection_drag(&mut term, anchor, head, kind);
        // Keep the screen anchor in sync so the parse thread re-pins the
        // finished selection too (a completed highlight must not walk off
        // the screen while an app streams).
        *self.screen_sel.lock() =
            (term.grid().display_offset() == 0).then_some((kind, anchor, head));
        self.refresh_snapshot(&term);
    }

    /// Drop any selection in this pane.
    pub fn selection_clear(&self) {
        let mut term = self.term.lock();
        term.selection = None;
        *self.screen_sel.lock() = None;
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

    /// The working directory the shell last reported (OSC 9;9 / OSC 7), for
    /// cwd inheritance by new tabs.
    pub fn current_cwd(&self) -> Option<String> {
        self.cwd.lock().clone()
    }

    /// Compile (or clear, on an empty query) the live scrollback search.
    /// Smart-case is alacritty's: an all-lowercase query matches
    /// case-insensitively. Returns `false` when the pattern does not
    /// compile — the previous state is kept, so a half-typed regex never
    /// drops the current match.
    pub fn search_set(&self, query: &str) -> bool {
        if query.is_empty() {
            let term = self.term.lock();
            *self.search.lock() = None;
            drop(term);
            self.poke_redraw();
            return true;
        }
        match alacritty_terminal::term::search::RegexSearch::new(query) {
            Ok(mut regex) => {
                // Lock order everywhere: term before search.
                let term = self.term.lock();
                let (matches, truncated) = collect_search_matches(&term, &mut regex);
                *self.search.lock() = Some(SearchLive {
                    regex,
                    query: query.to_string(),
                    current: None,
                    matches,
                    truncated,
                    index: 0,
                });
                drop(term);
                self.poke_redraw();
                true
            }
            Err(_) => false,
        }
    }

    /// Step to the next (`dir >= 0`) or previous match and scroll it into
    /// view (upper third). The first step after [`Self::search_set`] starts
    /// from the viewport top. alacritty's search wraps around the buffer.
    /// Returns whether a match is current afterwards.
    pub fn search_step(&self, dir: i32) -> bool {
        use alacritty_terminal::grid::Dimensions as _;
        use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
        let delta;
        let found;
        {
            let term = self.term.lock();
            let mut slot = self.search.lock();
            let Some(live) = slot.as_mut() else {
                return false;
            };
            let off = term.grid().display_offset() as i32;
            let rows = term.screen_lines() as i32;
            let origin = match (&live.current, dir >= 0) {
                (Some(m), true) => m.end().add(&*term, Boundary::None, 1),
                (Some(m), false) => m.start().sub(&*term, Boundary::None, 1),
                (None, true) => Point::new(Line(-off), Column(0)),
                (None, false) => Point::new(
                    Line(-off + rows - 1),
                    Column(term.columns().saturating_sub(1)),
                ),
            };
            let dirn = if dir >= 0 {
                Direction::Right
            } else {
                Direction::Left
            };
            let m = term.search_next(&mut live.regex, origin, dirn, Side::Left, None);
            found = m.is_some();
            delta = match &m {
                Some(m) => {
                    let line = m.start().line.0;
                    let viewport_row = line + off;
                    if (0..rows).contains(&viewport_row) {
                        0
                    } else {
                        let h = term.grid().history_size() as i32;
                        ((rows / 3) - line).clamp(0, h) - off
                    }
                }
                None => 0,
            };
            // Keep the counter honest: locate the match in the collected
            // list, re-collecting once when it isn't there (the grid moved
            // under an open bar).
            live.index = match &m {
                Some(m) => {
                    let pos = live.matches.iter().position(|x| x == m).or_else(|| {
                        let (matches, truncated) = collect_search_matches(&term, &mut live.regex);
                        live.matches = matches;
                        live.truncated = truncated;
                        live.matches.iter().position(|x| x == m)
                    });
                    pos.map(|p| p + 1).unwrap_or(0)
                }
                None => 0,
            };
            live.current = m;
        }
        if delta != 0 {
            self.scroll_display(delta);
        } else {
            self.poke_redraw();
        }
        found
    }

    /// Drop the live search (and its highlight).
    pub fn search_close(&self) {
        *self.search.lock() = None;
        self.poke_redraw();
    }

    /// The search-bar numbers ("3/12"), without touching the grid.
    pub fn search_status(&self) -> Option<SearchStatus> {
        let slot = self.search.lock();
        let live = slot.as_ref()?;
        Some(SearchStatus {
            index: live.index,
            total: live.matches.len(),
            truncated: live.truncated,
        })
    }

    /// What the renderer paints for a live search: the current match plus
    /// every other match intersecting the viewport (pale highlight), all in
    /// grid coordinates — the renderer maps them with the snapshot's
    /// display offset.
    pub fn search_render_state(&self) -> Option<SearchRender> {
        use alacritty_terminal::grid::Dimensions as _;
        // Lock order everywhere: term before search.
        let term = self.term.lock();
        let slot = self.search.lock();
        let live = slot.as_ref()?;
        let off = term.grid().display_offset() as i32;
        let rows = term.screen_lines() as i32;
        let as_pair = |m: &alacritty_terminal::term::search::Match| {
            (
                (m.start().line.0, m.start().column.0),
                (m.end().line.0, m.end().column.0),
            )
        };
        let current = live.current.as_ref().map(as_pair);
        let others = live
            .matches
            .iter()
            .filter(|m| {
                // Intersects the viewport (grid lines -off .. -off+rows-1)
                // and is not the current match (painted separately, solid).
                m.end().line.0 >= -off
                    && m.start().line.0 <= -off + rows - 1
                    && live.current.as_ref() != Some(*m)
            })
            .map(as_pair)
            .collect();
        Some(SearchRender { current, others })
    }

    /// Nudge the UI to repaint (generation bump + notify) without touching
    /// the grid — for state that lives beside the snapshot, like the search
    /// highlight.
    fn poke_redraw(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Jump to the previous (`dir < 0`) or next (`dir > 0`) shell prompt
    /// (OSC 133;A marks), ghostty-style. No-op without marks in that
    /// direction.
    pub fn jump_prompt(&self, dir: i32) {
        let delta = {
            let term = self.term.lock();
            let h = term.grid().history_size() as i64;
            let off = term.grid().display_offset() as i64;
            let marks = self.prompt_marks.lock();
            prompt_jump_delta(h, off, marks.iter().copied(), dir)
        };
        if let Some(d) = delta {
            self.scroll_display(d as i32);
        }
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
        let after = t.grid().display_offset();
        if after == before {
            return; // already clamped at top/bottom — nothing to repaint
        }
        if before == 0 && after > 0 {
            // Scrolling back freezes a screen-anchored selection to the
            // grid: from here it stays with its text, wheel and all.
            *self.screen_sel.lock() = None;
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

    /// Rebuild the snapshot from the current terminal state under the
    /// CURRENT global palette. A tab switch swaps the palette, but this
    /// session's last snapshot baked its ANSI colors under whichever
    /// palette was global when the PTY last printed — a background tab
    /// could wear another tab's colors until its next output.
    pub fn rebuild_snapshot(&self) {
        let t = self.term.lock();
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
    /// `cell_px` is the renderer's cell size in DEVICE pixels; it rides
    /// along so PTY consumers see real `TIOCGWINSZ` pixel dimensions and
    /// `CSI 16 t` replies (kitty graphics clients size images with them),
    /// and so image placements are footprinted in the same units the
    /// images are measured in.
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
    /// OSC 12 cursor color, when the application set one. `None` = paint
    /// the reverse-video block / theme-foreground thin cursor as always.
    pub cursor_color: Option<(u8, u8, u8)>,
    /// DECSCNM (`?5`): the whole screen renders with fg and bg swapped.
    /// terminfo's `flash` sets it, waits, and clears it — the visual bell.
    pub reverse_screen: bool,
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
            cursor_color: None,
            reverse_screen: false,
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
    /// Zero-width trailers stacked on this cell by the VTE: combining
    /// marks (NFD accents, dakuten), variation selectors (VS16), ZWJ.
    /// Rendered as ONE cluster with `c` — dropping them loses the accents
    /// (the adversarial-review finding). `None` for the common bare cell.
    pub zerowidth: Option<Box<[char]>>,
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
            zerowidth: None,
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
/// The screen-anchored drag core (see [`TerminalSession::selection_drag`]),
/// on a bare `Term` so it is unit-testable without a PTY: at the live tail
/// (`display_offset == 0`) the whole selection re-pins to the current
/// viewport cells; scrolled back, only the head moves (grid-glued).
fn apply_selection_drag<L>(
    term: &mut Term<L>,
    anchor: (usize, usize, bool),
    head: (usize, usize, bool),
    kind: SelectionKind,
) {
    if term.grid().display_offset() == 0 {
        let a = viewport_point(term, anchor.0, anchor.1);
        let mut sel = alacritty_terminal::selection::Selection::new(
            kind.to_alacritty(),
            a,
            side_of(anchor.2),
        );
        sel.update(viewport_point(term, head.0, head.1), side_of(head.2));
        term.selection = Some(sel);
    } else {
        let h = viewport_point(term, head.0, head.1);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(h, side_of(head.2));
        }
    }
}

/// Scroll delta (positive = older) that puts the previous/next prompt mark
/// at the top of the viewport. `h` = history size, `off` = current display
/// offset, `marks` = absolute buffer lines (oldest→newest). The viewport top
/// sits at absolute line `h - off`; the previous prompt is the newest mark
/// strictly above it, the next the oldest strictly below. `None` when no
/// mark lies in that direction (or the mark scrolled off the retained
/// history).
fn prompt_jump_delta(
    h: i64,
    off: i64,
    marks: impl DoubleEndedIterator<Item = u64> + Clone,
    dir: i32,
) -> Option<i64> {
    let top = h - off;
    let target = if dir < 0 {
        marks.rev().map(|m| m as i64).find(|&m| m < top)
    } else {
        marks.map(|m| m as i64).find(|&m| m > top)
    }?;
    let want_off = (h - target).clamp(0, h);
    (want_off != off).then_some(want_off - off)
}

/// Re-pin a live screen-anchored selection after output was applied: while
/// the view rides the live tail, the highlight stays on the viewport cells
/// the user marked, so streaming redraw loops (a timer-driven "Working…",
/// an app re-rendering on mouse reports) cannot walk it off the screen.
/// No-op when scrolled back (the session dropped the pin on scroll-back) or
/// when no screen-anchored selection is live.
pub(crate) fn repin_screen_selection<L>(term: &mut Term<L>, pin: &FairMutex<ScreenSel>) {
    if term.grid().display_offset() != 0 {
        return;
    }
    if let Some((kind, anchor, head)) = *pin.lock() {
        apply_selection_drag(term, anchor, head, kind);
    }
}

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
                // Combining marks / VS16 / ZWJ stacked on this cell — but
                // NOT for a kitty placeholder, whose "diacritics" are tile
                // coordinates already decoded into `image` above.
                zerowidth: if image.is_some() {
                    None
                } else {
                    indexed
                        .zerowidth()
                        .filter(|z| !z.is_empty())
                        .map(|z| z.to_vec().into_boxed_slice())
                },
            };
        }
    }

    // Effective wrap for URL detection: line editors (pwsh's PSReadLine)
    // repaint long input lines with explicit newlines/cursor motion, so a
    // visually wrapped URL often carries no WRAPLINE flag and the
    // continuation row lost its underline (殿 report 2026-07-20). When a
    // row is filled to its last column with URL-ish characters and the
    // next row starts with one, treat it as wrapped for link purposes —
    // the URL regex still decides what actually links, so a false join
    // costs nothing.
    let mut url_wrapped = wrapped.clone();
    for row in 0..rows.saturating_sub(1) {
        if !url_wrapped[row]
            && let (Some(a), Some(b)) = (cells[row].last(), cells[row + 1].first())
        {
            url_wrapped[row] = a.display_width != 0
                && b.display_width != 0
                && is_bare_url_char(a.c)
                && is_bare_url_char(b.c);
        }
    }
    detect_implicit_links(&mut cells, &url_wrapped, &mut links, &mut link_ids);
    let links = assign_link_occurrences(&mut cells, &url_wrapped, &links);

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
        // OSC 12: the app's dynamic cursor color, honored by the renderer.
        cursor_color: term.colors()[alacritty_terminal::vte::ansi::NamedColor::Cursor]
            .map(|c| (c.r, c.g, c.b)),
        reverse_screen: term
            .mode()
            .contains(alacritty_terminal::term::TermMode::SCREEN_REVERSE),
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

/// A character a bare URL can contain — shared by [`detect_urls`] and the
/// effective-wrap heuristic in `take_snapshot`.
fn is_bare_url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '`')
}

/// `[start, end)` char spans of bare URLs in `chars`, with the URI text.
fn detect_urls(chars: &[char]) -> Vec<(usize, usize, String)> {
    let is_url_char = is_bare_url_char;
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

/// Palette fallback for the 16 ANSI colors when the term color table has no
/// entry yet (an app's OSC 4 override still wins — see `resolve_color`). The
/// values come from the active [`theme`] palette, so a configured/wt-imported
/// theme recolors named-color text; the built-in default is Tango.
fn fallback_named_color(named: alacritty_terminal::vte::ansi::NamedColor) -> Option<ResolvedColor> {
    use alacritty_terminal::vte::ansi::NamedColor;
    let idx: u8 = match named {
        NamedColor::Black => 0,
        NamedColor::Red => 1,
        NamedColor::Green => 2,
        NamedColor::Yellow => 3,
        NamedColor::Blue => 4,
        NamedColor::Magenta => 5,
        NamedColor::Cyan => 6,
        NamedColor::White => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        _ => return None,
    };
    let c = crate::theme::ansi(idx);
    Some(ResolvedColor::Rgb(c.r, c.g, c.b))
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

/// Resolve an OSC color-query index the way the renderer actually paints
/// it. The dynamic default fg/bg answer from the ACTIVE THEME — the theme
/// is authoritative for what's on screen (OSC 10/11 sets are deliberately
/// not honored, and answering the stored-but-unpainted value lied to
/// set-then-query apps). The cursor answers its honored OSC 12 color, else
/// the theme fg it is drawn in. Palette indices 0–255: an explicitly set
/// entry (OSC 4) wins — those ARE honored — otherwise the engine's
/// standard palette.
pub(crate) fn query_color_rgb(
    colors: &alacritty_terminal::term::color::Colors,
    idx: usize,
) -> Option<alacritty_terminal::vte::ansi::Rgb> {
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
    if idx >= alacritty_terminal::term::color::COUNT {
        return None;
    }
    if idx == NamedColor::Foreground as usize {
        let c = crate::theme::foreground();
        return Some(Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        });
    }
    if idx == NamedColor::Background as usize {
        let c = crate::theme::background();
        return Some(Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        });
    }
    if idx == NamedColor::Cursor as usize {
        return Some(colors[idx].unwrap_or_else(|| {
            let c = crate::theme::foreground();
            Rgb {
                r: c.r,
                g: c.g,
                b: c.b,
            }
        }));
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
    None
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

    /// Replay a captured PTY dump (`RIKKA_DUMP_REPLAY=<path>`) frame by frame
    /// and report where each frame put the row matching `RIKKA_DUMP_MATCH`
    /// (default "interrupt"). A column that oscillates between frames means
    /// the horizontal jitter is already in the engine's grid; a stable column
    /// puts it downstream in the renderer. Ignored by default — a diagnostic,
    /// not a gate.
    #[test]
    #[ignore]
    fn replay_dump_frame_columns() {
        let Some(path) = std::env::var_os("RIKKA_DUMP_REPLAY") else {
            eprintln!("set RIKKA_DUMP_REPLAY=<dump path> to run");
            return;
        };
        let data = std::fs::read(&path).expect("read dump");
        let cols: usize = std::env::var("RIKKA_DUMP_COLS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let rows: usize = std::env::var("RIKKA_DUMP_ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(37);
        let needle = std::env::var("RIKKA_DUMP_MATCH").unwrap_or_else(|_| "interrupt".into());
        let mut term = make_term(cols, rows);
        let mut parser = Processor::<StdSyncHandler>::new();
        const ESU: &[u8] = b"\x1b[?2026l";
        let mut frames = 0usize;
        let mut seen: Vec<(usize, usize, usize, String)> = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            parser.advance(&mut term, b);
            // Frame boundary: the byte just consumed completed an ESU.
            if i + 1 >= ESU.len() && &data[i + 1 - ESU.len()..=i] == ESU {
                frames += 1;
                // RIKKA_DUMP_BOX="r0,r1,c0,c1": report the ink inside that
                // rectangle whenever it changes, with the byte offset of the
                // frame that changed it — the way to find which bytes emptied
                // a region that should have kept its content.
                if let Ok(spec) = std::env::var("RIKKA_DUMP_BOX") {
                    let v: Vec<usize> = spec
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    if v.len() == 4 {
                        let snap = take_snapshot(&term);
                        let ink: usize = snap
                            .cells
                            .iter()
                            .take(v[1] + 1)
                            .skip(v[0])
                            .map(|row| {
                                row.iter()
                                    .take(v[3] + 1)
                                    .skip(v[2])
                                    .filter(|c| c.c != ' ')
                                    .count()
                            })
                            .sum();
                        static LAST: std::sync::atomic::AtomicUsize =
                            std::sync::atomic::AtomicUsize::new(usize::MAX);
                        if LAST.swap(ink, Ordering::Relaxed) != ink {
                            eprintln!("frame {frames:5} @byte {i:8} box_ink={ink}");
                        }
                    }
                }
                let snap = take_snapshot(&term);
                for (r, row) in snap.cells.iter().enumerate() {
                    let text: String = row.iter().map(|c| c.c).collect();
                    if text.contains(&needle) {
                        let first = text.find(|c: char| c != ' ').unwrap_or(0);
                        seen.push((frames, r, first, text.trim().chars().take(46).collect()));
                    }
                    let _ = &text;
                }
            }
        }
        // RIKKA_DUMP_GRID=1: print the FINAL screen as text. Diffing that
        // against `tmux capture-pane -p` decides whether a multiplexer's
        // stream was interpreted faithfully — no screenshots, no OCR.
        if std::env::var_os("RIKKA_DUMP_GRID").is_some() {
            let snap = take_snapshot(&term);
            eprintln!("--- FINAL GRID {}x{} ---", snap.cols, snap.rows);
            for (r, row) in snap.cells.iter().enumerate() {
                let text: String = row.iter().map(|c| c.c).collect();
                eprintln!("{r:02}|{}|", text.trim_end());
            }
            eprintln!("--- END GRID ---");
        }
        eprintln!("frames={frames} matches={}", seen.len());
        for (f, r, col, text) in seen.iter().take(40) {
            eprintln!("frame {f:4} row {r:2} startcol {col:3}  {text}");
        }
        let cols_seen: std::collections::BTreeSet<usize> =
            seen.iter().map(|(_, _, c, _)| *c).collect();
        let rows_seen: std::collections::BTreeSet<usize> =
            seen.iter().map(|(_, r, _, _)| *r).collect();
        eprintln!("distinct start columns: {cols_seen:?}");
        eprintln!("distinct rows: {rows_seen:?}");
    }

    /// DECSLRM: a scroll inside left/right margins must move ONLY that column
    /// band. Without margins the same scroll takes the whole line — which is
    /// how a multiplexer scrolling one pane wiped the panes beside it (tmux
    /// asks for margins whenever the terminal claims to have them).
    #[test]
    fn decslrm_scroll_stays_inside_the_column_band() {
        let mut term = make_term(20, 8);
        // `AAAAn|BnBBB| CCCCC` — the row number appears BOTH outside the band
        // (index 4) and inside it (index 6), so each side can be checked to
        // have moved or stayed on its own.
        for r in 1..=6 {
            advance_bytes(&mut term, format!("\x1b[{r};1H").as_bytes());
            advance_bytes(&mut term, format!("AAAA{r}B{r}BBB CCCCC").as_bytes());
        }
        // Enable margins, confine to columns 6-10, scroll rows 1-6 up by 2.
        advance_bytes(&mut term, b"\x1b[?69h\x1b[6;10s\x1b[1;6r\x1b[2S");
        let snap = take_snapshot(&term);
        let row = |r: usize| -> String { snap.cells[r].iter().map(|c| c.c).collect() };

        // Outside the band both edges are untouched…
        assert!(
            row(0).starts_with("AAAA1"),
            "left of the band moved: {:?}",
            row(0)
        );
        assert!(
            row(0).trim_end().ends_with("CCCCC"),
            "right of the band moved: {:?}",
            row(0)
        );
        // …inside it, row 1 now shows what row 3 held.
        assert_eq!(&row(0)[5..10], "B3BBB", "band did not scroll: {:?}", row(0));
        // The bottom two rows of the region are blank INSIDE the band only.
        assert_eq!(
            &row(5)[5..10],
            "     ",
            "band tail not cleared: {:?}",
            row(5)
        );
        assert!(
            row(5).starts_with("AAAA6"),
            "clearing escaped the band: {:?}",
            row(5)
        );
        assert!(
            row(5).trim_end().ends_with("CCCCC"),
            "clearing escaped the band: {:?}",
            row(5)
        );
    }

    /// Without DECLRMM the same `CSI s` is SCOSC and scrolling is full-width,
    /// exactly as before — margins must not change any existing behavior.
    #[test]
    fn scroll_without_margins_is_unchanged() {
        let mut term = make_term(20, 6);
        for r in 1..=4 {
            advance_bytes(&mut term, format!("\x1b[{r};1H").as_bytes());
            advance_bytes(&mut term, format!("AAAA{r}BBBBB CCCCC").as_bytes());
        }
        // `CSI 6;10 s` with the mode OFF must NOT become a margin.
        advance_bytes(&mut term, b"\x1b[6;10s\x1b[1;4r\x1b[1S");
        let snap = take_snapshot(&term);
        let row0: String = snap.cells[0].iter().map(|c| c.c).collect();
        assert!(
            row0.starts_with("AAAA2"),
            "full-width scroll broke: {:?}",
            row0
        );
    }

    /// DECSCNM: terminfo declares `flash` as `?5h` … `?5l`, so the visual
    /// bell only exists if the mode actually reaches the snapshot.
    #[test]
    fn decscnm_reverses_the_screen() {
        let mut term = make_term(10, 3);
        advance_bytes(&mut term, b"hi");
        assert!(!take_snapshot(&term).reverse_screen);
        advance_bytes(&mut term, b"\x1b[?5h");
        assert!(
            take_snapshot(&term).reverse_screen,
            "?5h did not set DECSCNM"
        );
        advance_bytes(&mut term, b"\x1b[?5l");
        assert!(
            !take_snapshot(&term).reverse_screen,
            "?5l did not clear DECSCNM"
        );
    }

    #[test]
    fn sync_probe_2026_buffering() {
        let mut term = make_term(20, 5);
        let mut parser = Processor::<StdSyncHandler>::new();
        for &b in b"\x1b[?2026h\x1b[1;1HXYZ".iter() {
            parser.advance(&mut term, b);
        }
        let mid = take_snapshot(&term);
        assert_eq!(
            mid.cells[0][0].c, ' ',
            "grid changed mid-sync - BSU not buffering"
        );
        for &b in b"\x1b[?2026l".iter() {
            parser.advance(&mut term, b);
        }
        let after = take_snapshot(&term);
        assert_eq!(
            after.cells[0][0].c, 'X',
            "ESU did not apply the buffered frame"
        );
    }

    /// Double-click = word (semantic boundaries), triple-click = whole line:
    /// the kind expands the range beyond the clicked cell, and survives the
    /// streaming re-pin.
    #[test]
    fn word_and_line_selection_kinds_expand() {
        let mut term = make_term(40, 4);
        advance_bytes(&mut term, b"hello world etc");
        // Word: a click inside "world" (col 8) spans cols 6..=10.
        apply_selection_drag(&mut term, (0, 8, false), (0, 8, false), SelectionKind::Word);
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .unwrap();
        assert_eq!((r.start.column.0, r.end.column.0), (6, 10));

        // Line: the whole row regardless of the clicked column.
        apply_selection_drag(&mut term, (0, 8, false), (0, 8, false), SelectionKind::Line);
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .unwrap();
        assert_eq!((r.start.column.0, r.end.column.0), (0, 39));

        // The re-pin preserves the kind: after rotation the same text sits
        // on viewport row 3 — a Word pin there must re-expand to the word.
        let pin = FairMutex::new(Some((SelectionKind::Word, (3, 8, false), (3, 8, false))));
        advance_bytes(&mut term, b"\r\nx\r\ny\r\nz\r\nhello world etc");
        repin_screen_selection(&mut term, &pin);
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .unwrap();
        assert_eq!(
            (r.start.column.0, r.end.column.0),
            (6, 10),
            "word expansion must survive the streaming re-pin"
        );
    }

    /// Prompt jumping: previous = the newest mark above the viewport top,
    /// next = the oldest below; deltas are scroll_display units (positive =
    /// older). No mark in that direction = no-op.
    #[test]
    fn prompt_jump_delta_walks_marks() {
        let marks = [10u64, 50, 90];
        // Live tail (off=0, top=100): prev is the newest mark, 90.
        assert_eq!(
            prompt_jump_delta(100, 0, marks.iter().copied(), -1),
            Some(10)
        );
        // From there (top=90): prev = 50.
        assert_eq!(
            prompt_jump_delta(100, 10, marks.iter().copied(), -1),
            Some(40)
        );
        // And next from the middle goes back down to 90.
        assert_eq!(
            prompt_jump_delta(100, 50, marks.iter().copied(), 1),
            Some(-40)
        );
        // Nothing newer than the tail; nothing older than the first mark.
        assert_eq!(prompt_jump_delta(100, 0, marks.iter().copied(), 1), None);
        assert_eq!(prompt_jump_delta(100, 90, marks.iter().copied(), -1), None);
    }

    /// Streaming rotation must not slide an in-flight drag: at the live
    /// tail every update re-pins the whole selection to the CURRENT
    /// viewport cells (an Ink-style shimmer that scrolls each repaint used
    /// to drag the anchor off what the pointer covered). Scrolled back,
    /// the drag stays grid-glued: only the head moves.
    #[test]
    fn screen_anchored_drag_repins_under_rotation() {
        let mut term = make_term(20, 4);
        advance_bytes(&mut term, b"aaa\r\nbbb\r\nccc");
        // Drag over viewport row 0, cols 0..=2 ("aaa").
        apply_selection_drag(
            &mut term,
            (0, 0, false),
            (0, 2, true),
            SelectionKind::Simple,
        );
        // The app streams two more lines — one rotation into history.
        advance_bytes(&mut term, b"\r\nddd\r\neee");
        // Same pointer cells on the next update: the selection must cover
        // viewport row 0 as it is NOW, not the rotated-away line.
        apply_selection_drag(
            &mut term,
            (0, 0, false),
            (0, 2, true),
            SelectionKind::Simple,
        );
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .expect("live-tail drag always selects");
        let offset = term.grid().display_offset() as i32;
        assert_eq!(offset, 0);
        assert_eq!(r.start.line.0 + offset, 0, "must sit on viewport row 0");
        assert_eq!((r.start.column.0, r.end.column.0), (0, 2));

        // Scrolled back the view is still — the grid-glued path: the anchor
        // stays put and only the head follows the pointer.
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
        assert_eq!(term.grid().display_offset(), 1); // clamped to history
        apply_selection_drag(
            &mut term,
            (0, 0, false),
            (1, 3, true),
            SelectionKind::Simple,
        );
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .expect("selection persists across the scroll");
        // Anchor: still grid line 0 from the re-pin above — NOT re-anchored
        // to the new viewport row 0 (grid line -1). Head: viewport (1,3)
        // with offset 1 → grid line 0, column 3.
        assert_eq!((r.start.line.0, r.end.line.0), (0, 0));
        assert_eq!((r.start.column.0, r.end.column.0), (0, 3));
    }

    /// A COMPLETED selection must not walk off the screen either: the parse
    /// thread re-pins it from the stored viewport cells after every output
    /// application (Codex's timer-driven redraws, Claude Code re-rendering
    /// on hover-driven mouse reports). Once scrolled back the pin drops and
    /// rotation leaves the grid-glued selection to follow its text.
    #[test]
    fn completed_selection_re_pins_until_scrollback_freezes_it() {
        let mut term = make_term(20, 4);
        advance_bytes(&mut term, b"aaa\r\nbbb\r\nccc");
        let pin = FairMutex::new(Some((
            SelectionKind::Simple,
            (0usize, 0usize, false),
            (0usize, 2usize, true),
        )));
        apply_selection_drag(
            &mut term,
            (0, 0, false),
            (0, 2, true),
            SelectionKind::Simple,
        );

        // Rotation after the mouse released → the parse loop re-pins.
        advance_bytes(&mut term, b"\r\nddd\r\neee");
        repin_screen_selection(&mut term, &pin);
        let r = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .expect("pinned selection survives rotation");
        assert_eq!(
            r.start.line.0 + term.grid().display_offset() as i32,
            0,
            "highlight stays on viewport row 0"
        );

        // Scroll-back drops the pin (the session API does); from here the
        // frozen selection follows its TEXT through further rotation.
        *pin.lock() = None;
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(1));
        let frozen = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .expect("selection persists across the scroll");
        advance_bytes(&mut term, b"\r\nfff");
        repin_screen_selection(&mut term, &pin); // must be a no-op now
        let after = (term.selection.as_ref())
            .and_then(|s| s.to_range(&term))
            .expect("grid-glued selection persists");
        assert_eq!(after.start.line.0, frozen.start.line.0 - 1);
        assert_eq!(after.start.column.0, frozen.start.column.0);
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
    fn snapshot_keeps_zerowidth_trailers() {
        // Feed NFD "e + combining acute" through the real VTE: the
        // snapshot cell must carry the mark (the adversarial-review
        // finding — it used to be dropped for every non-kitty cell).
        let mut term = make_term(20, 3);
        advance_bytes(&mut term, "e\u{0301}x".as_bytes());
        let snap = take_snapshot(&term);
        assert_eq!(snap.cells[0][0].c, 'e');
        assert_eq!(
            snap.cells[0][0].zerowidth.as_deref(),
            Some(&['\u{0301}'][..])
        );
        assert_eq!(snap.cells[0][1].c, 'x');
        assert!(snap.cells[0][1].zerowidth.is_none());
    }

    #[test]
    fn implicit_url_joins_hard_wrapped_rows() {
        // PSReadLine-style repaint: the line is written to the last column,
        // then an explicit CRLF moves to the next row — no WRAPLINE flag.
        // The effective-wrap heuristic must still join the rows so the
        // continuation keeps its link (and its dotted underline).
        let mut term = make_term(10, 3);
        advance_bytes(&mut term, b"https://e.\r\ncom/ab");
        let snap = take_snapshot(&term);
        assert_eq!(snap.links, vec!["https://e.com/ab".to_string()]);
        assert_eq!(snap.cells[0][9].link, Some(0));
        assert_eq!(
            snap.cells[1][0].link,
            Some(0),
            "continuation row lost its link"
        );
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
    fn detect_urls_keeps_trailing_ampersand() {
        // Discord CDN links end in "&" — that is part of the URL, not prose
        // punctuation (殿 2026-07-21).
        let chars: Vec<char> = "https://cdn.example/a.txt?ex=1&hm=2&".chars().collect();
        let got = detect_urls(&chars);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2, "https://cdn.example/a.txt?ex=1&hm=2&");
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
            replies: Arc::default(),
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
        // Theme-relative expectations: another test may install a palette
        // concurrently (theme state is process-global), and the point here
        // is exactly that the query mirrors the painted theme.
        let theme_rgb = |c: crate::theme::Rgb| Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        };
        let mut colors = Colors::default();
        // The dynamic specials answer the active theme (what is painted)…
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Background as usize),
            Some(theme_rgb(crate::theme::background()))
        );
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Foreground as usize),
            Some(theme_rgb(crate::theme::foreground()))
        );
        // …indexed colors the standard palette (196 = pure red in the
        // 6×6×6 cube)…
        assert_eq!(
            query_color_rgb(&colors, 196),
            Some(Rgb { r: 255, g: 0, b: 0 })
        );
        // …an OSC 11 set does NOT change the answer — the theme stays
        // authoritative for what's painted, and the query must not claim
        // an unhonored color is in effect…
        colors[NamedColor::Background as usize] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Background as usize),
            Some(theme_rgb(crate::theme::background()))
        );
        // …an explicitly set palette entry (OSC 4) still wins — those ARE
        // honored by the renderer…
        colors[196] = Some(Rgb { r: 9, g: 8, b: 7 });
        assert_eq!(
            query_color_rgb(&colors, 196),
            Some(Rgb { r: 9, g: 8, b: 7 })
        );
        // …and the honored OSC 12 cursor color answers its set value.
        colors[NamedColor::Cursor as usize] = Some(Rgb { r: 4, g: 5, b: 6 });
        assert_eq!(
            query_color_rgb(&colors, NamedColor::Cursor as usize),
            Some(Rgb { r: 4, g: 5, b: 6 })
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

    /// DA1 must park in the ordered reply buffer, never on the clipboard
    /// channel — the channel is an immediate write and would put the fence
    /// ahead of the answers it fences (the kitty-detection inversion).
    #[test]
    fn da1_reply_parks_in_pending_replies_not_on_the_channel() {
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let replies: Arc<PendingReplies> = Arc::default();
        let listener = ClipboardListener {
            tx,
            replies: Arc::clone(&replies),
            title: Arc::default(),
        };
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), listener);
        let mut parser = Processor::<StdSyncHandler>::new();
        for &b in b"\x1b[c".iter() {
            parser.advance(&mut term, b);
        }
        assert!(
            rx.try_recv().is_err(),
            "DA1 must not take the immediate path"
        );
        let mut out = Vec::new();
        replies.drain_into(&mut out);
        assert_eq!(out, vec![b"\x1b[?62;4;22c".to_vec()]);
    }

    /// The engine's parked replies leave in emission order and the flag
    /// resets, so a second drain with nothing parked is a no-op.
    #[test]
    fn pending_replies_drain_in_order_and_reset() {
        let r = PendingReplies::default();
        let mut out: Vec<Vec<u8>> = vec![b"before".to_vec()];
        r.drain_into(&mut out);
        assert_eq!(out.len(), 1, "nothing parked, nothing appended");
        r.push("\x1b[?62;4;22c".into());
        r.push("\x1b[>0;1;1c".into());
        r.drain_into(&mut out);
        assert_eq!(
            out,
            vec![
                b"before".to_vec(),
                b"\x1b[?62;4;22c".to_vec(),
                b"\x1b[>0;1;1c".to_vec()
            ]
        );
        r.drain_into(&mut out);
        assert_eq!(out.len(), 3, "drained buffer stays empty");
    }
    /// Placeholder rows through both reflow paths — Alacritty's native one
    /// and the conhost-parity one a local ConPTY session uses — at a width
    /// they fill EXACTLY, narrower (they split), and back. The exact-fit
    /// case is the one that regressed live: the conhost port wrapped first
    /// and newlined second, leaving an empty row after every image row, so
    /// an image that fit the window to the cell doubled its height with
    /// blank stripes. Real conhost defers that wrap (probe in pty_local).
    #[test]
    fn placeholder_rows_reflow_without_phantom_rows_on_both_paths() {
        use alacritty_terminal::grid::Dimensions as _;
        use alacritty_terminal::index::{Column, Line};
        let placeholder_lines = |term: &Term<VoidListener>| -> Vec<(usize, usize)> {
            (0..term.screen_lines())
                .filter_map(|line| {
                    let row = &term.grid()[Line(line as i32)];
                    let n = (0..term.columns())
                        .filter(|&c| row[Column(c)].c == kitty_graphics::PLACEHOLDER)
                        .count();
                    (n > 0).then_some((line, n))
                })
                .collect()
        };
        let mut bytes = b"\x1b[2J\x1b[Hlabel\r\n".to_vec();
        bytes.extend(crate::sixel::placeholder_bytes(78, 44, 3, 0));
        bytes.extend(b"\r\nafter image\r\n");
        for conhost in [false, true] {
            let mut term = Term::new(Config::default(), &TermSize::new(133, 12), VoidListener);
            advance_bytes(&mut term, &bytes);
            let resize = |term: &mut Term<VoidListener>, cols: usize| {
                if conhost {
                    term.resize_anchored(TermSize::new(cols, 12), true);
                } else {
                    term.resize(TermSize::new(cols, 12));
                }
            };
            // The native path may scroll leading rows into history while
            // narrowing (viewport anchoring differs), so compare the
            // visible tail: contiguous lines, expected widths.
            let check = |term: &Term<VoidListener>, expect: &[usize], what: &str| {
                let got = placeholder_lines(term);
                let contiguous = got.windows(2).all(|w| w[1].0 == w[0].0 + 1);
                let widths: Vec<usize> = got.iter().map(|&(_, n)| n).collect();
                assert!(
                    contiguous,
                    "{what}: phantom row between image rows (conhost={conhost}): {got:?}"
                );
                assert!(
                    !widths.is_empty() && expect.ends_with(&widths),
                    "{what} (conhost={conhost}): got {widths:?}, expected a tail of {expect:?}"
                );
            };
            resize(&mut term, 44);
            check(&term, &[44, 44, 44], "exact fit");
            resize(&mut term, 31);
            check(&term, &[31, 13, 31, 13, 31, 13], "narrower splits 31 + 13");
            resize(&mut term, 133);
            check(&term, &[44, 44, 44], "widening restores the rows");
        }
    }
}
