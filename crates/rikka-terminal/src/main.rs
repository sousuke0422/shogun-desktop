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
//! Ctrl+Shift+L toggles session logging (● in the tab; see session_log);
//! Shift+PageUp/PageDown pages the scrollback.

// Release builds are GUI-subsystem: no console window tags along (and
// closing it can no longer kill the app with it). Debug builds keep the
// console for printf-style work; release diagnostics go to the panic log.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod attach;
mod cli;
mod config;
mod hub;
#[cfg(windows)]
mod pty_local;
mod session_log;
#[cfg(windows)]
mod tab_move;
mod tsf;
mod wt_profiles;

use std::io::{Read, Write};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, Application, Bounds, ClickEvent, Context, Entity, FocusHandle, KeyDownEvent, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use parking_lot::FairMutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use gpui_component::menu::ContextMenuExt as _;
use hub::TabEntry;
use rikka_terminal_core::ime::{ImeHost, TerminalIme};
use rikka_terminal_core::keys::key_to_pty_bytes;
use rikka_terminal_core::renderer::{measure_cell_metrics, render_grid};
use rikka_terminal_core::selection::{self, SelectionHost, SelectionState};
use rikka_terminal_core::{PtyResizer, ReportMods, TerminalSession, xtversion};
use rikka_terminal_ipc as ipc;

/// Default font: always present on Windows; CJK falls through DirectWrite's
/// system fallback. Bundled fonts are a P1 item.
const MONO_FONT: &str = "Consolas";
/// Horizontal pane padding (logical px); half on each side via `.px_1()`.
/// No vertical padding — the grid sits flush against the tab strip (a top
/// band of pane background reads as an ugly gap below the tabs).
const PAD: f32 = 8.0;
/// Tab strip height (logical px): WinUI TabView geometry — 8px breathing room
/// on top (TabViewHeaderPadding) + 32px tab zone (TabViewItemMinHeight).
const TAB_STRIP_H: f32 = 40.0;
const TAB_H: f32 = 32.0;
// ── chrome palette: Files (files.community) = WinUI TabView restyled ─────────
// Tokens lifted from TabView_themeresources.xaml / Common_themeresources_any
// (both MIT), dark theme — including the dark-gray surface ladder:
// SolidBackgroundFillColorBase for the window chrome and
// SolidBackgroundFillColorTertiary for the content layer, which is exactly
// the brush WinUI points TabViewItemHeaderBackgroundSelected at, so the
// selected tab merges with the pane by construction.
const CHROME_BG: u32 = 0x202020;
/// Pane surface = SolidBackgroundFillColorTertiary; the selected tab shares
/// it (the WinUI merge).
const PANE_BG: u32 = 0x282828;
/// LayerOnMicaBaseAltFillColorSecondary — unselected tab hover.
const TAB_HOVER: u32 = 0xFFFFFF0F;
/// SubtleFillColorSecondary — small button (close / add / caption) hover.
const SUBTLE_HOVER: u32 = 0xFFFFFF0F;
/// TextFillColorPrimary / TextFillColorSecondary.
const TEXT_PRIMARY: u32 = 0xFFFFFF;
const TEXT_SECONDARY: u32 = 0xFFFFFFC5;
/// DividerStrokeColorDefault — the 1px separators between unselected tabs.
const DIVIDER: u32 = 0xFFFFFF15;
/// PTY-burst coalescing window (same rationale/value as shogun-desktop).
pub(crate) const FRAME_COALESCE: Duration = Duration::from_millis(8);

gpui::actions!(rikka_terminal, [TerminalCopy, TerminalPaste]);

/// Appearance/terminal settings resolved once at startup from
/// `%APPDATA%/rikka-terminal/config.toml` (see `config.rs`).
static FONT_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static ACRYLIC_CFG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static SCROLLBACK_CFG: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

fn apply_appearance(cfg: &config::Config) {
    if let Some(size) = cfg.appearance.font_size {
        rikka_terminal_core::typography::set_font_size(size);
    }
    if let Some(mult) = cfg.appearance.line_height {
        rikka_terminal_core::typography::set_line_height(mult);
    }
    if let Some(font) = &cfg.appearance.font {
        let _ = FONT_OVERRIDE.set(font.clone());
    }
    let _ = ACRYLIC_CFG.set(cfg.appearance.acrylic.unwrap_or(false));
    let _ = SCROLLBACK_CFG.set(cfg.terminal.scrollback.map(|n| n as usize));
}

/// The grid font: configured, or the classic default.
fn mono_font() -> &'static str {
    FONT_OVERRIDE.get().map(String::as_str).unwrap_or(MONO_FONT)
}

/// Configured scrollback capacity, applied to every new session by
/// [`hub::new_tab`]. `None` = keep the engine default.
pub(crate) fn configured_scrollback() -> Option<usize> {
    SCROLLBACK_CFG.get().copied().flatten()
}

/// `[appearance] acrylic = true` (or RIKKA_ACRYLIC=1) → system acrylic
/// blur behind the window, with the chrome and pane surround going
/// translucent. The grid itself stays opaque (the engine paints cell
/// backgrounds) — blur belongs to the chrome, not under the text.
fn acrylic() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("RIKKA_ACRYLIC").is_ok_and(|v| v != "0")
            || ACRYLIC_CFG.get().copied().unwrap_or(false)
    })
}

/// Strip fill: solid chrome, or a 72% tint over acrylic.
fn chrome_fill() -> gpui::Rgba {
    if acrylic() {
        gpui::rgba(0x202020B8)
    } else {
        rgb(CHROME_BG)
    }
}

/// Pane surround fill (also the selected tab, which merges with it): solid,
/// or a 78% tint over acrylic.
fn pane_fill() -> gpui::Rgba {
    if acrylic() {
        gpui::rgba(0x282828C8)
    } else {
        rgb(PANE_BG)
    }
}

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

/// Spawn `program args…` on a local PTY and wire it into an engine session.
///
/// Windows first tries the handoff-shaped direct ConPTY drive (pty_local) —
/// the session then owns the same handle set an OS handoff delivers, which
/// is what cross-window tab moves ride. portable-pty stays as the fallback
/// (and the `RIKKA_LEGACY_PTY` escape hatch) so a missing/mismatched
/// sideload pair degrades to the old path instead of a dead tab.
fn spawn_local_shell(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
) -> Result<TerminalSession> {
    #[cfg(windows)]
    if std::env::var_os("RIKKA_LEGACY_PTY").is_none() {
        match pty_local::spawn_local(program, args, cwd, cols, rows) {
            Ok(session) => return Ok(session),
            Err(e) => {
                log::warn!("handoff-shaped local spawn failed, using portable-pty: {e:#}");
            }
        }
    }
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    cmd.env("TERM_PROGRAM", xtversion::TERM_PROGRAM);
    cmd.env("TERM_PROGRAM_VERSION", xtversion::TERM_PROGRAM_VERSION);
    let _child = pair.slave.spawn_command(cmd)?;
    let writer: Box<dyn Write + Send> = pair.master.take_writer()?;
    let reader: Box<dyn Read + Send> = Box::new(pair.master.try_clone_reader()?);
    let resizer: Arc<dyn PtyResizer> = Arc::new(LocalResizer {
        master: FairMutex::new(SendMaster(pair.master)),
    });
    let session = rikka_terminal_core::pty_session::build_terminal_session(
        cols,
        rows,
        reader,
        Arc::new(FairMutex::new(writer)),
        resizer,
        &xtversion::engine_identity(),
    )?;
    // portable-pty on Windows is ConPTY underneath — same conhost reflow
    // and no kitty-keyboard advertisement (mark_conpty docs).
    #[cfg(windows)]
    session.mark_conpty();
    Ok(session)
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

/// A wt profile as a CLI spec: its resolved argv replaces the shell, its
/// name seeds the tab title (until the app's OSC 0/2 overrides it).
fn profile_to_spec(p: &wt_profiles::WtProfile) -> cli::TabSpec {
    cli::TabSpec {
        profile: None,
        dir: p.dir.clone(),
        title: Some(p.name.clone()),
        cmdline: p.argv.clone(),
        hold: false,
    }
}

/// The spec a plain new-tab opens: the configured default wt profile, or the
/// built-in shell search when no profile menu is active.
fn default_spec(cx: &App) -> cli::TabSpec {
    let menu = &cx.global::<hub::ProfileMenu>().0;
    match menu.default.and_then(|i| menu.profiles.get(i)) {
        Some(p) => profile_to_spec(p),
        None => cli::TabSpec::default(),
    }
}

/// New shell session wrapped as a movable tab (driver task included).
fn create_tab(cx: &mut App) -> Option<TabEntry> {
    let spec = default_spec(cx);
    create_tab_spec(cx, &spec)
}

/// Tab from a CLI spec (wt semantics): an explicit commandline replaces the
/// shell entirely; `-p` narrows the shell to one candidate; `--title` seeds
/// the tab title until the application's OSC 0/2 takes over.
fn create_tab_spec(cx: &mut App, spec: &cli::TabSpec) -> Option<TabEntry> {
    let candidates: Vec<String> = if !spec.cmdline.is_empty() {
        vec![spec.cmdline[0].clone()]
    } else if let Some(p) = &spec.profile {
        vec![p.clone()]
    } else {
        shell_candidates()
    };
    let args: &[String] = if spec.cmdline.len() > 1 {
        &spec.cmdline[1..]
    } else {
        &[]
    };
    let session = candidates
        .iter()
        .find_map(|program| spawn_local_shell(program, args, spec.dir.as_deref(), 80, 24).ok())?;
    if let Some(t) = &spec.title {
        *session.title.lock() = Some(t.clone());
    }
    Some(hub::new_tab(cx, session))
}

// ── the tabbed window ────────────────────────────────────────────────────────

/// The OTHER process owning the top-level window under the mouse cursor —
/// the drag-merge target probe, called on a release outside our window.
/// `None`: nothing there, or it is ours. Whether that pid is actually a
/// rikka window is settled by `resolve_window` (unknown pids fail).
#[cfg(windows)]
fn other_process_under_cursor() -> Option<(u32, (i32, i32))> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetCursorPos, GetWindowThreadProcessId, WindowFromPoint,
    };
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    let hwnd = unsafe { WindowFromPoint(pt) };
    if hwnd.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(root, Some(&mut pid)) };
    (pid != 0 && pid != std::process::id()).then_some((pid, (pt.x, pt.y)))
}

