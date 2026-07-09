//! RikkaTerminal — standalone terminal prototype on rikka-terminal-core.
//!
//! Deliberately thin: spawn a local shell over ConPTY, hand the byte pipes to
//! `build_terminal_session`, and render the engine's grid in one gpui window.
//! Everything protocol-shaped (keys incl. kitty keyboard, mouse reporting,
//! selection that tracks the grid, IME preedit, kitty graphics, OSC title…)
//! comes from the engine. See README.md for the design and roadmap.

use std::io::{Read, Write};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, Application, Bounds, Context, ElementInputHandler, Entity, FocusHandle, KeyDownEvent,
    ScrollDelta, ScrollWheelEvent, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas,
    div, prelude::*, px, rgb, size,
};
use parking_lot::FairMutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use rikka_terminal_core::ime::{ImeHost, TerminalIme};
use rikka_terminal_core::keys::key_to_pty_bytes;
use rikka_terminal_core::renderer::{measure_cell_metrics, render_grid};
use rikka_terminal_core::selection::{self, SelectionHost, SelectionState};
use rikka_terminal_core::{PtyResizer, ReportMods, TerminalSession, xtversion};

/// Default font: always present on Windows; CJK falls through DirectWrite's
/// system fallback. Bundled fonts are a P1 item.
const MONO_FONT: &str = "Consolas";
/// Pane padding (logical px); half on each side via `.p_1()`.
const PAD: f32 = 8.0;
/// PTY-burst coalescing window (same rationale/value as shogun-desktop).
const FRAME_COALESCE: Duration = Duration::from_millis(8);

// ── local PTY plumbing ───────────────────────────────────────────────────────

/// Newtype asserting `Box<dyn MasterPty>` is `Send + Sync` — same reasoning as
/// shogun-desktop's pty_spawn: ConPTY's HPCON is thread-safe for resize, and
/// the FairMutex serializes access anyway.
struct SendMaster(Box<dyn portable_pty::MasterPty>);
unsafe impl Send for SendMaster {}
unsafe impl Sync for SendMaster {}

struct LocalResizer {
    master: FairMutex<SendMaster>,
}

impl PtyResizer for LocalResizer {
    fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Result<()> {
        self.master.lock().0.resize(PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        })?;
        Ok(())
    }
}

/// Spawn `shell` on a local PTY and wire it into an engine session.
fn spawn_local_shell(shell: &str, cols: u16, rows: u16) -> Result<TerminalSession> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(shell);
    cmd.env("TERM_PROGRAM", xtversion::TERM_PROGRAM);
    cmd.env("TERM_PROGRAM_VERSION", xtversion::TERM_PROGRAM_VERSION);
    let _child = pair.slave.spawn_command(cmd)?;
    let writer: Box<dyn Write + Send> = pair.master.take_writer()?;
    let reader: Box<dyn Read + Send> = Box::new(pair.master.try_clone_reader()?);
    let resizer: Arc<dyn PtyResizer> = Arc::new(LocalResizer {
        master: FairMutex::new(SendMaster(pair.master)),
    });
    build_session(cols, rows, reader, writer, resizer)
}

fn build_session(
    cols: u16,
    rows: u16,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    resizer: Arc<dyn PtyResizer>,
) -> Result<TerminalSession> {
    rikka_terminal_core::pty_session::build_terminal_session(
        cols,
        rows,
        reader,
        Arc::new(FairMutex::new(writer)),
        resizer,
        &xtversion::engine_identity(),
    )
}

/// The shells to try, most preferred first.
fn shell_candidates() -> Vec<String> {
    if cfg!(windows) {
        let mut v = vec!["pwsh.exe".to_string(), "powershell.exe".to_string()];
        v.push(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
        v
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    }
}

// ── the window ───────────────────────────────────────────────────────────────

struct TermWindow {
    session: Option<TerminalSession>,
    terminal_focus: FocusHandle,
    ime: Entity<TerminalIme<Self>>,
    selection: SelectionState,
    /// Fractional wheel accumulators (trackpads deliver sub-line deltas).
    scroll_accum: f32,
    hwheel_accum: f32,
    /// Last PTY size applied, to dedup the per-frame viewport check.
    cols: u16,
    rows: u16,
    /// Last OSC title applied to the OS window (dedup).
    applied_title: Option<String>,
}

impl ImeHost for TermWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.session.as_ref()
    }

    fn ime_font(&self) -> &str {
        MONO_FONT
    }
}

