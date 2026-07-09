//! RikkaTerminal — standalone tabbed terminal on rikka-terminal-core.
//!
//! Thin by design: local shells over ConPTY, engine sessions, one gpui window
//! per tab group. Tabs are window-independent sessions (see `hub`), so
//! detach/merge are synchronous Vec moves — the failure class where Windows
//! Terminal crashes (live-control migration racing output) cannot occur.
//!
//! Keys: Ctrl+Shift+T new tab / W close / D detach to a new window /
//! A merge every window into this one (M where the OS delivers it);
//! Ctrl+PageDown/PageUp (or Ctrl+Tab where delivered) cycles;
//! Ctrl+Shift+C/V (and Ctrl/Shift+Insert) copy/paste;
//! Shift+PageUp/PageDown pages the scrollback.

mod hub;

use std::io::{Read, Write};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, Application, Bounds, ClickEvent, Context, ElementInputHandler, Entity, FocusHandle,
    KeyDownEvent, ScrollDelta, ScrollWheelEvent, TitlebarOptions, Window, WindowBounds,
    WindowOptions, canvas, div, prelude::*, px, rgb, size,
};
use parking_lot::FairMutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use hub::TabEntry;
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
/// Tab strip height (logical px).
const TAB_STRIP_H: f32 = 30.0;
/// PTY-burst coalescing window (same rationale/value as shogun-desktop).
pub(crate) const FRAME_COALESCE: Duration = Duration::from_millis(8);

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

/// New shell session wrapped as a movable tab (driver task included).
fn create_tab(cx: &mut App) -> Option<TabEntry> {
    let session = shell_candidates()
        .iter()
        .find_map(|shell| spawn_local_shell(shell, 80, 24).ok())?;
    Some(hub::new_tab(cx, session))
}

// ── the tabbed window ────────────────────────────────────────────────────────

pub struct TabsWindow {
    tabs: Vec<TabEntry>,
    active: usize,
    terminal_focus: FocusHandle,
    ime: Entity<TerminalIme<Self>>,
    selection: SelectionState,
    /// Fractional wheel accumulators (trackpads deliver sub-line deltas).
    scroll_accum: f32,
    hwheel_accum: f32,
    /// Last PTY size applied to the ACTIVE tab (0 forces a re-apply, e.g.
    /// after a tab switch or an adoption from another window).
    cols: u16,
    rows: u16,
    /// Last OSC title applied to the OS window (dedup).
    applied_title: Option<String>,
}

impl ImeHost for TabsWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.active_session()
    }

    fn ime_font(&self) -> &str {
        MONO_FONT
    }
}

impl SelectionHost for TabsWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn pane_session(&self, _pane: usize) -> Option<&TerminalSession> {
        self.active_session()
    }
}

impl TabsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>, initial: Vec<TabEntry>) -> Self {
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
        let terminal_focus = cx.focus_handle();
        window.focus(&terminal_focus);
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            terminal_focus,
            ime,
            selection: SelectionState::default(),
            scroll_accum: 0.0,
            hwheel_accum: 0.0,
            cols: 0,
            rows: 0,
            applied_title: None,
        };
        for entry in initial {
            this.adopt(entry, cx);
        }
        this
    }

    fn active_session(&self) -> Option<&TerminalSession> {
        self.tabs.get(self.active).map(|e| &e.0.session)
    }

    /// Take ownership of a tab: point its driver's waker at this window and
    /// make it the active tab. This is the whole "attach" operation — the
    /// session itself never moves threads.
    fn adopt(&mut self, entry: TabEntry, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        self.tabs.push(entry);
        self.active = self.tabs.len() - 1;
        self.after_tab_change(cx);
    }

    fn switch_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            self.after_tab_change(cx);
        }
    }

    fn cycle(&mut self, forward: bool, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n > 1 {
            self.active = (self.active + if forward { 1 } else { n - 1 }) % n;
            self.after_tab_change(cx);
        }
    }

    fn after_tab_change(&mut self, cx: &mut Context<Self>) {
        // Force the viewport size onto the newly shown session (it may have
        // been sized by another window) and re-sync the OS title.
        self.cols = 0;
        self.rows = 0;
        self.applied_title = None;
        cx.notify();
    }

    /// Close the active tab; closing the last one closes the window.
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        let entry = self.tabs.remove(self.active);
        entry.0.shutdown();
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
    }

    /// Detach the active tab into a fresh window (no-op with a single tab —
    /// that would just be a window move).
    fn detach_active(&mut self, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        let entry = self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
        open_tabs_window(cx, vec![entry]);
    }

    /// Pull every other window's tabs into this one, then close them. Each
    /// step is a synchronous Vec move on this thread — mash it as hard as
    /// you like.
    fn merge_all(&mut self, cx: &mut Context<Self>) {
        let my_id = cx.entity_id();
        let others = hub::other_windows(cx, my_id);
        for (handle, weak) in others {
            let moved: Vec<TabEntry> = weak
                .upgrade()
                .map(|e| e.update(cx, |other, _| std::mem::take(&mut other.tabs)))
                .unwrap_or_default();
            for entry in moved {
                self.adopt(entry, cx);
            }
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn new_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = create_tab(cx) {
            self.adopt(entry, cx);
        }
    }
}