/// Strip content-x → insertion index: the gap nearest the drop point.
/// Dropping on a tab's left half inserts before it, right half after;
/// anything past the last tab appends.
fn strip_insert_index(content_x: f32, tab_w: f32, len: usize) -> usize {
    if tab_w <= 0.0 {
        return len;
    }
    (((content_x / tab_w) + 0.5).floor().max(0.0) as usize).min(len)
}

/// Drag payload of a tab being moved: its strip index (`title` rides along
/// for the ghost). Dropping on another tab reorders; dropping on the pane
/// detaches into a fresh window.
#[derive(Clone)]
struct TabDrag {
    ix: usize,
    title: String,
}

/// The floating preview under the pointer while a tab is dragged.
struct TabDragGhost {
    title: String,
}

impl Render for TabDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(TAB_H))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .bg(pane_fill())
            .border_1()
            .border_color(gpui::rgba(DIVIDER))
            .text_size(px(12.))
            .text_color(rgb(TEXT_PRIMARY))
            .child(self.title.clone())
    }
}

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
    /// The new-tab profile dropdown is open (rendered below the strip).
    profile_menu: bool,
    /// Horizontal scroll of the tab viewport (Firefox-style overflow: when
    /// tabs would push the caption buttons, they scroll here instead).
    strip_scroll: ScrollHandle,
    /// The tab index riding an active drag — the tear-off handler needs it
    /// on a mouse up OUTSIDE the window, where the hitbox-based drop
    /// system cannot see anything. Cleared by every in-window resolution
    /// (the drop handlers and the root mouse-up).
    dragging_tab: Option<usize>,
}

impl ImeHost for TabsWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.active_session()
    }

    fn ime_font(&self) -> &str {
        mono_font()
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
        // TSF (always on — see `tsf`): bind the store while our terminal
        // input owns focus. The waker only schedules a notify; render then
        // drains the queued IME events.
        let tsf_view = cx.weak_entity();
        let tsf_async = cx.to_async();
        window
            .on_focus_in(&terminal_focus, cx, move |_, _| {
                let view = tsf_view.clone();
                let async_cx = tsf_async.clone();
                tsf::on_input_focus(Box::new(move || {
                    let view = view.clone();
                    async_cx
                        .spawn(async move |cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                }));
            })
            .detach();
        window
            .on_focus_out(&terminal_focus, cx, |_, _, _| tsf::on_input_blur())
            .detach();
        // Re-assert TSF focus once the OS window is FOREGROUND. The store's
        // SetFocus must run while our window is active for the active TIP to
        // bind the taskbar IME indicator to us and for stricter IMEs (Google
        // 日本語入力) to engage — basic composition works even bound in the
        // wrong context, but the indicator and those IMEs do not. The
        // window.focus() above fires on_focus_in during construction, before
        // the window is foreground, so this activation hook corrects it (and
        // re-binds after an alt-tab away and back).
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && this.terminal_focus.is_focused(window) {
                let view = cx.weak_entity();
                let async_cx = cx.to_async();
                tsf::on_input_focus(Box::new(move || {
                    let view = view.clone();
                    async_cx
                        .spawn(async move |cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                }));
            }
        })
        .detach();
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
            profile_menu: false,
            strip_scroll: ScrollHandle::default(),
            dragging_tab: None,
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
        self.adopt_at(entry, None, cx);
    }

    /// [`Self::adopt`] at a strip position; `None`/out-of-range appends.
    fn adopt_at(&mut self, entry: TabEntry, ix: Option<usize>, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        let ix = ix.unwrap_or(usize::MAX).min(self.tabs.len());
        self.tabs.insert(ix, entry);
        self.active = ix;
        self.after_tab_change(cx);
    }

    /// [`Self::adopt`], but a drag-merge lands at the strip position under
    /// the drop point instead of always appending. The geometry runs on
    /// Win32 — the adopt path arrives from the IPC pump with no
    /// `&mut Window` — recreating `render`'s strip math from the client
    /// rect and DPI of the window under the point. Anything that doesn't
    /// line up (point over a different window of this process, stale
    /// coordinates) falls back to append.
    #[cfg(windows)]
    fn adopt_dropped(
        &mut self,
        entry: TabEntry,
        drop_at: Option<(i32, i32)>,
        cx: &mut Context<Self>,
    ) {
        let ix = drop_at.and_then(|pt| self.drop_index_at(pt, cx));
        self.adopt_at(entry, ix, cx);
    }

    /// Map a screen-pixel drop point to the tab-strip insertion index —
    /// `render`'s layout math (equal-width tabs, WinUI paddings) replayed
    /// against Win32 window metrics. `None` = not on this window, append.
    #[cfg(windows)]
    fn drop_index_at(&self, pt: (i32, i32), cx: &App) -> Option<usize> {
        use windows::Win32::Foundation::{POINT, RECT};
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetClientRect, GetWindowThreadProcessId, WindowFromPoint,
        };
        let mut p = POINT { x: pt.0, y: pt.1 };
        let hwnd = unsafe { WindowFromPoint(p) };
        if hwnd.0.is_null() {
            return None;
        }
        let hwnd = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // A window process hosts one window, so ours-by-pid is ours. (An
        // in-process second window would need per-window addressing first —
        // see TODO "窓単位 addressing".)
        if pid != std::process::id() {
            return None;
        }
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rc) }.ok()?;
        if !unsafe { ScreenToClient(hwnd, &mut p) }.as_bool() {
            return None;
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let width = (rc.right - rc.left) as f32 / scale;
        let x = p.x as f32 / scale;
        // Mirror render(): avail / plus_w / needs_scroll / tab_w, then the
        // strip origin (pl_2 = 8, plus the left arrow when scrolling).
        let profiles = cx.global::<hub::ProfileMenu>().0.profiles.len();
        let plus_w = 32.0 + if profiles > 1 { 18.0 } else { 0.0 };
        let avail = width - 8.0 - (3.0 * 46.0) - (2.0 * 24.0);
        let needs_scroll = (self.tabs.len() as f32 * 100.0 + plus_w) > avail;
        let tab_w = ((avail - plus_w) / self.tabs.len().max(1) as f32).clamp(100.0, 240.0);
        let origin = 8.0 + if needs_scroll { 22.0 } else { 0.0 };
        let content_x = x - origin - (self.strip_scroll.offset().x / px(1.));
        Some(strip_insert_index(content_x, tab_w, self.tabs.len()))
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
        self.close_at(self.active, window, cx);
    }

    /// Close the tab at `ix` (tab-strip ✕); closing the last one closes the
    /// window.
    fn close_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            if self.tabs.is_empty() {
                window.remove_window();
            }
            return;
        }
        let entry = self.tabs.remove(ix);
        entry.0.shutdown();
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
    }

    /// Detach the active tab into a fresh window (no-op with a single tab —
    /// that would just be a window move).
    fn detach_active(&mut self, cx: &mut Context<Self>) {
        self.split_off_in_process(self.active, cx);
    }

    /// The in-process half of a detach: move the tab's Arc into a fresh
    /// window of THIS process (crash fate shared, scrollback kept intact).
    fn split_off_in_process(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 || ix >= self.tabs.len() {
            return;
        }
        let entry = self.tabs.remove(ix);
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
        open_tabs_window(cx, vec![entry]);
    }

    /// Detach the tab at `ix` into a fresh window, preferring full crash
    /// isolation (its own OS process) when the session can leave this one;
    /// a non-transferable session (legacy PTY) splits in-process instead,
    /// which never risks it. Single tab = a window move, all cost no gain —
    /// no-op. Drag-to-pane and Ctrl+Shift+E both land here.
    fn detach_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.dragging_tab = None;
        if self.tabs.len() < 2 || ix >= self.tabs.len() {
            return;
        }
        #[cfg(windows)]
        {
            let entry = &self.tabs[ix];
            if tab_move::is_transferable(&entry.0.session) {
                match tab_move::send_tab(&entry.0.session, tab_move::Destination::NewProcess) {
                    Ok(()) => self.close_at(ix, window, cx),
                    // Quiesce is irreversible — the tab stays, honestly
                    // disconnected; splitting it off would just relocate
                    // the corpse.
                    Err(e) => {
                        log::warn!("cross-process detach failed: {e:#}");
                        cx.notify();
                    }
                }
                return;
            }
        }
        let _ = window;
        self.split_off_in_process(ix, cx);
    }

    /// Reorder: the dragged tab leaves `from` and lands at the strip
    /// position of the drop target. The moved tab stays the user's focus.
    fn reorder_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.dragging_tab = None;
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let entry = self.tabs.remove(from);
        self.tabs.insert(to, entry);
        self.active = to;
        self.after_tab_change(cx);
    }

    /// Cross-process detach of the active tab (Ctrl+Shift+E) — see
    /// [`Self::detach_at`].
    #[cfg(windows)]
    fn eject_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detach_at(self.active, window, cx);
    }

    /// A tab released over ANOTHER rikka window (the drag-merge gesture):
    /// move it there via that window's own socket. Works from a single-tab
    /// window too — that is a window merge, and moving the last tab closes
    /// this one. Returns false when `pid` is not a reachable rikka window
    /// (the caller falls back to the tear-off); a non-transferable session
    /// (legacy PTY — an in-process Arc cannot cross processes) cancels.
    #[cfg(windows)]
    fn drop_tab_on_window_process(
        &mut self,
        ix: usize,
        pid: u32,
        drop_at: (i32, i32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(entry) = self.tabs.get(ix) else {
            return true;
        };
        if !tab_move::is_transferable(&entry.0.session) {
            log::warn!("drag-merge: session is not transferable (legacy PTY) — cancelled");
            return true;
        }
        let Ok(endpoint) = tab_move::resolve_window(u64::from(pid)) else {
            return false; // not a (reachable) rikka window
        };
        match tab_move::send_tab(
            &entry.0.session,
            tab_move::Destination::Window {
                id: u64::from(pid),
                endpoint,
                drop_at: Some(drop_at),
            },
        ) {
            Ok(()) => self.close_at(ix, window, cx),
            // connect-first ordering means the tab usually survives this.
            Err(e) => {
                log::warn!("drag-merge failed: {e:#}");
                cx.notify();
            }
        }
        true
    }

    /// Move the active tab into another window PROCESS (resolve through the
    /// monarch, attach on that window's own socket). Moving the last tab
    /// closes this window — that is a window merge, and the point.
    #[cfg(windows)]
    fn move_active_to_other_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.tabs.get(self.active) else {
            return;
        };
        match tab_move::move_to_any_other_window(&entry.0.session) {
            Ok(()) => self.close_at(self.active, window, cx),
            Err(e) => {
                log::warn!("tab move failed: {e:#}");
                cx.notify();
            }
        }
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
        self.profile_menu = false;
    }

    /// Open a new tab from the profile at `idx` in the shared menu.
    fn new_tab_profile(&mut self, idx: usize, cx: &mut Context<Self>) {
        let spec = cx
            .global::<hub::ProfileMenu>()
            .0
            .profiles
            .get(idx)
            .map(profile_to_spec);
        if let Some(spec) = spec
            && let Some(entry) = create_tab_spec(cx, &spec)
        {
            self.adopt(entry, cx);
        }
        self.profile_menu = false;
    }
}