impl SelectionHost for TermWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn pane_session(&self, _pane: usize) -> Option<&TerminalSession> {
        self.session.as_ref()
    }
}

impl TermWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
        let terminal_focus = cx.focus_handle();
        window.focus(&terminal_focus);

        let session = shell_candidates()
            .iter()
            .find_map(|shell| spawn_local_shell(shell, 80, 24).ok());
        if let Some(s) = &session {
            Self::spawn_refresh(cx, s);
        }
        Self {
            session,
            terminal_focus,
            ime,
            selection: SelectionState::default(),
            scroll_accum: 0.0,
            hwheel_accum: 0.0,
            cols: 0,
            rows: 0,
            applied_title: None,
        }
    }

    /// Park on the session's notify; coalesce bursts into ~120 fps repaints.
    /// Blink (SGR cells or a blinking cursor) races a phase timer so the
    /// on/off flip repaints without output.
    fn spawn_refresh(cx: &mut Context<Self>, session: &TerminalSession) {
        let generation = Arc::clone(&session.generation);
        let notify = Arc::clone(&session.notify);
        let snapshot = Arc::clone(&session.snapshot);
        cx.spawn(async move |this, cx| {
            let mut last = generation.load(Ordering::Relaxed);
            loop {
                let blink = {
                    let s = snapshot.lock();
                    s.has_blink || s.cursor_blink
                };
                if blink {
                    let timer = cx.background_executor().timer(Duration::from_millis(300));
                    futures::future::select(Box::pin(notify.notified()), Box::pin(timer)).await;
                } else {
                    notify.notified().await;
                }
                cx.background_executor().timer(FRAME_COALESCE).await;
                let cur = generation.load(Ordering::Relaxed);
                if cur == last && !blink {
                    continue;
                }
                last = cur;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for TermWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // OSC 0/2 → OS window title (deduped; render runs per frame).
        if let Some(s) = self.session.as_ref() {
            let title = s.title.lock().clone();
            if title != self.applied_title {
                window.set_window_title(title.as_deref().unwrap_or("RikkaTerminal"));
                self.applied_title = title;
            }
        }

        let (cw, ch) = measure_cell_metrics(&cx.text_system(), MONO_FONT, window.scale_factor());

        // Fit the PTY to the viewport (content box = window minus padding).
        let vp = window.viewport_size();
        let content_w = (vp.width / px(1.)) - PAD;
        let content_h = (vp.height / px(1.)) - PAD;
        if content_w > cw && content_h > ch {
            let new_cols = ((content_w / cw) as u16).max(2);
            let new_rows = ((content_h / ch) as u16).max(2);
            if (new_cols, new_rows) != (self.cols, self.rows) {
                self.cols = new_cols;
                self.rows = new_rows;
                if let Some(s) = self.session.as_ref() {
                    s.resize(new_cols, new_rows, (cw, ch));
                }
            }
        }

        let Some(snap) = self.session.as_ref().map(|s| s.snapshot.lock().clone()) else {
            return div()
                .size_full()
                .bg(rgb(0x1A1A1A))
                .text_color(rgb(0xE8DCC8))
                .child("シェルを起動できなかった (pwsh / cmd が見つからない)")
                .into_any_element();
        };
        let images = self.session.as_ref().map(|s| Arc::clone(&s.images));
        let ime_preedit = self.ime.read(cx).marked.clone();
        let focus_handle = self.terminal_focus.clone();
        let ime = self.ime.clone();
        let view = cx.entity();
        let (grid_rows, grid_cols) = (snap.rows, snap.cols);

        div()
            .size_full()
            .bg(rgb(0x1A1A1A))
            .track_focus(&focus_handle)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _win, cx| {
                let ks = &event.keystroke;
                let m = &ks.modifiers;
                // Copy / paste chords (both the conventional and the classic
                // terminal keys), handled before the PTY encoder.
                if (m.control && m.shift && ks.key == "c")
                    || (m.control && !m.shift && ks.key == "insert")
                {
                    selection::copy_to_clipboard(&this.selection, this.session.as_ref(), cx);
                    cx.stop_propagation();
                    return;
                }
                if (m.control && m.shift && ks.key == "v")
                    || (!m.control && m.shift && ks.key == "insert")
                {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                        && let Some(s) = &this.session
                    {
                        s.paste(&text);
                    }
                    cx.stop_propagation();
                    return;
                }
                // Shift+PageUp/PageDown: page through the scrollback.
                if m.shift && (ks.key == "pageup" || ks.key == "pagedown") {
                    if let Some(s) = &this.session {
                        let page = s.rows.load(Ordering::Relaxed).saturating_sub(1) as i32;
                        s.scroll_display(if ks.key == "pageup" { page } else { -page });
                    }
                    cx.stop_propagation();
                    return;
                }
                // Everything else through the engine's encoder (legacy or
                // kitty keyboard, mode-dependent). Printable unmodified keys
                // return None and keep propagating so WM_CHAR feeds the IME
                // input handler instead (otherwise chars would double).
                if let Some(s) = &this.session {
                    let mode = *s.term.lock().mode();
                    if let Some(bytes) = key_to_pty_bytes(ks, mode) {
                        s.send_bytes(&bytes);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _win, _cx| {
                    let Some(s) = &this.session else { return };
                    let pad = PAD / 2.0;
                    let cols = s.cols.load(Ordering::Relaxed).max(1) as usize;
                    let rows = s.rows.load(Ordering::Relaxed).max(1) as usize;
                    let col = ((((event.position.x / px(1.)) - pad) / cw).max(0.0) as usize)
                        .min(cols - 1);
                    let row = ((((event.position.y / px(1.)) - pad) / ch).max(0.0) as usize)
                        .min(rows - 1);
                    let mods = ReportMods {
                        alt: event.modifiers.alt,
                        ctrl: event.modifiers.control,
                    };
                    // Vertical: PTY first (mouse reporting / alternate scroll),
                    // local scrollback otherwise.
                    this.scroll_accum += match &event.delta {
                        ScrollDelta::Pixels(p) => (p.y / px(1.)) / ch,
                        ScrollDelta::Lines(l) => l.y,
                    };
                    let whole = this.scroll_accum.trunc() as i32;
                    if whole != 0 {
                        this.scroll_accum -= whole as f32;
                        if !s.wheel_to_pty(whole, col, row, mods) {
                            s.scroll_display(whole);
                        }
                    }
                    // Horizontal: reporting-only (buttons 66/67).
                    this.hwheel_accum += match &event.delta {
                        ScrollDelta::Pixels(p) => (p.x / px(1.)) / cw,
                        ScrollDelta::Lines(l) => l.x,
                    };
                    let whole_x = this.hwheel_accum.trunc() as i32;
                    if whole_x != 0 {
                        this.hwheel_accum -= whole_x as f32;
                        s.hwheel_to_pty(whole_x, col, row, mods);
                    }
                }),
            )
            .p_1()
            .child(
                div()
                    .relative()
                    .size_full()
                    .child(render_grid(
                        &snap,
                        MONO_FONT,
                        cw,
                        ch,
                        snap.selection,
                        self.selection.hover_link_for(0),
                        images.as_deref(),
                        ime_preedit,
                    ))
                    .child(
                        // Paint-phase overlay: registers the IME input handler
                        // and the window-level mouse-selection listeners.
                        // Nothing is drawn here (paint calls from it don't
                        // reach the screen — engine-established fact).
                        canvas(
                            |_bounds, _window, _cx| (),
                            move |bounds, (), window, cx| {
                                window.handle_input(
                                    &focus_handle,
                                    ElementInputHandler::new(bounds, ime.clone()),
                                    cx,
                                );
                                selection::register_mouse_selection(
                                    window,
                                    view.clone(),
                                    bounds,
                                    0,
                                    cw,
                                    ch,
                                    grid_rows,
                                    grid_cols,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .into_any_element()
    }
}

fn main() {
    Application::new().run(|cx| {
        // The engine's renderer rides gpui-component primitives; initialise
        // its statics and pin the dark theme (the grid brings its own colors).
        gpui_component::init(cx);
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
        let bounds = Bounds::centered(None, size(px(1000.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("RikkaTerminal".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TermWindow::new(window, cx)),
        )
        .expect("open window");
    });
}