impl Render for TabsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // OSC 0/2 of the active tab → OS window title (deduped).
        if let Some(s) = self.active_session() {
            let title = s.title.lock().clone();
            if title != self.applied_title {
                window.set_window_title(title.as_deref().unwrap_or("RikkaTerminal"));
                self.applied_title = title;
            }
        }

        let (cw, ch) = measure_cell_metrics(&cx.text_system(), MONO_FONT, window.scale_factor());

        // Fit the ACTIVE tab's PTY to the pane (viewport minus strip/padding).
        let vp = window.viewport_size();
        let content_w = (vp.width / px(1.)) - PAD;
        let content_h = (vp.height / px(1.)) - PAD - TAB_STRIP_H;
        if content_w > cw && content_h > ch {
            let new_cols = ((content_w / cw) as u16).max(2);
            let new_rows = ((content_h / ch) as u16).max(2);
            if (new_cols, new_rows) != (self.cols, self.rows) {
                self.cols = new_cols;
                self.rows = new_rows;
                if let Some(s) = self.active_session() {
                    s.resize(new_cols, new_rows, (cw, ch));
                }
            }
        }

        // ── tab strip ────────────────────────────────────────────────────
        let strip = div()
            .w_full()
            .h(px(TAB_STRIP_H))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_1()
            .bg(rgb(0x101010))
            .children(self.tabs.iter().enumerate().map(|(ix, entry)| {
                let title = entry
                    .0
                    .session
                    .title
                    .lock()
                    .clone()
                    .unwrap_or_else(|| format!("シェル {}", ix + 1));
                let title: String = title.chars().take(24).collect();
                let active = ix == self.active;
                div()
                    .id(("tab", ix))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(if active { rgb(0x333333) } else { rgb(0x1B1B1B) })
                    .text_color(if active { rgb(0xE8DCC8) } else { rgb(0x8A8578) })
                    .text_size(px(12.))
                    .child(title)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                        this.switch_to(ix, cx);
                    }))
            }))
            .child(
                div()
                    .id("tab-new")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(0x1B1B1B))
                    .text_color(rgb(0x8A8578))
                    .text_size(px(12.))
                    .child("+")
                    .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                        this.new_tab(cx);
                    })),
            );

        // ── terminal pane (active tab) ───────────────────────────────────
        let pane = if let Some(snap) = self.active_session().map(|s| s.snapshot.lock().clone()) {
            let images = self.active_session().map(|s| Arc::clone(&s.images));
            let ime_preedit = self.ime.read(cx).marked.clone();
            let focus_handle = self.terminal_focus.clone();
            let ime = self.ime.clone();
            let view = cx.entity();
            let (grid_rows, grid_cols) = (snap.rows, snap.cols);
            div()
                .flex_1()
                .w_full()
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
                            // Paint-phase overlay: IME handler + selection
                            // listeners. Nothing draws here.
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
        } else {
            div()
                .flex_1()
                .text_color(rgb(0xE8DCC8))
                .child("シェルを起動できなかった (pwsh / cmd が見つからない)")
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x1A1A1A))
            .track_focus(&self.terminal_focus.clone())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let ks = &event.keystroke;
                let m = &ks.modifiers;
                // ── tab management chords ─────────────────────────────
                if m.control && m.shift {
                    let handled = match ks.key.as_str() {
                        "t" => {
                            this.new_tab(cx);
                            true
                        }
                        "w" => {
                            this.close_active(window, cx);
                            true
                        }
                        "d" => {
                            this.detach_active(cx);
                            true
                        }
                        // NOTE: gpui-Windows never delivers Ctrl+M (the
                        // ^M = CR legacy swallows it) — "a" (attach-all) is
                        // the real binding, "m" kept for platforms where it
                        // arrives.
                        "m" | "a" => {
                            this.merge_all(cx);
                            true
                        }
                        "c" => {
                            selection::copy_to_clipboard(
                                &this.selection,
                                this.active_session(),
                                cx,
                            );
                            true
                        }
                        "v" => {
                            if let Some(item) = cx.read_from_clipboard()
                                && let Some(text) = item.text()
                                && let Some(s) = this.active_session()
                            {
                                s.paste(&text);
                            }
                            true
                        }
                        "tab" => {
                            this.cycle(false, cx);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                        return;
                    }
                }
                if m.control && !m.shift && (ks.key == "tab" || ks.key == "pagedown") {
                    this.cycle(true, cx);
                    cx.stop_propagation();
                    return;
                }
                if m.control && !m.shift && ks.key == "pageup" {
                    this.cycle(false, cx);
                    cx.stop_propagation();
                    return;
                }
                if m.control && !m.shift && ks.key == "insert" {
                    selection::copy_to_clipboard(&this.selection, this.active_session(), cx);
                    cx.stop_propagation();
                    return;
                }
                if !m.control && m.shift && ks.key == "insert" {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                        && let Some(s) = this.active_session()
                    {
                        s.paste(&text);
                    }
                    cx.stop_propagation();
                    return;
                }
                // Shift+PageUp/PageDown: page through the scrollback.
                if m.shift && (ks.key == "pageup" || ks.key == "pagedown") {
                    if let Some(s) = this.active_session() {
                        let page = s.rows.load(Ordering::Relaxed).saturating_sub(1) as i32;
                        s.scroll_display(if ks.key == "pageup" { page } else { -page });
                    }
                    cx.stop_propagation();
                    return;
                }
                // Everything else through the engine's encoder. Printable
                // unmodified keys return None and keep propagating so WM_CHAR
                // feeds the IME input handler (otherwise chars would double).
                if let Some(s) = this.active_session() {
                    let mode = *s.term.lock().mode();
                    if let Some(bytes) = key_to_pty_bytes(ks, mode) {
                        s.send_bytes(&bytes);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _win, _cx| {
                    let Some(s) = this.active_session() else {
                        return;
                    };
                    let pad = PAD / 2.0;
                    let cols = s.cols.load(Ordering::Relaxed).max(1) as usize;
                    let rows = s.rows.load(Ordering::Relaxed).max(1) as usize;
                    let col = ((((event.position.x / px(1.)) - pad) / cw).max(0.0) as usize)
                        .min(cols - 1);
                    let row = ((((event.position.y / px(1.)) - pad - TAB_STRIP_H) / ch).max(0.0)
                        as usize)
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
                        let s = this.active_session().unwrap();
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
                        let s = this.active_session().unwrap();
                        s.hwheel_to_pty(whole_x, col, row, mods);
                    }
                }),
            )
            .child(strip)
            .child(pane)
    }
}

/// Open a tab-group window hosting `initial` and register it for merge-all
/// and release cleanup.
fn open_tabs_window(cx: &mut App, initial: Vec<TabEntry>) {
    let bounds = Bounds::centered(None, size(px(1000.), px(640.)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("RikkaTerminal".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TabsWindow::new(window, cx, initial)),
        )
        .expect("open window");
    let Ok(entity) = handle.update(cx, |_, _, cx| cx.entity()) else {
        return;
    };
    hub::register_window(cx, handle.into(), entity.downgrade());
    // Window closed (X or last-tab close): stop the surviving tabs' drivers;
    // the sessions drop with the entity and close their PTYs.
    cx.observe_release(&entity, |win: &mut TabsWindow, _| {
        for tab in &win.tabs {
            tab.0.shutdown();
        }
    })
    .detach();
}

fn main() {
    Application::new().run(|cx| {
        // The engine's renderer rides gpui-component primitives; initialise
        // its statics and pin the dark theme (the grid brings its own colors).
        gpui_component::init(cx);
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
        hub::init(cx);
        let initial = create_tab(cx).into_iter().collect();
        open_tabs_window(cx, initial);
    });
}