/// Native caption button (min/max/close), Files-style. The
/// `window_control_area` hitbox makes WM_NCHITTEST return
/// HTMINBUTTON/HTMAXBUTTON/HTCLOSE and gpui's NC handlers run the native
/// action (ShowWindowAsync / WM_CLOSE) — no click listener wanted, or the
/// strip would have to reimplement press/release semantics.
fn caption_button(glyph: &'static str, area: WindowControlArea) -> impl IntoElement {
    let close = matches!(area, WindowControlArea::Close);
    div()
        .w(px(46.))
        .h_full()
        // Never shrink: the caption group stays pinned right and full-size no
        // matter how many tabs crowd the strip (they scroll instead).
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        // Segoe MDL2 Assets ships on Windows 10+; its E92x/E8BB glyphs are
        // the system caption icons (what Files/wt render).
        .font_family("Segoe MDL2 Assets")
        .text_size(px(10.))
        .text_color(gpui::rgba(TEXT_SECONDARY))
        .hover(move |t| {
            if close {
                // The one non-monochrome hover in the chrome: the standard
                // Windows close red.
                t.bg(gpui::rgba(0xC42B1CFF)).text_color(rgb(TEXT_PRIMARY))
            } else {
                t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY))
            }
        })
        .window_control_area(area)
        .child(glyph)
}

impl Render for TabsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // TSF: apply queued IME events while our terminal input owns focus —
        // preedit renders inline via ime.marked, commits go to the active
        // tab's PTY.
        if self.terminal_focus.is_focused(window) {
            for ev in tsf::drain() {
                match ev {
                    rikka_terminal_gpui_ime::ImeEvent::Preedit(s) => {
                        let marked = (!s.is_empty()).then_some(s);
                        self.ime.update(cx, |ime, cx| {
                            ime.marked = marked;
                            cx.notify();
                        });
                    }
                    rikka_terminal_gpui_ime::ImeEvent::Commit(s) => {
                        if let Some(session) = self.active_session() {
                            session.send_bytes(s.as_bytes());
                        }
                        self.ime.update(cx, |ime, cx| {
                            ime.marked = None;
                            cx.notify();
                        });
                    }
                }
            }
        }

        // OSC 0/2 of the active tab → OS window title (deduped).
        if let Some(s) = self.active_session() {
            let title = s.title.lock().clone();
            if title != self.applied_title {
                window.set_window_title(title.as_deref().unwrap_or("RikkaTerminal"));
                self.applied_title = title;
            }
        }

        let (cw, ch) = measure_cell_metrics(&cx.text_system(), mono_font(), window.scale_factor());

        // Fit the ACTIVE tab's PTY to the pane (viewport minus strip/padding).
        let vp = window.viewport_size();
        let content_w = (vp.width / px(1.)) - PAD;
        let content_h = (vp.height / px(1.)) - TAB_STRIP_H;
        if content_w > cw && content_h > ch {
            let new_cols = ((content_w / cw) as u16).max(2);
            let new_rows = ((content_h / ch) as u16).max(2);
            if (new_cols, new_rows) != (self.cols, self.rows) {
                self.cols = new_cols;
                self.rows = new_rows;
                // Every tab, not just the active one: the guard above is
                // window state, so a background tab that misses this moment
                // would never be re-fit — by the time it's activated the
                // dims already "match" and it stays at its stale PTY size
                // (wrong wrap column) until the next window resize.
                for entry in &self.tabs {
                    entry.0.session.resize(new_cols, new_rows, (cw, ch));
                }
            }
        }

        // ── tab strip = the titlebar (appears_transparent hides the native
        // one; Files integrates tabs the same way). Geometry and states are
        // WinUI TabView's: 32px tabs bottom-aligned under 8px of breathing
        // room, top-rounded 8px, the selected tab merging seamlessly into
        // the pane surface below (no accent, no bottom border), and 1px
        // separators between unselected neighbors. ─────────────────────────
        let maximized = window.is_maximized();
        let active_ix = self.active;
        // New-tab profile menu (wt profiles, config-filtered). >1 profile
        // shows the dropdown chevron next to [+]; the list is rendered below
        // the strip when open.
        let profiles: Vec<(usize, String)> = cx
            .global::<hub::ProfileMenu>()
            .0
            .profiles
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.name.clone()))
            .collect();
        let has_profile_menu = profiles.len() > 1;
        // Firefox-style tab overflow: tabs shrink to a 100px floor, then
        // scroll (caption buttons stay pinned) once even the floored tabs
        // plus [+]/⌄ can't fit left of the caption group and the arrows.
        let plus_w = 32.0 + if has_profile_menu { 18.0 } else { 0.0 };
        let avail = (vp.width / px(1.)) - 8.0 - (3.0 * 46.0) - (2.0 * 24.0);
        let needs_scroll = (self.tabs.len() as f32 * 100.0 + plus_w) > avail;
        // WinUI TabView (Files/wt) sizing: EQUAL widths regardless of the
        // title — the standard 240px, shrinking uniformly to the 100px
        // floor as tabs crowd the strip, then they scroll. Computed here
        // instead of leaning on flex shrink: a (potential) scroll
        // container measures its children against infinite space, so
        // taffy would never shrink them.
        let tab_w = ((avail - plus_w) / self.tabs.len().max(1) as f32).clamp(100.0, 240.0);
        let tab_viewport = div()
            .id("tab-viewport")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_end()
            .map(|v| {
                if needs_scroll {
                    v.overflow_x_scroll().track_scroll(&self.strip_scroll)
                } else {
                    v.overflow_hidden()
                }
            })
            // A drop on the strip's empty space (past the last tab) moves
            // the dragged tab to the end — drops ON a tab are consumed by
            // that tab's own handler and never bubble here.
            .on_drop(cx.listener(|this, drag: &TabDrag, _window, cx| {
                let last = this.tabs.len().saturating_sub(1);
                this.reorder_tab(drag.ix, last, cx);
            }))
            .children(self.tabs.iter().enumerate().flat_map(|(ix, entry)| {
                let title = entry
                    .0
                    .session
                    .title
                    .lock()
                    .clone()
                    .unwrap_or_else(|| format!("シェル {}", ix + 1));
                let title: String = title.chars().take(20).collect();
                let drag_title = title.clone();
                let active = ix == active_ix;
                // Recording indicator: session logging is on (Ctrl+Shift+L).
                let rec_dot = entry.0.session.logging_active().then(|| {
                    div()
                        .mr(px(4.))
                        .flex_shrink_0()
                        .text_color(rgb(0xE81123))
                        .child("●")
                });
                // Separator to the left of this tab — hidden next to the
                // selected tab, whose silhouette does the separating.
                let sep = (ix > 0 && !active && ix - 1 != active_ix).then(|| {
                    div()
                        .w(px(1.))
                        .h(px(16.))
                        .mb(px((TAB_H - 16.0) / 2.0))
                        .bg(gpui::rgba(DIVIDER))
                        .into_any_element()
                });
                let tab = div()
                    .id(("tab", ix))
                    .h(px(TAB_H))
                    // Equal WinUI TabView width — see `tab_w` above.
                    .w(px(tab_w))
                    .flex_shrink_0()
                    .pl(px(8.))
                    .pr(px(4.))
                    .py(px(3.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded_tl(px(8.))
                    .rounded_tr(px(8.))
                    .text_size(px(12.))
                    .map(|t| {
                        if active {
                            t.bg(pane_fill()).text_color(rgb(TEXT_PRIMARY))
                        } else {
                            t.text_color(gpui::rgba(TEXT_SECONDARY))
                                .hover(|t| t.bg(gpui::rgba(TAB_HOVER)))
                        }
                    })
                    .children(rec_dot)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .child(
                        // WinUI tab close: 32×24 hit target, ControlCornerRadius,
                        // E711 glyph at 12px.
                        div()
                            .id(("tab-close", ix))
                            .w(px(32.))
                            .h(px(24.))
                            .ml(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .font_family("Segoe MDL2 Assets")
                            .text_size(px(12.))
                            .text_color(gpui::rgba(TEXT_SECONDARY))
                            .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                            .child("\u{E711}")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.close_at(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                        this.switch_to(ix, cx);
                    }))
                    // Tab DnD: drop on a tab = reorder there; drop on the
                    // pane below = detach into a fresh window (see the pane).
                    .on_drag(
                        TabDrag {
                            ix,
                            title: drag_title,
                        },
                        |drag, _offset, _window, cx| {
                            let title = drag.title.clone();
                            cx.new(|_| TabDragGhost { title })
                        },
                    )
                    .drag_over::<TabDrag>(|style, _, _, _| style.bg(gpui::rgba(TAB_HOVER)))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _window, cx| {
                        this.reorder_tab(drag.ix, ix, cx);
                    }))
                    .into_any_element();
                sep.into_iter().chain(std::iter::once(tab))
            }))
            .child(
                // WinUI add-tab button: 32×24, E710 at 12px, centered in the
                // tab zone.
                div()
                    .id("tab-new")
                    .w(px(32.))
                    .h(px(24.))
                    .ml(px(4.))
                    .mb(px((TAB_H - 24.0) / 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .font_family("Segoe MDL2 Assets")
                    .text_size(px(12.))
                    .text_color(gpui::rgba(TEXT_SECONDARY))
                    .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                    .child("\u{E710}")
                    .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                        this.new_tab(cx);
                    })),
            )
            .when(has_profile_menu, |strip| {
                // ChevronDown next to [+]: opens the profile list (wt-style
                // split new-tab button).
                strip.child(
                    div()
                        .id("tab-new-menu")
                        .w(px(18.))
                        .h(px(24.))
                        // Breathing room from [+]: they are distinct actions
                        // (new default tab vs. pick a profile), not a merged
                        // split button.
                        .ml(px(6.))
                        .mb(px((TAB_H - 24.0) / 2.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .font_family("Segoe MDL2 Assets")
                        .text_size(px(8.))
                        .text_color(gpui::rgba(TEXT_SECONDARY))
                        .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                        .child("\u{E70D}")
                        .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                            this.profile_menu = !this.profile_menu;
                            cx.notify();
                        })),
                )
            })
            .child(
                // Trailing drag filler INSIDE the viewport: draggable empty
                // space when tabs are few; collapses to zero (and the tabs
                // scroll) when they overflow. HTCAPTION buys native drag,
                // double-click maximize, snap and the system menu.
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            );

        // ChevronLeft / ChevronRight, shown only when the tabs overflow. Each
        // click nudges the viewport by ~2 tabs, clamped to the scroll range.
        let scroll_arrow = |id: &'static str, glyph: &'static str, dir: f32| {
            div()
                .id(id)
                .flex_shrink_0()
                .w(px(22.))
                .h(px(24.))
                .mb(px((TAB_H - 24.0) / 2.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .font_family("Segoe MDL2 Assets")
                .text_size(px(10.))
                .text_color(gpui::rgba(TEXT_SECONDARY))
                .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                .child(glyph)
                .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                    let cur = this.strip_scroll.offset().x / px(1.);
                    let maxw = this.strip_scroll.max_offset().width / px(1.);
                    let nx = (cur + dir * 220.0).clamp(-maxw, 0.0);
                    this.strip_scroll.set_offset(point(px(nx), px(0.)));
                    cx.notify();
                }))
        };

        let strip = div()
            .w_full()
            .h(px(TAB_STRIP_H))
            .flex()
            .flex_row()
            .items_end()
            .pl_2()
            .bg(chrome_fill())
            // Wheel anywhere over the strip scrolls the tabs horizontally
            // (both axes fold into horizontal, browser-style), and never
            // reaches the terminal below. No-op when the tabs already fit.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _win, cx| {
                let maxw = this.strip_scroll.max_offset().width / px(1.);
                if maxw <= 0.0 {
                    return;
                }
                let step = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.x) + f32::from(p.y),
                    ScrollDelta::Lines(l) => (l.x + l.y) * 40.0,
                };
                if step == 0.0 {
                    return;
                }
                let cur = this.strip_scroll.offset().x / px(1.);
                let nx = (cur + step).clamp(-maxw, 0.0);
                this.strip_scroll.set_offset(point(px(nx), px(0.)));
                cx.stop_propagation();
                cx.notify();
            }))
            .when(needs_scroll, |s| {
                s.child(scroll_arrow("tab-scroll-left", "\u{E76B}", 1.0))
            })
            .child(tab_viewport)
            .when(needs_scroll, |s| {
                s.child(scroll_arrow("tab-scroll-right", "\u{E76C}", -1.0))
            })
            .child(caption_button("\u{E921}", WindowControlArea::Min))
            .child(caption_button(
                if maximized { "\u{E923}" } else { "\u{E922}" },
                WindowControlArea::Max,
            ))
            .child(caption_button("\u{E8BB}", WindowControlArea::Close));

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
                .px_1()
                // Focus-on-click lives on the PANE, not the window root: gpui's
                // focus listener calls prevent_default on every mouse down over
                // a focusable hitbox, and gpui-Windows reads that as "the app
                // consumed this click" — which would swallow the caption
                // buttons' and drag area's non-client handling up in the strip.
                .track_focus(&self.terminal_focus.clone())
                // Dropping a tab on the pane detaches it into a fresh window
                // (its own OS process when the session is transferable) —
                // the "tear a tab off" gesture without OS-level DnD. The
                // dashed outline advertises it while a tab hovers.
                .drag_over::<TabDrag>(|style, _, _, _| {
                    style
                        .border_2()
                        .border_dashed()
                        .border_color(gpui::rgba(0x8A9CC880))
                })
                .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                    this.detach_at(drag.ix, window, cx);
                }))
                .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                    selection::copy_to_clipboard(&this.selection, this.active_session(), cx);
                }))
                .on_action(cx.listener(|this, _: &TerminalPaste, _window, cx| {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                        && let Some(s) = this.active_session()
                    {
                        s.paste(&text);
                    }
                }))
                .child(
                    div()
                        .relative()
                        .size_full()
                        .child(render_grid(
                            &snap,
                            mono_font(),
                            cw,
                            ch,
                            snap.selection,
                            self.selection.hover_link_for(0),
                            images.as_deref(),
                            ime_preedit,
                        ))
                        // Shared pane overlay (IME handler + selection
                        // listeners + caret). Single-sourced in the engine so
                        // shogun-desktop and rikka hit-test identically. The
                        // wrapper above is already the grid's content box, so
                        // the overlay pins flush (inset 0); the PTY resize is
                        // driven from the viewport, so no size sink is needed.
                        .child(rikka_terminal_core::pane::pane_overlay(
                            rikka_terminal_core::pane::PaneOverlay {
                                focus_handle,
                                ime,
                                view,
                                pane: 0,
                                cw,
                                ch,
                                grid_rows,
                                grid_cols,
                                inset: 0.0,
                                caret_enabled: true,
                                measured: None,
                            },
                            // Pipe the caret rect to TSF so the IME candidate
                            // window opens at the terminal cursor.
                            move |caret| {
                                tsf::set_caret(caret.map(|(left, top, right, bottom)| {
                                    rikka_terminal_gpui_ime::CaretRect {
                                        left,
                                        top,
                                        right,
                                        bottom,
                                    }
                                }));
                            },
                        )),
                )
                // Right-click menu, same actions as the Ctrl+Shift chords.
                // Attached to the pane — a plain flex div, NEVER a scroll
                // container: the open menu injects a window-sized absolute
                // subtree as a child, and taffy counts absolute children
                // toward a scroll container's content size, which scrolled
                // the grid clean out of view in shogun-desktop (the
                // "right-click blanks the alt screen" bug, fixed 2026-07-10).
                .context_menu({
                    let menu_focus = self.terminal_focus.clone();
                    move |menu, _window, _cx| {
                        menu.action_context(menu_focus.clone())
                            .menu("コピー", Box::new(TerminalCopy))
                            .menu("ペースト", Box::new(TerminalPaste))
                    }
                })
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
            .bg(pane_fill())
            // ── tab tear-off ─────────────────────────────────────────────
            // Track which tab rides the active drag; on a mouse up OUTSIDE
            // the window (SetCapture routes it here) no drop target can
            // fire — that release IS the browser-style tear-off gesture,
            // so detach the tab into its own window. An in-window release
            // that no drop target consumed just cancels.
            .on_drag_move::<TabDrag>(cx.listener(
                |this, ev: &gpui::DragMoveEvent<TabDrag>, _w, cx| {
                    this.dragging_tab = Some(ev.drag(cx).ix);
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev, _w, _cx| {
                    this.dragging_tab = None;
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev: &gpui::MouseUpEvent, window, cx| {
                    let Some(ix) = this.dragging_tab.take() else {
                        return;
                    };
                    // Released over another rikka window = drag-merge (the
                    // tab moves there, even from a single-tab window);
                    // anywhere else = tear-off into a fresh window.
                    #[cfg(windows)]
                    if let Some((pid, drop_at)) = other_process_under_cursor()
                        && this.drop_tab_on_window_process(ix, pid, drop_at, window, cx)
                    {
                        return;
                    }
                    this.detach_at(ix, window, cx);
                }),
            )
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
                        // Cross-process moves (Windows; IPC.md tab moves):
                        // E(ject) = detach into an own OS process,
                        // X(fer) = move into another window process.
                        #[cfg(windows)]
                        "e" => {
                            this.eject_active(window, cx);
                            true
                        }
                        #[cfg(windows)]
                        "x" => {
                            this.move_active_to_other_window(window, cx);
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
                        // Session logging toggle — the ● in the tab is the
                        // feedback, so redraw right away.
                        "l" => {
                            if let Some(s) = this.active_session() {
                                session_log::toggle(s);
                            }
                            cx.notify();
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
                    let row = ((((event.position.y / px(1.)) - TAB_STRIP_H) / ch).max(0.0)
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
            .when(self.profile_menu, |root| {
                // Click-away scrim behind the list; a click anywhere else
                // closes the menu. Rendered before the list so the list wins
                // the overlap.
                root.child(
                    div()
                        .id("profile-scrim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                            this.profile_menu = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(TAB_STRIP_H))
                        .left(px(8.))
                        .flex()
                        .flex_col()
                        .min_w(px(200.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(rgb(CHROME_BG))
                        .border_1()
                        .border_color(gpui::rgba(DIVIDER))
                        .children(profiles.into_iter().map(|(idx, name)| {
                            div()
                                .id(("profile", idx))
                                .px(px(12.))
                                .py(px(6.))
                                .text_size(px(13.))
                                .text_color(gpui::rgba(TEXT_SECONDARY))
                                .hover(|t| {
                                    t.bg(gpui::rgba(TAB_HOVER)).text_color(rgb(TEXT_PRIMARY))
                                })
                                .child(name)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                                    cx.stop_propagation();
                                    this.new_tab_profile(idx, cx);
                                    cx.notify();
                                }))
                        })),
                )
            })
    }
}

/// Dark-mode DWM frames for every window of this process. The titlebar
/// itself is hidden (appears_transparent), but the attribute still colors
/// the remaining 1px window border and any pre-first-paint frame flash.
/// DWM attribute only — no gpui changes.
#[cfg(windows)]
fn apply_dark_titlebars() {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
    use windows::core::BOOL;
    unsafe extern "system" fn apply(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == lparam.0 as u32 {
            let dark: i32 = 1;
            unsafe {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    &raw const dark as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(apply), LPARAM(GetCurrentProcessId() as isize));
    }
}

#[cfg(not(windows))]
fn apply_dark_titlebars() {}

/// Open a tab-group window hosting `initial` and register it for merge-all
/// and release cleanup.
fn open_tabs_window(cx: &mut App, initial: Vec<TabEntry>) {
    open_tabs_window_opts(cx, initial, &cli::Launch::default());
}

/// Open a tab-group window with wt-style launch geometry. `--size` uses cell
/// ESTIMATES (real metrics need a live window; the PTY refits to the painted
/// pane on the first frame, so only the window's outer size is approximate).
fn open_tabs_window_opts(cx: &mut App, initial: Vec<TabEntry>, launch: &cli::Launch) {
    const EST_CW: f32 = 8.5;
    const EST_CH: f32 = 21.0;
    let win_size = match launch.size_cells {
        Some((c, r)) => size(
            px(c as f32 * EST_CW + PAD * 2.0),
            px(r as f32 * EST_CH + TAB_STRIP_H + PAD * 2.0),
        ),
        None => size(px(1000.), px(640.)),
    };
    let bounds = match launch.pos {
        Some((x, y)) => Bounds {
            origin: point(px(x), px(y)),
            size: win_size,
        },
        None => Bounds::centered(None, win_size, cx),
    };
    let window_bounds = if launch.fullscreen {
        WindowBounds::Fullscreen(bounds)
    } else if launch.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("RikkaTerminal".into()),
                    // Hides the native titlebar (gpui: hide_title_bar) — the
                    // tab strip takes over via window_control_area hitboxes.
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                // gpui maps Blurred to ACCENT_ENABLE_ACRYLICBLURBEHIND
                // (SetWindowCompositionAttribute, Win10 1809+) — the same
                // Win10-era acrylic Files' setting uses. Opt-in until the
                // known drag-latency cost of that API is judged acceptable;
                // a settings file is a P1 item.
                window_background: if acrylic() {
                    gpui::WindowBackgroundAppearance::Blurred
                } else {
                    gpui::WindowBackgroundAppearance::Opaque
                },
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TabsWindow::new(window, cx, initial)),
        )
        .expect("open window");
    let Ok(entity) = handle.update(cx, |_, _, cx| cx.entity()) else {
        return;
    };
    hub::register_window(cx, handle.into(), entity.downgrade());
    // Window closed (caption ✕ or last-tab close): stop the surviving tabs'
    // drivers; the sessions drop with the entity and close their PTYs. Quit
    // once no window is left — with the titlebar integrated, the caption ✕
    // is the product's real close button and must end the process.
    cx.observe_release(&entity, |win: &mut TabsWindow, cx| {
        for tab in &win.tabs {
            tab.0.shutdown();
        }
        if hub::live_windows(cx) == 0 {
            cx.quit();
        }
    })
    .detach();
    apply_dark_titlebars();
}

/// Single-instance role after the socket election.
enum Role {
    /// This process owns the socket — it hosts windows and serves forwards.
    Monarch(ipc::transport::Monarch),
    /// Spawned by the monarch to host one window (crash isolation): no
    /// socket, no forwarding — just register back with the spawner.
    WindowProcess,
    /// Couldn't bind and couldn't forward (a rare race) — run a lone window.
    Standalone,
}

/// Bind the per-user socket (become monarch), or forward this launch to the
/// running monarch. Returns `None` when the launch was forwarded (this process
/// should exit); `Some(role)` when this process should run.
fn elect(endpoint: &str, launch: &cli::Launch) -> Option<Role> {
    for _ in 0..5 {
        match ipc::transport::Monarch::bind(endpoint) {
            Ok(monarch) => return Some(Role::Monarch(monarch)),
            // Someone holds the socket — forward our launch to them and exit.
            // If they vanished mid-race the connect fails; loop and re-bind.
            Err(_) if forward_launch(endpoint, launch).is_ok() => return None,
            Err(_) => continue,
        }
    }
    Some(Role::Standalone)
}

/// The wire form of a cold-start handoff: the same message the shim would
/// have sent warm, except the handles now live in THIS process (inherited
/// through CreateProcess), so the pid is ours and a monarch pulls from us.
fn attach_request(a: &cli::AttachSpec, launch: &cli::Launch) -> ipc::AttachArgs {
    let [input, output, signal, reference, server, client] = a.handles;
    // A tab-move relay parent left the screen replay in a temp file (bulk
    // bytes cannot ride handle inheritance) — consume it exactly once.
    let state = a
        .state_path
        .as_ref()
        .and_then(|p| {
            let bytes = std::fs::read(p);
            let _ = std::fs::remove_file(p);
            bytes.ok()
        })
        .map(|vt| ipc::state_from_vt(&vt));
    ipc::AttachArgs {
        pid: std::process::id(),
        handles: ipc::Handles {
            input,
            output,
            signal,
            reference,
            server,
            client,
            ..Default::default()
        },
        startup: ipc::StartupInfo {
            title: a.title.clone(),
            x: 0,
            y: 0,
            cols: launch.size_cells.map_or(0, |s| s.0),
            rows: launch.size_cells.map_or(0, |s| s.1),
        },
        state,
        elevated: false,
        target: ipc::Target::New,
        drop_at: None,
    }
}

/// Forward this launch to the running monarch and exit: a plain CLI goes as
/// a `spawn` (raw argv + cwd, re-parsed there); a cold-start handoff that
/// LOST the bind race goes as a regular `attach` — the winner pulls the
/// inherited handles straight out of this process (IPC.md's race rule).
fn forward_launch(endpoint: &str, launch: &cli::Launch) -> std::io::Result<()> {
    let mut conn = ipc::transport::connect(endpoint)?;
    let req = match &launch.attach {
        Some(a) => ipc::Request::Attach(attach_request(a, launch)),
        None => {
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(str::to_owned));
            let argv: Vec<String> = std::env::args().skip(1).collect();
            ipc::Request::Spawn(ipc::SpawnArgs {
                cwd,
                argv,
                ..Default::default()
            })
        }
    };
    conn.send_request(&req)?;
    if conn.recv_response()?.ok {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "monarch rejected the forwarded launch",
        ))
    }
}

/// Open the cold-start handoff window when this launch carries one. Returns
/// false when there is nothing to adopt or the adoption failed.
#[cfg(windows)]
fn open_inherited(cx: &mut App, launch: &cli::Launch) -> bool {
    let Some(a) = &launch.attach else {
        return false;
    };
    let args = attach_request(a, launch);
    match attach::local_attach(&args).and_then(attach::LocalAttach::into_session) {
        Ok(session) => {
            open_attached(cx, session, args.startup);
            true
        }
        Err(e) => {
            log::warn!("cold-start attach failed, opening a normal window: {e:#}");
            false
        }
    }
}

#[cfg(not(windows))]
fn open_inherited(_cx: &mut App, launch: &cli::Launch) -> bool {
    if launch.attach.is_some() {
        log::warn!("--attach is Windows-only (OS default-terminal handoff)");
    }
    false
}

/// A request accepted on the IPC thread, handed to the main (gpui) thread to
/// open its window. An `Attach` session is already live (its PTY threads are
/// pumping) — only the window wrapping needs the UI thread.
enum Forwarded {
    /// A forwarded launch: re-parse the CLI and open a window for it.
    Spawn(Vec<String>),
    /// An adopted OS handoff: wrap the session in a fresh window.
    #[cfg(windows)]
    Attach(Box<TerminalSession>, ipc::StartupInfo),
    /// A tab-move arriving on this window's OWN socket: adopt the session as
    /// a tab of the window this process hosts (never a new window).
    #[cfg(windows)]
    AdoptTab(Box<TerminalSession>, ipc::StartupInfo, Option<(i32, i32)>),
}

/// Apply one IPC-thread message on the gpui thread — shared by the monarch's
/// main-socket pump and every window's own-socket pump.
fn pump_forwarded(cx: &mut App, msg: Forwarded) {
    match msg {
        Forwarded::Spawn(argv) => open_forwarded(cx, argv),
        #[cfg(windows)]
        Forwarded::Attach(session, startup) => open_attached(cx, *session, startup),
        #[cfg(windows)]
        Forwarded::AdoptTab(session, startup, drop_at) => {
            adopt_forwarded(cx, *session, startup, drop_at)
        }
    }
}

/// Adopt an `attach` on the IPC thread — the handle pull must finish while
/// the sender still waits on our response. The session then gets its own
/// window process (crash isolation); only if that launch fails is it hosted
/// in this process as a fallback.
#[cfg(windows)]
fn handle_attach(
    tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    args: ipc::AttachArgs,
) -> Result<()> {
    if args.target != ipc::Target::New {
        // Tab moves route directly: resolve_window, then attach on the
        // window's own socket. The monarch never proxies handles.
        anyhow::bail!("attach target window:<id>: resolve_window and attach its own socket");
    }
    let pulled = attach::pull_attach(&args)?;
    match pulled.relay_to_window_process() {
        // Dropping `pulled` closes our copies; the child keeps its
        // inherited ones.
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!("attach relay failed, adopting in-process: {e:#}");
            let startup = pulled.startup.clone();
            let session = pulled.into_session()?;
            tx.unbounded_send(Forwarded::Attach(Box::new(session), startup))
                .map_err(|_| anyhow::anyhow!("monarch is shutting down"))?;
            Ok(())
        }
    }
}

#[cfg(not(windows))]
fn handle_attach(
    _tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    _args: ipc::AttachArgs,
) -> Result<()> {
    anyhow::bail!("attach is Windows-only (OS default-terminal handoff)")
}

/// The monarch's window bookkeeping (IPC.md `register_window` /
/// `list_windows`). v1: window ids are pids and there is no liveness
/// pruning — id-targeted routing lands with inc6's tab moves.
#[derive(Default)]
struct WindowDirectory(Vec<ipc::RegisterWindow>);

impl WindowDirectory {
    fn register(&mut self, r: ipc::RegisterWindow) {
        match self.0.iter_mut().find(|w| w.pid == r.pid) {
            Some(slot) => *slot = r,
            None => self.0.push(r),
        }
    }

    fn list(&self) -> Vec<ipc::WindowInfo> {
        self.0
            .iter()
            .map(|w| ipc::WindowInfo {
                id: w.window_id,
                title: None,
            })
            .collect()
    }

    /// The window's own socket endpoint, for direct tab-move routing. `None`
    /// when the id is unknown or the window registered without a socket
    /// (its bind failed — it cannot receive moves).
    fn resolve(&self, window_id: u64) -> Option<String> {
        self.0
            .iter()
            .find(|w| w.window_id == window_id && !w.endpoint.is_empty())
            .map(|w| w.endpoint.clone())
    }
}

/// Launch a forwarded spawn as its own OS process (`--window-process` + the
/// original argv, in the original cwd) — the crash-isolation core: one
/// window dying can never take the others with it.
fn spawn_window_process(s: &ipc::SpawnArgs) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--window-process").args(&s.argv);
    if let Some(cwd) = &s.cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().map(drop)
}

/// Window process: tell the monarch we exist (and where our own socket
/// listens, for direct tab-move routing), off the UI thread. Fire and
/// forget — if the monarch died meanwhile, coordination is paused anyway
/// and this window just keeps running (the isolation guarantee).
fn register_with_monarch(endpoint: String, window_endpoint: Option<String>) {
    std::thread::Builder::new()
        .name("rikka-register".into())
        .spawn(move || {
            let Ok(mut conn) = ipc::transport::connect(&endpoint) else {
                return;
            };
            let _ = conn.send_request(&ipc::Request::RegisterWindow(ipc::RegisterWindow {
                pid: std::process::id(),
                window_id: u64::from(std::process::id()),
                endpoint: window_endpoint.unwrap_or_default(),
            }));
            let _ = conn.recv_response();
        })
        .ok();
}

/// Monarch side: accept forwarded launches on a background thread. Spawns
/// and handoffs become their own window processes; the channel to the main
/// (gpui) thread only carries the in-process fallbacks.
fn spawn_ipc_accept(
    cx: &mut App,
    monarch: ipc::transport::Monarch,
    window_endpoint: Option<String>,
) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
    std::thread::Builder::new()
        .name("rikka-ipc".into())
        .spawn(move || {
            // The monarch hosts a window itself, so it is in the directory
            // too (same id scheme: pid), reachable through its own window
            // socket like everyone else.
            let directory = Arc::new(FairMutex::new(WindowDirectory::default()));
            directory.lock().register(ipc::RegisterWindow {
                pid: std::process::id(),
                window_id: u64::from(std::process::id()),
                endpoint: window_endpoint.unwrap_or_default(),
            });
            loop {
                let Ok(mut conn) = monarch.accept() else {
                    break;
                };
                // One worker per connection: a client that connects but
                // never sends its frame (dying process, wedged pipe) must
                // not stall every later launch and tab move behind it.
                let tx = tx.clone();
                let directory = Arc::clone(&directory);
                std::thread::Builder::new()
                    .name("rikka-ipc-conn".into())
                    .spawn(move || match conn.recv_request() {
                        Ok(ipc::Request::Spawn(s)) => {
                            // Crash isolation first; in-process only as
                            // fallback (a monarch-resident window beats a
                            // dead launch).
                            if let Err(e) = spawn_window_process(&s) {
                                log::warn!("window-process spawn failed, opening in-process: {e}");
                                let _ = tx.unbounded_send(Forwarded::Spawn(s.argv));
                            }
                            let _ = conn.send_response(&ipc::Response::ok());
                        }
                        Ok(ipc::Request::Attach(a)) => {
                            let resp = match handle_attach(&tx, a) {
                                Ok(()) => ipc::Response::ok(),
                                Err(e) => ipc::Response::error(e.to_string()),
                            };
                            let _ = conn.send_response(&resp);
                        }
                        Ok(ipc::Request::RegisterWindow(r)) => {
                            directory.lock().register(r);
                            let _ = conn.send_response(&ipc::Response::ok());
                        }
                        Ok(ipc::Request::ListWindows) => {
                            let windows = directory.lock().list();
                            let _ = conn.send_response(&ipc::Response {
                                ok: true,
                                windows: Some(windows),
                                ..Default::default()
                            });
                        }
                        Ok(ipc::Request::ResolveWindow { window }) => {
                            let resolved = directory.lock().resolve(window);
                            let resp = match resolved {
                                Some(endpoint) => ipc::Response {
                                    ok: true,
                                    endpoint: Some(endpoint),
                                    ..Default::default()
                                },
                                None => ipc::Response::error(format!(
                                    "window {window} is unknown or unreachable"
                                )),
                            };
                            let _ = conn.send_response(&resp);
                        }
                        Ok(ipc::Request::Ping) => {
                            let _ = conn.send_response(&ipc::Response::ok());
                        }
                        Err(_) => {} // client hung up before sending a frame
                    })
                    .ok();
            }
        })
        .ok();
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = cx.update(|cx| pump_forwarded(cx, msg));
            }
        })
        .detach();
}

/// Serve this window's OWN socket (IPC.md direct tab-move routing): an
/// `attach` landing here means "adopt as a tab of THIS window" — the sender
/// already resolved the target through the monarch. Everything else is a
/// protocol error; window coordination stays with the monarch.
#[cfg(windows)]
fn window_accept_loop(
    listener: ipc::transport::Monarch,
    tx: futures::channel::mpsc::UnboundedSender<Forwarded>,
) {
    loop {
        let Ok(mut conn) = listener.accept() else {
            break;
        };
        // One worker per connection — same reasoning as the monarch loop: a
        // silent client must not block the next tab move behind it.
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("rikka-win-conn".into())
            .spawn(move || match conn.recv_request() {
                Ok(ipc::Request::Attach(a)) => {
                    let resp = match adopt_attach(&tx, a) {
                        Ok(()) => ipc::Response::ok(),
                        Err(e) => ipc::Response::error(e.to_string()),
                    };
                    let _ = conn.send_response(&resp);
                }
                Ok(ipc::Request::Ping) => {
                    let _ = conn.send_response(&ipc::Response::ok());
                }
                Ok(_) => {
                    let _ = conn.send_response(&ipc::Response::error(
                        "window socket: attach and ping only",
                    ));
                }
                Err(_) => {}
            })
            .ok();
    }
}

/// Adopt a direct-routed tab move on the IPC thread: pull the handles while
/// the sender still waits on our response, assemble the session, and hand it
/// to the gpui thread as a tab for this process's window.
#[cfg(windows)]
fn adopt_attach(
    tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    args: ipc::AttachArgs,
) -> Result<()> {
    let drop_at = args.drop_at;
    let pulled = attach::pull_attach(&args)?;
    let startup = pulled.startup.clone();
    let session = pulled.into_session()?;
    tx.unbounded_send(Forwarded::AdoptTab(Box::new(session), startup, drop_at))
        .map_err(|_| anyhow::anyhow!("window is shutting down"))?;
    Ok(())
}

/// Bind this process's own window socket and serve it. Failure is degraded
/// operation, not fatal: the window still works, it just cannot RECEIVE
/// direct tab moves (and its registration advertises no endpoint).
#[cfg(windows)]
fn spawn_window_accept(cx: &mut App) -> Option<String> {
    let name = ipc::transport::window_endpoint_name(std::process::id());
    let listener = match ipc::transport::Monarch::bind(&name) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("window socket bind failed ({name}): {e}");
            return None;
        }
    };
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
    std::thread::Builder::new()
        .name("rikka-win-ipc".into())
        .spawn(move || window_accept_loop(listener, tx))
        .ok()?;
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = cx.update(|cx| pump_forwarded(cx, msg));
            }
        })
        .detach();
    Some(name)
}

/// Adopt a direct-routed session as a tab of this process's window. A window
/// process hosts exactly one window; if none is alive (shutdown race), a
/// fresh window still beats losing the session. Lookup and adopt run on the
/// same thread with no await between them, so the entry cannot fall through
/// the gap.
#[cfg(windows)]
fn adopt_forwarded(
    cx: &mut App,
    session: TerminalSession,
    startup: ipc::StartupInfo,
    drop_at: Option<(i32, i32)>,
) {
    match hub::any_window(cx) {
        Some(view) => {
            let entry = hub::new_tab(cx, session);
            let _ = view.update(cx, |v, cx| v.adopt_dropped(entry, drop_at, cx));
        }
        None => open_attached(cx, session, startup),
    }
}

/// Window for an adopted handoff session. Always its own new window, never an
/// auto-tab (IPC.md's windowing rule); the tab title was seeded from the
/// handoff's startup info when one was carried.
#[cfg(windows)]
fn open_attached(cx: &mut App, session: TerminalSession, startup: ipc::StartupInfo) {
    let launch = cli::Launch {
        size_cells: (startup.cols >= 2 && startup.rows >= 2)
            .then_some((startup.cols, startup.rows)),
        ..Default::default()
    };
    let entry = hub::new_tab(cx, session);
    open_tabs_window_opts(cx, vec![entry], &launch);
}

/// Re-parse a forwarded CLI and open a window for it IN THIS process — the
/// fallback when the window-process spawn failed (isolation is best-effort;
/// a launch must never be lost).
fn open_forwarded(cx: &mut App, argv: Vec<String>) {
    let launch = cli::parse(argv).unwrap_or_default();
    let specs = cli::expand_dir_tabs(launch.tabs.clone());
    let specs = if specs.is_empty() {
        vec![default_spec(cx)]
    } else {
        specs
    };
    let initial: Vec<TabEntry> = specs
        .iter()
        .filter_map(|spec| create_tab_spec(cx, spec))
        .collect();
    if !initial.is_empty() {
        open_tabs_window_opts(cx, initial, &launch);
    }
}

fn main() {
    // Same field diagnosis as shogun-desktop: panics (any thread) append to
    // %TEMP%/shogun-tsf/panic.log.
    rikka_terminal_core::install_panic_log();
    rikka_terminal_core::install_file_logger("rikka-terminal");
    // wt-compatible CLI (see `cli`). Errors and --help go to a message box —
    // the GUI-subsystem binary has no console.
    let launch = match cli::parse(std::env::args().skip(1).collect()) {
        Ok(launch) => launch,
        Err(msg) => {
            cli::error_box(&msg);
            return;
        }
    };
    // Single-instance election: become the monarch, or forward this launch to
    // the running one and exit (the forwarded launch opens its window there).
    // A monarch-spawned window process skips all of it — the parent owns the
    // socket, and electing or forwarding from here would boomerang the launch.
    let endpoint = ipc::transport::endpoint_name();
    let role = if launch.window_process {
        Role::WindowProcess
    } else {
        match elect(&endpoint, &launch) {
            Some(role) => role,
            None => return,
        }
    };
    Application::new().run(move |cx| {
        // The engine's renderer rides gpui-component primitives; initialise
        // its statics and pin the dark theme (the grid brings its own colors).
        gpui_component::init(cx);
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
        // New-tab profiles: wt's list filtered by rikka's config (read once
        // at startup; a broken/absent config or wt just yields an empty menu
        // and the built-in shell search).
        let (wt, wt_default) = wt_profiles::discover();
        let cfg = config::Config::load();
        apply_appearance(&cfg);
        session_log::init(cfg.logging.clone());
        let menu = cfg.build_menu(wt, wt_default);
        hub::init(cx, menu);
        // A cold-start handoff rides in this launch (IPC.md "attach cold"):
        // adopt the inherited handles as the initial window. On failure fall
        // through to a normal launch — a visible window beats a silent death
        // for diagnosis (the file log has the why).
        if !open_inherited(cx, &launch) {
            // `rt <dir>` opens the default shell there (code-style; one tab
            // per directory).
            let specs = cli::expand_dir_tabs(launch.tabs.clone());
            let specs = if specs.is_empty() {
                vec![default_spec(cx)]
            } else {
                specs
            };
            let initial: Vec<TabEntry> = specs
                .iter()
                .filter_map(|spec| create_tab_spec(cx, spec))
                .collect();
            open_tabs_window_opts(cx, initial, &launch);
        }
        // Every window-hosting process serves its own socket for direct
        // tab-move routing (Windows; Unix moves await SCM_RIGHTS).
        #[cfg(windows)]
        let window_endpoint = spawn_window_accept(cx);
        #[cfg(not(windows))]
        let window_endpoint: Option<String> = None;
        match role {
            // Monarch: serve forwarded launches, handoffs and registrations.
            Role::Monarch(monarch) => spawn_ipc_accept(cx, monarch, window_endpoint),
            // Window process: report in so list/route can see us.
            Role::WindowProcess => register_with_monarch(endpoint, window_endpoint),
            Role::Standalone => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_insert_index_picks_the_nearest_gap() {
        // 5 tabs of 200px: left half of tab 0 → before it, right half →
        // after it, and so on down the strip.
        assert_eq!(strip_insert_index(-50.0, 200.0, 5), 0);
        assert_eq!(strip_insert_index(40.0, 200.0, 5), 0);
        assert_eq!(strip_insert_index(160.0, 200.0, 5), 1);
        assert_eq!(strip_insert_index(430.0, 200.0, 5), 2);
        assert_eq!(strip_insert_index(999.0, 200.0, 5), 5);
        assert_eq!(strip_insert_index(5000.0, 200.0, 5), 5);
        // Degenerate width appends rather than dividing by zero.
        assert_eq!(strip_insert_index(100.0, 0.0, 3), 3);
    }

    #[test]
    fn window_directory_upserts_by_pid_and_lists_ids() {
        let reg = |pid: u32, id: u64| ipc::RegisterWindow {
            pid,
            window_id: id,
            endpoint: String::new(),
        };
        let mut dir = WindowDirectory::default();
        dir.register(reg(100, 100));
        dir.register(reg(200, 200));
        dir.register(reg(100, 101)); // re-registration replaces, no duplicate
        let ids: Vec<u64> = dir.list().iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![101, 200]);
    }

    #[test]
    fn window_directory_resolves_endpoints() {
        let mut dir = WindowDirectory::default();
        dir.register(ipc::RegisterWindow {
            pid: 1,
            window_id: 1,
            endpoint: "ep-one".into(),
        });
        dir.register(ipc::RegisterWindow {
            pid: 2,
            window_id: 2,
            endpoint: String::new(),
        });
        assert_eq!(dir.resolve(1).as_deref(), Some("ep-one"));
        assert_eq!(dir.resolve(2), None, "endpointless windows are unreachable");
        assert_eq!(dir.resolve(9), None, "unknown ids resolve to nothing");
    }

    /// A window's own socket adopts a direct-routed attach as a tab: the
    /// handles are pulled while the sender waits on the response, and the
    /// live session reaches the gpui pump as `AdoptTab` — never a new
    /// window. This is the receiving half of a cross-process tab move.
    #[cfg(windows)]
    #[test]
    fn window_socket_adopts_direct_attach() {
        use std::os::windows::io::IntoRawHandle as _;
        let name = format!("rikka-test-win-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        let (out_read, _out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let mut conn = ipc::transport::connect(&name).expect("connect window socket");
        conn.send_request(&ipc::Request::Attach(ipc::AttachArgs {
            pid: std::process::id(),
            handles: ipc::Handles {
                input: in_write.into_raw_handle() as isize as i64,
                output: out_read.into_raw_handle() as isize as i64,
                ..Default::default()
            },
            startup: ipc::StartupInfo {
                title: Some("moved".into()),
                ..Default::default()
            },
            target: ipc::Target::Window(u64::from(std::process::id())),
            ..Default::default()
        }))
        .unwrap();
        let resp = conn.recv_response().unwrap();
        assert!(resp.ok, "adopt must succeed: {:?}", resp.error);

        match rx.try_recv().expect("one pumped message") {
            Forwarded::AdoptTab(session, startup, _) => {
                assert_eq!(startup.title.as_deref(), Some("moved"));
                assert_eq!(session.title.lock().as_deref(), Some("moved"));
            }
            _ => panic!("expected exactly one AdoptTab"),
        }
        drop(conn);
        drop(accept); // detach: the loop parks in accept() until process exit
    }

    /// A client that connects but never sends a frame must not stall the
    /// socket: each connection gets its own worker, so the next client is
    /// served while the silent one sits there.
    #[cfg(windows)]
    #[test]
    fn silent_connection_does_not_block_the_window_socket() {
        let name = format!("rikka-test-silent-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, _rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        let _silent = ipc::transport::connect(&name).expect("silent client connects");

        // The ping runs on a helper thread so a regression fails by
        // timeout instead of hanging the test run.
        let name2 = name.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut conn = ipc::transport::connect(&name2).expect("second client connects");
            conn.send_request(&ipc::Request::Ping).unwrap();
            done_tx.send(conn.recv_response().unwrap().ok).ok();
        });
        let ok = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("second client starved behind the silent one");
        assert!(ok, "ping must succeed");
        drop(accept);
    }

    /// The SENDING half against the receiving half, one process, real
    /// socket: a LIVE session (reader pumping) is quiesced, its birth-time
    /// duplicates pushed over the window socket, pulled back
    /// (`DUPLICATE_CLOSE_SOURCE` against ourselves) and assembled — and
    /// output written to the PTY *after* the move lands in the receiver's
    /// grid, still flowing after the sender's remains drop. This is the
    /// whole cross-process tab move, minus the second process.
    #[cfg(windows)]
    #[test]
    fn live_tab_moves_across_the_window_socket() {
        use std::io::Write as _;
        use std::os::windows::io::OwnedHandle;

        use rikka_terminal_core::pty_handoff::{HandoffPty, build_handoff_session};

        let (out_read, mut out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let source = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("live source session");
        *source.title.lock() = Some("mover".into());

        // Screen content the move must carry: parsed into the SENDER's grid
        // (and gone from the pipe) before the transfer starts.
        out_write.write_all(b"BEFORE-MOVE").unwrap();
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let row0: String = source.snapshot.lock().cells[0]
                    .iter()
                    .map(|c| c.c)
                    .collect();
                if row0.starts_with("BEFORE-MOVE") {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "content never reached the sender's grid"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        let name = format!("rikka-test-move-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        tab_move::send_tab(
            &source,
            tab_move::Destination::Window {
                id: 1,
                endpoint: name,
                drop_at: Some((123, 45)),
            },
        )
        .expect("send the live tab");

        let received = match rx.try_recv().expect("one pumped message") {
            Forwarded::AdoptTab(session, startup, drop_at) => {
                assert_eq!(startup.title.as_deref(), Some("mover"));
                // The drag-drop point survives the wire — the receiver
                // inserts at the strip position under it.
                assert_eq!(drop_at, Some((123, 45)));
                session
            }
            _ => panic!("expected exactly one AdoptTab"),
        };

        let sees = |needle: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let grid: String = received
                    .snapshot
                    .lock()
                    .cells
                    .iter()
                    .flat_map(|row| row.iter().map(|c| c.c))
                    .collect();
                if grid.contains(needle) {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{needle:?} never reached the receiver's grid"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // The sender's screen arrived with the move (the state replay ran
        // as the receiver's parser preface)…
        sees("BEFORE-MOVE");
        // …and the live pipe keeps flowing after it.
        out_write.write_all(b"AFTER-MOVE").unwrap();
        sees("AFTER-MOVE");

        // The sender's teardown must not break the moved pipes: its handles
        // are independent duplicates of what the receiver pulled.
        drop(source);
        out_write.write_all(b" STILL-FLOWING").unwrap();
        sees("STILL-FLOWING");
        drop(accept);
    }
}
