use crate::image_upload::{self, UploadState};
use crate::pty_spawn as pty_session;
use crate::settings::{ConnectionBackend, ControlPathType, load_settings, save_settings};
use crate::ssh::SshClient;
use crate::tabs::AgentCardData;
use crate::tabs::{
    SettingsTab, fetch_agent_cards, render_agents_tab, render_dashboard_tab, render_settings_tab,
    render_terminal_tab, render_terminal_tab_disconnected, render_terminal_tab_empty,
    render_terminal_tab_error, run_fetch_agents, run_fetch_dashboard,
};
use crate::terminal::TerminalSession;
use crate::terminal::ime::{ImeHost, TerminalIme};
use crate::terminal::keys::key_to_pty_bytes;
use crate::terminal::selection::{self, SelectionHost, SelectionState};
use crate::theme::Colors;
use gpui::{
    App, Bounds, ClickEvent, Context, ExternalPaths, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    size,
};
use gpui_component::{
    Disableable, Root, Sizable,
    button::{Button, ButtonVariants as _},
    h_flex,
    radio::{Radio, RadioGroup},
    switch::Switch,
    v_flex,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

const TAB_LABELS: [&str; 6] = ["将軍", "エージェント", "戦況", "設定", "──", "家老陣"];

/// Key context for the terminal panes. Bindings scoped to this context are
/// matched deeper than gpui_component Root's, letting the terminal reclaim
/// keys Root binds globally (tab / shift-tab focus cycling).
pub const TERMINAL_KEY_CONTEXT: &str = "ShogunTerminal";

/// Vertical padding of the terminal scroll pane (`.p_1()` = 4px top + 4px
/// bottom). Must be subtracted when computing the grid row count: otherwise
/// the grid + padding overflows the pane by up to 8px whenever
/// `available_height % cell_height < 8`, and `overflow_y_scroll` turns that
/// remainder into a spurious few-pixel scroll range.
pub const TERMINAL_PANE_PADDING_PX: f32 = 8.0;

/// Height of the green/red connection status bar shown at the top of every
/// tab except 設定 (the terminal 陣幕, the 戦況 dashboard header, the
/// エージェント header). Shared so the bar is the same size everywhere —
/// 32 px fits a `.small()` button (24 px) with breathing room.
pub const STATUS_BAR_HEIGHT_PX: f32 = 32.0;

gpui::actions!(
    shogun_terminal,
    [
        TerminalSendTab,
        TerminalSendBacktab,
        TerminalCopy,
        TerminalPaste
    ]
);
// Cell metrics measurement moved into the engine crate with the rest of the
// renderer; re-exported here because half the app imports it from window.
pub use crate::terminal::renderer::measure_cell_metrics;

/// State for the Agents tab.
pub struct AgentsState {
    pub content: String,
    pub cards: Vec<AgentCardData>,
    pub is_connected: bool,
    pub error_message: Option<String>,
    pub last_refresh: SystemTime,
}

impl Default for AgentsState {
    fn default() -> Self {
        Self {
            content: String::new(),
            cards: Vec::new(),
            is_connected: false,
            error_message: None,
            last_refresh: SystemTime::UNIX_EPOCH,
        }
    }
}

fn fetch_agents_bundle(
    settings: crate::settings::ShogunDesktopSettings,
) -> anyhow::Result<(String, Vec<AgentCardData>)> {
    if settings.project.path.is_empty() {
        anyhow::bail!("プロジェクトパスが未設定です（設定タブで project_path を入力してください）");
    }
    let ssh = SshClient::from_settings(&settings)?;
    let agents = settings.sessions.agents.clone();
    let cards = fetch_agent_cards(&ssh, &settings.project.path, &agents);
    let content = run_fetch_agents(settings).unwrap_or_default();
    Ok((content, cards))
}

/// State for the Dashboard tab.
pub struct DashboardState {
    pub content: String,
    pub is_connected: bool,
    pub error_message: Option<String>,
    pub last_refresh: SystemTime,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            content: String::new(),
            is_connected: false,
            error_message: None,
            last_refresh: SystemTime::UNIX_EPOCH,
        }
    }
}

/// Pane index used by the shared selection machinery (`terminal::selection`):
/// 0 = 将軍 tab, 1 = 家老陣 (multiagent) tab.
pub(crate) fn selection_pane(is_shogun: bool) -> usize {
    if is_shogun { 0 } else { 1 }
}

pub struct ShogunWindow {
    selected_tab: usize,
    settings_tab: SettingsTab,
    agents_state: AgentsState,
    dashboard_state: DashboardState,
    pub shogun_session: Option<TerminalSession>,
    pub multiagent_session: Option<TerminalSession>,
    pub shogun_error: Option<String>,
    pub multiagent_error: Option<String>,
    pub shogun_scroll_handle: ScrollHandle,
    pub multiagent_scroll_handle: ScrollHandle,
    status_message: SharedString,
    /// Last known terminal size, used to detect viewport changes and resize sessions.
    terminal_cols: u16,
    terminal_rows: u16,
    terminal_font: String,
    /// Cached session names for the tab jinmaku (from settings). Kept on the
    /// view because `render` runs every frame and must not touch the
    /// settings file — synchronous DrvFs reads there are exactly the class
    /// of stall that makes other terminals freeze for seconds.
    shogun_session_name: String,
    multiagent_session_name: String,
    upload_state: UploadState,
    dragged_paths: Option<Vec<std::path::PathBuf>>,
    /// Focus handle for the terminal panes; required so an IME input handler
    /// can be registered (GPUI only routes WM_CHAR / IME composition events
    /// to a registered input handler on the focused element).
    pub(crate) terminal_focus: FocusHandle,
    /// Shared IME text-input handler (see `terminal::ime`). Holds the current
    /// composition text; observed so composition changes re-render.
    ime: gpui::Entity<TerminalIme<Self>>,
    /// Shared mouse-selection state (see `terminal::selection`).
    selection: SelectionState,
    /// Mirrors `Window::is_window_active()` (kept fresh by an activation
    /// observer) so the async notification watcher — which has no `Window` —
    /// can apply Ghostty-style focus suppression.
    window_active: bool,
    /// OSC 9 / 777 desktop notifications enabled (settings.terminal).
    desktop_notifications: bool,
    /// 家老陣 tab notifications (default off — many agents, constant toasts).
    desktop_notifications_multiagent: bool,
    /// Terminal-pane size actually painted (logical px, padding box),
    /// reported by the pane's overlay canvas. `(0, 0)` until the first
    /// paint; `render` derives rows/cols from this instead of estimating
    /// chrome heights (the estimate drifting even 1px past the padding
    /// made the pane scrollable — the "micro-scroll" bug).
    pane_measured: Rc<Cell<(f32, f32)>>,
    /// Terminal-pane origin (logical px, content box) from the same overlay
    /// canvas — converts window-relative wheel positions to grid cells for
    /// mouse reporting (tmux picks the pane under the cursor by coordinate).
    pane_origin: Rc<Cell<(f32, f32)>>,
    /// Fractional wheel-line accumulator for PTY wheel forwarding (trackpads
    /// deliver sub-line pixel deltas).
    wheel_accum: f32,
}

impl SelectionHost for ShogunWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn pane_session(&self, pane: usize) -> Option<&TerminalSession> {
        // Pane indices from `selection_pane`: 0 = 将軍, 1 = 家老陣.
        match pane {
            0 => self.shogun_session.as_ref(),
            1 => self.multiagent_session.as_ref(),
            _ => None,
        }
    }
}

impl ImeHost for ShogunWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.active_session()
    }

    fn ime_font(&self) -> &str {
        &self.terminal_font
    }
}

impl ShogunWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings().unwrap_or_default();
        let terminal_font = settings.terminal.font.clone();
        let shogun_session_name = settings.sessions.shogun.clone();
        let multiagent_session_name = settings.sessions.multiagent.clone();
        let desktop_notifications = settings.terminal.desktop_notifications;
        let desktop_notifications_multiagent = settings.terminal.desktop_notifications_multiagent;
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |view, window, _cx| {
            view.window_active = window.is_window_active();
            view.sync_focus_reports();
        })
        .detach();
        let terminal_focus = cx.focus_handle();
        // TSF (Windows): make the taskbar IME indicator track this window while
        // a terminal tab's input is focused — app-driven so gpui stays
        // untouched (see `crate::tsf`; gated by SHOGUN_TSF).
        window
            .on_focus_in(&terminal_focus, cx, |_, _| crate::tsf::on_input_focus())
            .detach();
        window
            .on_focus_out(&terminal_focus, cx, |_, _, _| crate::tsf::on_input_blur())
            .detach();
        Self {
            selected_tab: 0,
            settings_tab: SettingsTab::new(window, cx, &settings),
            agents_state: AgentsState::default(),
            dashboard_state: DashboardState::default(),
            shogun_session: None,
            multiagent_session: None,
            shogun_error: None,
            multiagent_error: None,
            shogun_scroll_handle: ScrollHandle::default(),
            multiagent_scroll_handle: ScrollHandle::default(),
            status_message: SharedString::default(),
            terminal_cols: 0,
            terminal_rows: 0,
            terminal_font,
            shogun_session_name,
            multiagent_session_name,
            upload_state: UploadState::Idle,
            dragged_paths: None,
            terminal_focus,
            ime,
            selection: SelectionState::default(),
            window_active: window.is_window_active(),
            desktop_notifications,
            desktop_notifications_multiagent,
            pane_measured: Rc::new(Cell::new((0.0, 0.0))),
            pane_origin: Rc::new(Cell::new((0.0, 0.0))),
            wheel_accum: 0.0,
        }
    }

    /// Forward a wheel event on a terminal pane to its PTY when the running
    /// application asked for it (mouse reporting — tmux `mouse on` — or
    /// alternate scroll). Returns `true` when consumed; `false` means the
    /// caller should treat the wheel as a local scroll. Mirrors the shell
    /// window's wheel path; see `TerminalSession::wheel_to_pty`.
    pub(crate) fn wheel_to_pty_for_pane(
        &mut self,
        is_shogun: bool,
        event: &ScrollWheelEvent,
        cw: f32,
        ch: f32,
    ) -> bool {
        let session = if is_shogun {
            self.shogun_session.as_ref()
        } else {
            self.multiagent_session.as_ref()
        };
        let Some(s) = session else {
            return false;
        };
        self.wheel_accum += match &event.delta {
            ScrollDelta::Pixels(p) => (p.y / px(1.)) / ch,
            ScrollDelta::Lines(l) => l.y,
        };
        // With `whole == 0` (sub-line trackpad fragment) this still consults
        // the mode: wheel_to_pty sends nothing but returns whether the PTY
        // owns the wheel, so fragments never scroll locally *and* remotely.
        let whole = self.wheel_accum.trunc() as i32;
        self.wheel_accum -= whole as f32;
        let (ox, oy) = self.pane_origin.get();
        let cols = s.cols.load(Ordering::Relaxed).max(1) as usize;
        let rows = s.rows.load(Ordering::Relaxed).max(1) as usize;
        let col = ((((event.position.x / px(1.)) - ox) / cw).max(0.0) as usize).min(cols - 1);
        let row = ((((event.position.y / px(1.)) - oy) / ch).max(0.0) as usize).min(rows - 1);
        let mods = crate::terminal::ReportMods {
            alt: event.modifiers.alt,
            ctrl: event.modifiers.control,
        };
        s.wheel_to_pty(whole, col, row, mods)
    }

    /// The terminal session belonging to the currently selected tab, if any.
    fn active_session(&self) -> Option<&TerminalSession> {
        match self.selected_tab {
            0 => self.shogun_session.as_ref(),
            5 => self.multiagent_session.as_ref(),
            _ => None,
        }
    }

    /// Send raw bytes to the active tab's PTY, if connected.
    pub(crate) fn send_bytes_to_active(&self, bytes: &[u8]) {
        if let Some(session) = self.active_session() {
            session.send_bytes(bytes);
        }
    }

    /// Tab / back-tab for the TerminalSendTab/Backtab actions. Routed through
    /// the key encoder — the actions bypass capture_key_down, so the kitty
    /// keyboard-protocol mode must be applied here too (shift-tab is CSI Z
    /// only in legacy mode; kitty-aware apps expect CSI 9;2u).
    pub(crate) fn send_tab_to_active(&self, shift: bool) {
        let Some(session) = self.active_session() else {
            return;
        };
        let mode = *session.term.lock().mode();
        let ks = gpui::Keystroke {
            key: "tab".to_string(),
            modifiers: gpui::Modifiers {
                shift,
                ..Default::default()
            },
            key_char: None,
        };
        if let Some(bytes) = key_to_pty_bytes(&ks, mode) {
            session.send_bytes(&bytes);
        }
    }

    /// Paste the OS clipboard into the active pane's PTY (ctrl-shift-v /
    /// cmd-v via the `TerminalPaste` action), bracketed when the app
    /// enabled `?2004`.
    pub(crate) fn paste_clipboard(&self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if let Some(session) = self.active_session() {
            session.paste(&text);
        }
    }

    /// Copy the selected cells of the owning pane to the OS clipboard
    /// (ctrl-shift-c / cmd-c via the `TerminalCopy` action).
    pub(crate) fn copy_selection(&self, cx: &mut Context<Self>) {
        let session = match self.selection.selected() {
            Some((pane, ..)) if pane == selection_pane(true) => self.shogun_session.as_ref(),
            Some(_) => self.multiagent_session.as_ref(),
            None => return,
        };
        selection::copy_to_clipboard(&self.selection, session, cx);
    }

    fn get_ssh_client(&self) -> Option<SshClient> {
        if !self
            .shogun_session
            .as_ref()
            .map(|s| s.is_connected())
            .unwrap_or(false)
        {
            return None;
        }
        let settings = load_settings().ok()?;
        if settings.ssh.host.is_empty() || settings.project.path.is_empty() {
            return None;
        }
        SshClient::from_settings(&settings).ok()
    }

    fn start_upload(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let settings = load_settings().unwrap_or_default();
        let project_path = settings.project.path.clone();
        if project_path.is_empty() {
            self.upload_state = UploadState::Error("プロジェクトパスが未設定".to_string());
            cx.notify();
            return;
        }
        let ssh_client = match self.get_ssh_client() {
            Some(c) => c,
            None => {
                self.upload_state = UploadState::Error("SSH未接続".to_string());
                cx.notify();
                return;
            }
        };

        let total = paths.len();
        self.upload_state = UploadState::InProgress { done: 0, total };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut success_names: Vec<String> = vec![];
            let mut failed = 0usize;

            for (i, path) in paths.iter().enumerate() {
                let fname = image_upload::remote_filename(path, i);
                match ssh_client.upload_image(path, &fname, &project_path) {
                    Ok(()) => success_names.push(fname),
                    Err(_) => failed += 1,
                }
                let done = i + 1;
                let _ = this.update(cx, |this, cx| {
                    this.upload_state = UploadState::InProgress { done, total };
                    cx.notify();
                });
            }

            if !success_names.is_empty() {
                let names = success_names.join(", ");
                let msg = format!(
                    "Desktop から画像{}枚を受信: {}\nqueue/screenshots/ に保存済み。",
                    success_names.len(),
                    names
                );
                let escaped = msg.replace('\'', "'\\''");
                let notify_cmd = format!(
                    "bash {project_path}/scripts/inbox_write.sh shogun '{escaped}' screenshot desktop"
                );
                let _ = ssh_client.exec(&notify_cmd);
            }

            let s = success_names.len();
            let _ = this.update(cx, |this, cx| {
                this.upload_state = UploadState::Done {
                    success: s,
                    failed,
                };
                cx.notify();
            });

            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.upload_state = UploadState::Idle;
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_and_upload_images(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            rfd::FileDialog::new()
                .add_filter("画像", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
                .set_title("転送する画像を選択")
                .pick_files()
        });
        cx.spawn(async move |this, cx| {
            if let Some(paths) = task.await {
                let images: Vec<std::path::PathBuf> = paths
                    .into_iter()
                    .filter(|p| image_upload::is_image(p))
                    .collect();
                if !images.is_empty() {
                    let _ = this.update(cx, |this, cx| {
                        this.start_upload(images, cx);
                    });
                }
            }
        })
        .detach();
    }

    fn render_upload_status(&self) -> gpui::AnyElement {
        match &self.upload_state {
            UploadState::InProgress { done, total } => div()
                .px_2()
                .py_1()
                .child(format!("転送中… {done}/{total}枚"))
                .into_any_element(),
            UploadState::Done { success, failed } => {
                let msg = if *failed == 0 {
                    format!("✅ {success}枚 転送完了")
                } else {
                    format!("✅ {success}枚 完了 / ❌ {failed}枚 失敗")
                };
                div().px_2().py_1().child(msg).into_any_element()
            }
            UploadState::Error(e) => div()
                .px_2()
                .py_1()
                .text_color(gpui::rgb(0xcc2200))
                .child(format!("❌ {e}"))
                .into_any_element(),
            UploadState::Idle => div().into_any_element(),
        }
    }

    pub fn start_shogun_session(&mut self, cx: &mut Context<Self>) {
        let settings = load_settings().unwrap_or_default();
        if settings.ssh.host.is_empty() {
            return;
        }
        let tmux_session = settings.sessions.shogun.clone();

        cx.spawn(async move |this, cx| {
            let settings_bg = settings.clone();
            let connect = cx
                .background_executor()
                .spawn(async move { SshClient::from_settings(&settings_bg) })
                .await;

            let ssh = match connect {
                Ok(client) => client,
                Err(e) => {
                    let _ = this.update(cx, |view, cx| {
                        view.shogun_error = Some(format!("SSH接続失敗: {e}"));
                        cx.notify();
                    });
                    return;
                }
            };

            let control_path = ssh.control_socket_path();
            let spawn_result = cx
                .background_executor()
                .spawn(
                    async move { pty_session::spawn(&ssh, &tmux_session, 220, 50, control_path) },
                )
                .await;

            let _ = this.update(cx, |view, cx| {
                match spawn_result {
                    Ok(session) => {
                        view.shogun_session = Some(session);
                        view.shogun_error = None;
                        // Correct the session's initial focused=true if the
                        // user is looking elsewhere by the time spawn lands.
                        view.sync_focus_reports();
                        view.start_terminal_refresh(true, cx);
                    }
                    Err(e) => {
                        view.shogun_error = Some(format!("PTY起動失敗: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn start_multiagent_session(&mut self, cx: &mut Context<Self>) {
        let settings = load_settings().unwrap_or_default();
        if settings.ssh.host.is_empty() {
            return;
        }
        let tmux_session = settings.sessions.multiagent.clone();

        cx.spawn(async move |this, cx| {
            let settings_bg = settings.clone();
            let connect = cx
                .background_executor()
                .spawn(async move { SshClient::from_settings(&settings_bg) })
                .await;

            let ssh = match connect {
                Ok(client) => client,
                Err(e) => {
                    let _ = this.update(cx, |view, cx| {
                        view.multiagent_error = Some(format!("SSH接続失敗: {e}"));
                        cx.notify();
                    });
                    return;
                }
            };

            let control_path = ssh.control_socket_path();
            let spawn_result = cx
                .background_executor()
                .spawn(
                    async move { pty_session::spawn(&ssh, &tmux_session, 220, 50, control_path) },
                )
                .await;

            let _ = this.update(cx, |view, cx| {
                match spawn_result {
                    Ok(session) => {
                        view.multiagent_session = Some(session);
                        view.multiagent_error = None;
                        // Correct the session's initial focused=true if the
                        // user is looking elsewhere by the time spawn lands.
                        view.sync_focus_reports();
                        view.start_terminal_refresh(false, cx);
                    }
                    Err(e) => {
                        view.multiagent_error = Some(format!("PTY起動失敗: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Event-driven refresh: one watcher task per session. The task parks on
    /// the session's `Notify` (zero wakeups while the terminal is idle) and
    /// coalesces output bursts into ~60fps frames with a short timer. Each
    /// (re)connect spawns a fresh watcher bound to the new session; a watcher
    /// exits when its session has been replaced (`Arc::ptr_eq` mismatch), and
    /// a watcher whose session merely died stays parked at no CPU cost.
    fn start_terminal_refresh(&self, is_shogun: bool, cx: &mut Context<Self>) {
        let session = if is_shogun {
            &self.shogun_session
        } else {
            &self.multiagent_session
        };
        let Some(session) = session.as_ref() else {
            return;
        };
        let generation = Arc::clone(&session.generation);
        let notify = Arc::clone(&session.notify);
        let snapshot = Arc::clone(&session.snapshot);
        let scroll = if is_shogun {
            self.shogun_scroll_handle.clone()
        } else {
            self.multiagent_scroll_handle.clone()
        };

        cx.spawn(async move |this, cx| {
            let mut last = generation.load(Ordering::Relaxed);
            loop {
                // While SGR-blink cells are on screen, race the PTY wakeup
                // against the blink phase timer so the on/off flip repaints
                // even with no output. Otherwise park purely on notify —
                // zero wakeups while idle.
                let blink = snapshot.lock().has_blink;
                if blink {
                    let timer = cx.background_executor().timer(Duration::from_millis(300));
                    futures::future::select(Box::pin(notify.notified()), Box::pin(timer)).await;
                } else {
                    notify.notified().await;
                }
                // Coalesce a burst of PTY chunks into a single frame.
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let cur = generation.load(Ordering::Relaxed);
                let data_changed = cur != last;
                if !data_changed && !blink {
                    continue;
                }
                last = cur;

                let alive = this.update(cx, |view, cx| {
                    let current = if is_shogun {
                        &view.shogun_session
                    } else {
                        &view.multiagent_session
                    };
                    // Session replaced by a reconnect: a newer watcher owns it.
                    let owned = current
                        .as_ref()
                        .is_some_and(|s| Arc::ptr_eq(&s.generation, &generation));
                    if !owned {
                        return false;
                    }
                    let notifications = current.as_ref().map(|s| Arc::clone(&s.notifications));

                    // OSC 9 / 777 desktop notifications, Ghostty-style focus
                    // suppression: toast only when the user is NOT looking at
                    // this surface (window inactive or another tab selected).
                    // Always drain, even when suppressed or disabled.
                    if let Some(queue) = notifications {
                        let pending = crate::terminal::notify::take_notifications(&queue);
                        let tab = if is_shogun { 0 } else { 5 };
                        let surface_focused = view.window_active && view.selected_tab == tab;
                        // 家老陣 is swallowed unless its own switch is also on.
                        let enabled = view.desktop_notifications
                            && (is_shogun || view.desktop_notifications_multiagent);
                        if !pending.is_empty() && enabled && !surface_focused {
                            let default_title = if is_shogun { "将軍" } else { "家老陣" };
                            for n in &pending {
                                crate::notify_toast::show(
                                    n.title.as_deref().unwrap_or(default_title),
                                    &n.body,
                                );
                            }
                        }
                    }
                    // Blink-only ticks repaint but must not touch the scroll
                    // position. On data changes keep the pane pinned to its
                    // bottom — normally a no-op, since the grid is PTY-fit
                    // sized and never overflows; it only matters transiently
                    // (first paint before the fit lands). The old
                    // autoscroll-lock bookkeeping here was inert for the same
                    // reason (see terminal_tab's wheel handler).
                    if data_changed {
                        scroll.scroll_to_bottom();
                    }
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Handle a key-down aimed at the terminal. Returns `true` when the key
    /// was consumed here (the caller should stop propagation so GPUI actions
    /// like tab focus-cycling never see it); `false` when the key is left for
    /// the platform text-input path (WM_CHAR / IME → EntityInputHandler).
    pub(crate) fn handle_terminal_key(
        &mut self,
        event: &KeyDownEvent,
        _cx: &mut Context<Self>,
    ) -> bool {
        // key_to_pty_bytes returns None for keys that must be left to the
        // platform text-input path (WM_CHAR / IME → TerminalIme handler).
        // The term mode selects the encoding (legacy vs kitty protocol).
        let mode = self
            .active_session()
            .map(|s| *s.term.lock().mode())
            .unwrap_or_else(alacritty_terminal::term::TermMode::empty);
        let Some(bytes) = key_to_pty_bytes(&event.keystroke, mode) else {
            return false;
        };
        if let Some(session) = self.active_session() {
            session.send_bytes(&bytes);
        }
        true
    }

    fn render_terminal_for_session(
        &self,
        session: &Option<TerminalSession>,
        error: &Option<String>,
        scroll_handle: &ScrollHandle,
        is_shogun: bool,
        cw: f32,
        ch: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(err) = error {
            return render_terminal_tab_error(err.clone(), cx).into_any_element();
        }
        if let Some(session) = session {
            if session.is_connected() {
                let snap = session.snapshot.lock().clone();
                render_terminal_tab(
                    &snap,
                    scroll_handle,
                    &self.terminal_focus,
                    self.ime.clone(),
                    self.ime.read(cx).marked.clone(),
                    self.selection.range_for(selection_pane(is_shogun)),
                    self.selection.hover_link_for(selection_pane(is_shogun)),
                    Some(&session.images),
                    is_shogun,
                    &self.terminal_font,
                    cw,
                    ch,
                    self.pane_measured.clone(),
                    self.pane_origin.clone(),
                    cx,
                )
                .into_any_element()
            } else {
                let btn_id = if is_shogun {
                    "reconnect-shogun"
                } else {
                    "reconnect-multiagent"
                };
                let reconnect_btn = Button::new(btn_id).label("再接続").on_click(cx.listener(
                    move |this, _, _, cx| {
                        if is_shogun {
                            this.start_shogun_session(cx);
                        } else {
                            this.start_multiagent_session(cx);
                        }
                    },
                ));
                render_terminal_tab_disconnected(reconnect_btn, cx).into_any_element()
            }
        } else {
            render_terminal_tab_empty(cx).into_any_element()
        }
    }

    /// Start the agents status auto-refresh loop (10 s interval).
    pub fn start_agents_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let settings = load_settings().unwrap_or_default();
                let result = cx
                    .background_executor()
                    .spawn(async move { fetch_agents_bundle(settings) })
                    .await;

                let now = SystemTime::now();
                let _ = this.update(cx, |view, cx| {
                    match result {
                        Ok((content, cards)) => {
                            view.agents_state.content = content;
                            view.agents_state.cards = cards;
                            view.agents_state.is_connected = true;
                            view.agents_state.error_message = None;
                            view.agents_state.last_refresh = now;
                        }
                        Err(err) => {
                            view.agents_state.is_connected = false;
                            view.agents_state.error_message = Some(format!("SSH接続失敗: {err}"));
                        }
                    }
                    cx.notify();
                });

                cx.background_executor()
                    .timer(Duration::from_secs(10))
                    .await;
            }
        })
        .detach();
    }

    /// Trigger an immediate agents status refresh.
    pub fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let settings = load_settings().unwrap_or_default();
            let result = cx
                .background_executor()
                .spawn(async move { fetch_agents_bundle(settings) })
                .await;

            let now = SystemTime::now();
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok((content, cards)) => {
                        view.agents_state.content = content;
                        view.agents_state.cards = cards;
                        view.agents_state.is_connected = true;
                        view.agents_state.error_message = None;
                        view.agents_state.last_refresh = now;
                    }
                    Err(err) => {
                        view.agents_state.is_connected = false;
                        view.agents_state.error_message = Some(format!("SSH接続失敗: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Start the dashboard auto-refresh loop (30 s interval).
    pub fn start_dashboard_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let settings = load_settings().unwrap_or_default();
                let result = cx
                    .background_executor()
                    .spawn(async move { run_fetch_dashboard(settings) })
                    .await;

                let now = SystemTime::now();
                let _ = this.update(cx, |view, cx| {
                    match result {
                        Ok(content) => {
                            view.dashboard_state.content = content;
                            view.dashboard_state.is_connected = true;
                            view.dashboard_state.error_message = None;
                            view.dashboard_state.last_refresh = now;
                        }
                        Err(err) => {
                            view.dashboard_state.is_connected = false;
                            view.dashboard_state.error_message =
                                Some(format!("SSH接続失敗: {err}"));
                        }
                    }
                    cx.notify();
                });

                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
            }
        })
        .detach();
    }

    /// Trigger an immediate dashboard refresh.
    pub fn refresh_dashboard(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let settings = load_settings().unwrap_or_default();
            let result = cx
                .background_executor()
                .spawn(async move { run_fetch_dashboard(settings) })
                .await;

            let now = SystemTime::now();
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(content) => {
                        view.dashboard_state.content = content;
                        view.dashboard_state.is_connected = true;
                        view.dashboard_state.error_message = None;
                        view.dashboard_state.last_refresh = now;
                    }
                    Err(err) => {
                        view.dashboard_state.is_connected = false;
                        view.dashboard_state.error_message = Some(format!("SSH接続失敗: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_tab(
        &mut self,
        index: usize,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_tab = index;
        self.sync_focus_reports();
        cx.notify();
    }

    /// Focus reporting (?1004): a terminal surface here counts as focused
    /// when the window is active AND its tab is the selected one — the same
    /// rule the OSC 9 toast suppression uses. `report_focus` de-duplicates,
    /// so calling this on every activation/tab change is safe.
    fn sync_focus_reports(&self) {
        if let Some(s) = self.shogun_session.as_ref() {
            s.report_focus(self.window_active && self.selected_tab == 0);
        }
        if let Some(s) = self.multiagent_session.as_ref() {
            s.report_focus(self.window_active && self.selected_tab == 5);
        }
    }

    fn set_control_path(&mut self, path: ControlPathType, cx: &mut Context<Self>) {
        self.settings_tab.control_path = path;
        cx.notify();
    }

    fn set_connection_backend(&mut self, backend: ConnectionBackend, cx: &mut Context<Self>) {
        self.settings_tab.connection_backend = backend;
        cx.notify();
    }

    pub fn save_settings(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_tab.collect(cx);
        self.terminal_font = settings.terminal.font.clone();
        self.shogun_session_name = settings.sessions.shogun.clone();
        self.multiagent_session_name = settings.sessions.multiagent.clone();
        // Font features apply engine-globally on the next frame.
        crate::terminal::renderer::set_font_features(crate::settings::parse_font_features(
            &settings.terminal.font_features,
        ));
        self.status_message = match save_settings(&settings) {
            Ok(()) => "設定を保存しました".into(),
            Err(err) => format!("保存失敗: {err}").into(),
        };
        cx.notify();
    }

    fn set_terminal_font_preset(
        &mut self,
        font: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_tab.set_terminal_font_preset(font, window, cx);
        cx.notify();
    }

    pub fn test_ssh(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_tab.collect(cx);
        self.status_message = "SSH接続テスト中...".into();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let host = settings.ssh.host.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    SshClient::from_settings(&settings).and_then(|client| {
                        let output = client.exec("echo ok")?;
                        Ok(format!(
                            "✅ 接続成功 (Host: {host}, echo: {})",
                            output.trim()
                        ))
                    })
                })
                .await;

            let message: SharedString = match result {
                Ok(msg) => msg.into(),
                Err(err) => format!("❌ 接続失敗: {err}").into(),
            };

            let _ = this.update(cx, |view, cx| {
                view.status_message = message;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_terminal_with_ui(
        &self,
        session_opt: &Option<TerminalSession>,
        error_opt: &Option<String>,
        scroll_handle: &ScrollHandle,
        is_shogun: bool,
        session_name: &str,
        cw: f32,
        ch: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        const SPECIAL_KEYS: [(&str, &str); 9] = [
            ("↵", "\n"),
            ("C-c", "\x03"),
            ("C-b", "\x02"),
            ("↑", "\x1b[A"),
            ("↓", "\x1b[B"),
            ("Tab", "\t"),
            ("ESC", "\x1b"),
            ("C-o", "\x0f"),
            ("C-d", "\x04"),
        ];

        let is_connected = session_opt
            .as_ref()
            .map(|s| s.is_connected())
            .unwrap_or(false);
        let jinmaku_bg = if is_connected {
            Colors::matsuba()
        } else {
            Colors::kurenai()
        };
        let jinmaku_text: SharedString = if is_connected {
            format!("接続中 — {}:main", session_name).into()
        } else {
            "未接続".into()
        };

        let terminal_content = self.render_terminal_for_session(
            session_opt,
            error_opt,
            scroll_handle,
            is_shogun,
            cw,
            ch,
            cx,
        );

        let key_buttons = SPECIAL_KEYS.iter().enumerate().map(|(i, (label, seq))| {
            let seq: &'static str = seq;
            let label: &'static str = label;
            let id_base: usize = if is_shogun { i } else { i + 100 };
            Button::new(("sk", id_base))
                .label(label)
                .small()
                .on_click(cx.listener(move |this, _, _, _cx| {
                    let session = if is_shogun {
                        &this.shogun_session
                    } else {
                        &this.multiagent_session
                    };
                    if let Some(s) = session {
                        s.send_bytes(seq.as_bytes());
                    }
                }))
        });

        let upload_btn = if is_shogun {
            Some(
                Button::new("upload-image")
                    .label("📎")
                    .tooltip("画像をサーバーへ転送")
                    .disabled(!is_connected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pick_and_upload_images(cx);
                    })),
            )
        } else {
            None
        };

        let upload_status = if is_shogun {
            self.render_upload_status()
        } else {
            div().into_any_element()
        };

        let mut root = v_flex()
            .flex_1()
            .size_full()
            .bg(Colors::shikkoku())
            .child(
                div()
                    // `.relative()` so the progress bar pins to this bar's
                    // bottom edge — the same placement the shell window uses.
                    .relative()
                    .w_full()
                    .h(px(STATUS_BAR_HEIGHT_PX))
                    .bg(jinmaku_bg)
                    .flex()
                    .items_center()
                    // Match the 戦況 / エージェント headers' horizontal inset so
                    // the status text starts at the same x on every tab.
                    .px_3()
                    .text_color(Colors::zouge())
                    .text_size(px(12.))
                    .child(jinmaku_text)
                    .children(
                        session_opt
                            .as_ref()
                            .and_then(terminal_progress)
                            .map(|p| {
                                render_progress_bar(("jinmaku-progress", is_shogun as usize), p)
                            }),
                    ),
            )
            .child(div().flex_1().overflow_hidden().child(terminal_content))
            .child(upload_status);

        if is_shogun {
            root =
                root.on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    let images: Vec<std::path::PathBuf> = paths
                        .paths()
                        .iter()
                        .filter(|p| image_upload::is_image(p))
                        .cloned()
                        .collect();
                    this.dragged_paths = None;
                    if !images.is_empty() {
                        this.start_upload(images, cx);
                    }
                    cx.notify();
                }));
        }

        root.child(
            h_flex()
                .w_full()
                .h(px(32.))
                .bg(Colors::sumi())
                .items_center()
                .gap_1()
                .px_1()
                .children(key_buttons)
                .children(upload_btn.into_iter()),
        )
        .into_any_element()
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(48.))
            .bg(Colors::sumi())
            .border_t_1()
            .border_color(Colors::border())
            .children((0..6).map(|index| {
                let selected = self.selected_tab == index;
                // OSC 9;4 progress reported by the terminal behind this tab
                // (将軍 / 家老陣), drawn as a thin bar along the tab's bottom.
                let progress = match index {
                    0 => self.shogun_session.as_ref(),
                    5 => self.multiagent_session.as_ref(),
                    _ => None,
                }
                .and_then(terminal_progress);
                div()
                    .id(("tab", index))
                    .relative()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(if selected {
                        Colors::kinpaku()
                    } else if index == 4 {
                        Colors::border()
                    } else {
                        Colors::muted()
                    })
                    .child(TAB_LABELS[index])
                    .children(progress.map(|p| render_progress_bar(("tab-progress", index), p)))
                    .on_click(cx.listener(move |this, event, window, cx| {
                        if index != 4 {
                            this.select_tab(index, event, window, cx);
                        }
                    }))
            }))
    }
}

/// Progress to display for a terminal session: an explicit OSC 9;4 report when
/// the app sends one, otherwise a bar inferred from a window-title spinner.
///
/// The fallback covers agents that drop OSC 9;4 on some surfaces — Claude Code
/// animates a Braille spinner in the title and emits no OSC 9;4 inside tmux, so
/// its progress would otherwise never show in the 将軍 / 家老陣 tabs. The
/// spinner→activity heuristic lives in `rikka-terminal-agent-integration`
/// (kept out of the agent-agnostic engine core); this maps it to the existing
/// indeterminate bar. Requires the tmux side to forward the title
/// (`set-titles on`, done in the attach command).
pub fn terminal_progress(
    session: &crate::terminal::TerminalSession,
) -> Option<(crate::terminal::progress::ProgressState, u8)> {
    if let Some(explicit) = session.progress.get() {
        return Some(explicit);
    }
    let title = session.title.lock();
    rikka_terminal_agent_integration::progress_from_title(title.as_deref().unwrap_or(""))
        .map(|_| (crate::terminal::progress::ProgressState::Indeterminate, 0))
}

/// Segment count of the rainbow fill — enough for a smooth spectrum at tab
/// width without meaningfully increasing quad count.
const PROGRESS_RAINBOW_SEGMENTS: usize = 16;

/// Thin OSC 9;4 progress bar pinned to its parent's bottom edge (the parent
/// must be `.relative()`). Used on the terminal tabs and the shell window's
/// status bar.
///
/// Normal progress fills to the percentage with a scrolling rainbow
/// (ゲーミング仕様 — a static green was too easy to miss); indeterminate is
/// the same rainbow across the full width (no meaningful value). Error=紅 and
/// warning=金箔 stay static so their semantics remain readable at a glance.
///
/// The rainbow uses gpui's `with_animation`, which only requests frames while
/// the element is actually rendered — when progress is removed the loop stops
/// and idle CPU stays at zero.
pub fn render_progress_bar(
    id: impl Into<gpui::ElementId>,
    (state, percent): (crate::terminal::progress::ProgressState, u8),
) -> gpui::AnyElement {
    use crate::terminal::progress::ProgressState;
    use gpui::AnimationExt as _;

    let fraction = match state {
        ProgressState::Normal => percent as f32 / 100.0,
        ProgressState::Indeterminate => 1.0,
        // Keep a visible sliver even at 0% so the state itself shows.
        ProgressState::Error | ProgressState::Warning => (percent as f32 / 100.0).max(0.05),
    };
    let fill = div().h_full().w(gpui::relative(fraction.clamp(0.0, 1.0)));
    let fill: gpui::AnyElement =
        match state {
            ProgressState::Error => fill.bg(Colors::kurenai()).into_any_element(),
            ProgressState::Warning => fill.bg(Colors::kinpaku()).into_any_element(),
            ProgressState::Normal | ProgressState::Indeterminate => fill
                .with_animation(
                    id,
                    gpui::Animation::new(Duration::from_secs(2)).repeat(),
                    |bar, delta| {
                        // Scrolling spectrum: each segment's hue is offset by its
                        // position, and the whole ramp slides with the frame delta.
                        // Each segment is a gradient from its own hue to the next
                        // segment's, so the spectrum is seamless instead of a
                        // 16-step color bar. Lightness sits at 0.42 — bright
                        // enough to read on the raised track, dim enough to not
                        // sear the eyes while it scrolls.
                        let seg_hue = move |i: usize| {
                            let hue = (i as f32 / PROGRESS_RAINBOW_SEGMENTS as f32 - delta)
                                .rem_euclid(1.0);
                            gpui::hsla(hue, 0.75, 0.42, 1.0)
                        };
                        bar.child(h_flex().size_full().children(
                            (0..PROGRESS_RAINBOW_SEGMENTS).map(move |i| {
                                div().flex_1().h_full().bg(gpui::linear_gradient(
                                    90.,
                                    gpui::linear_color_stop(seg_hue(i), 0.),
                                    gpui::linear_color_stop(seg_hue(i + 1), 1.),
                                ))
                            }),
                        ))
                    },
                )
                .into_any_element(),
        };
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(px(3.))
        .bg(Colors::raised())
        .child(fill)
        .into_any_element()
}

impl Render for ShogunWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // ── PTY resize on viewport change ─────────────────────────────────────
        // Calculate the terminal dimensions from the current viewport.
        // Chrome heights: jinmaku status bar (32) + key buttons (32) + tab bar (48) = 112 px.
        //
        // Cell dimensions are measured from the active font via TextSystem::ch_advance
        // (Windows Terminal–style; see measure_cell_metrics).
        let (cw, ch) = measure_cell_metrics(
            &cx.text_system(),
            &self.terminal_font,
            window.scale_factor(),
        );
        {
            let vp = window.viewport_size();
            // Prefer the pane size the overlay canvas actually painted —
            // exact regardless of chrome heights, borders or DPI rounding.
            // The overlay is pinned to the pane's content box (inset by the
            // padding), so the measurement is the grid area directly. The
            // chrome-height estimate only covers the first frame, before
            // anything has painted.
            let (mw, mh) = self.pane_measured.get();
            let content_w = if mw > 0.0 { mw } else { vp.width / px(1.) };
            let content_h = if mh > 0.0 {
                mh.max(ch)
            } else {
                ((vp.height / px(1.)) - 112.0 - TERMINAL_PANE_PADDING_PX).max(ch)
            };
            let new_cols = ((content_w / cw) as u16).max(1);
            let new_rows = ((content_h / ch) as u16).max(1);

            // Resize whenever the viewport changes OR when a session was just
            // started and its recorded size doesn't yet match the target.
            let session_needs_resize = |s: &Option<TerminalSession>| {
                s.as_ref().map_or(false, |sess| {
                    sess.cols.load(Ordering::Relaxed) != new_cols
                        || sess.rows.load(Ordering::Relaxed) != new_rows
                })
            };

            if new_cols != self.terminal_cols
                || new_rows != self.terminal_rows
                || session_needs_resize(&self.shogun_session)
                || session_needs_resize(&self.multiagent_session)
            {
                self.terminal_cols = new_cols;
                self.terminal_rows = new_rows;
                // TIOCGWINSZ pixel fields carry device pixels, hence × scale.
                let cell_px = (cw * window.scale_factor(), ch * window.scale_factor());
                if let Some(s) = &self.shogun_session {
                    s.resize(new_cols, new_rows, cell_px);
                }
                if let Some(s) = &self.multiagent_session {
                    s.resize(new_cols, new_rows, cell_px);
                }
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        let content: gpui::AnyElement = match self.selected_tab {
            0 => {
                let session_name = self.shogun_session_name.clone();
                self.render_terminal_with_ui(
                    &self.shogun_session,
                    &self.shogun_error,
                    &self.shogun_scroll_handle,
                    true,
                    &session_name,
                    cw,
                    ch,
                    cx,
                )
            }
            1 => render_agents_tab(&self.agents_state, cx).into_any_element(),
            2 => render_dashboard_tab(&self.dashboard_state, window, cx).into_any_element(),
            3 => {
                let save_btn = Button::new("save-settings")
                    .primary()
                    .label("保存")
                    .on_click(cx.listener(Self::save_settings));
                let test_btn = Button::new("test-ssh")
                    .label("SSH接続テスト")
                    .on_click(cx.listener(Self::test_ssh));
                let shell_btn =
                    Button::new("open-shell")
                        .label("シェルを開く")
                        .on_click(cx.listener(|_, _, _, cx| {
                            crate::shell_window::open_shell_window(cx);
                        }));
                let backend = self.settings_tab.connection_backend.clone();
                let connection_backend_selector = RadioGroup::horizontal("conn-backend")
                    .selected_index(Some(match backend {
                        ConnectionBackend::Native => 0,
                        ConnectionBackend::System => 1,
                    }))
                    .child(Radio::new("conn-backend-native").label("Native (russh)"))
                    .child(Radio::new("conn-backend-system").label("System (ssh.exe)"))
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        let backend = match index {
                            0 => ConnectionBackend::Native,
                            _ => ConnectionBackend::System,
                        };
                        this.set_connection_backend(backend, cx);
                    }));
                let accept_all = self.settings_tab.accept_all_host_keys;
                let accept_all_host_keys_toggle = Switch::new("accept-all-host-keys")
                    .checked(accept_all)
                    .label("ホスト鍵を常に受け入れる（known_hosts スキップ）")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.settings_tab.accept_all_host_keys = *checked;
                        cx.notify();
                    }));
                // Toggles apply to the live watcher immediately; the 保存
                // button persists them (same lifecycle as the other fields).
                let notification_toggles = v_flex()
                    .gap_2()
                    .child(
                        Switch::new("desktop-notifications")
                            .checked(self.settings_tab.desktop_notifications)
                            .label("デスクトップ通知を出す")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings_tab.desktop_notifications = *checked;
                                this.desktop_notifications = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        Switch::new("desktop-notifications-multiagent")
                            .checked(self.settings_tab.desktop_notifications_multiagent)
                            .label("家老陣タブも通知する")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings_tab.desktop_notifications_multiagent = *checked;
                                this.desktop_notifications_multiagent = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        // Applies on the next connection (persisted by 保存),
                        // like the TERM / identity fields.
                        Switch::new("tmux-forward-titles")
                            .checked(self.settings_tab.tmux_forward_titles)
                            .label("tmux タイトル転送（エージェント進捗バー用・要再接続）")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings_tab.tmux_forward_titles = *checked;
                                cx.notify();
                            })),
                    );
                let identity = self.settings_tab.terminal_identity;
                let terminal_identity_selector = RadioGroup::horizontal("terminal-identity")
                    .selected_index(Some(match identity {
                        crate::settings::TerminalIdentity::Honest => 0,
                        crate::settings::TerminalIdentity::Ghostty => 1,
                    }))
                    .child(Radio::new("identity-honest").label("正直 (rikka-terminal)"))
                    .child(Radio::new("identity-ghostty").label("Ghostty偽装"))
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.settings_tab.terminal_identity = match index {
                            0 => crate::settings::TerminalIdentity::Honest,
                            _ => crate::settings::TerminalIdentity::Ghostty,
                        };
                        cx.notify();
                    }));
                let term_name = self.settings_tab.term_name;
                let term_name_selector = RadioGroup::horizontal("term-name")
                    .selected_index(Some(match term_name {
                        crate::settings::TermName::Xterm256color => 0,
                        crate::settings::TermName::XtermGhostty => 1,
                        crate::settings::TermName::XtermRikka => 2,
                    }))
                    .child(Radio::new("term-256color").label("xterm-256color"))
                    .child(Radio::new("term-ghostty").label("xterm-ghostty"))
                    // xterm-rikka stays visible but inert: there is no rikka
                    // terminfo to broadcast yet, so selecting it could only
                    // break the remote. Re-enable once RikkaTerminal ships
                    // its terminfo + auto-install.
                    .child(
                        Radio::new("term-rikka")
                            .label("xterm-rikka（将来用）")
                            .disabled(true),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        let (term, notice) = match index {
                            0 => (
                                crate::settings::TermName::Xterm256color,
                                "TERM=xterm-256color — 全リモートに terminfo があり安全",
                            ),
                            1 => (
                                crate::settings::TermName::XtermGhostty,
                                "⚠ TERM=xterm-ghostty — リモートに Ghostty の terminfo が必要。無いと vim/less/tmux が壊れる（確認: infocmp xterm-ghostty）",
                            ),
                            _ => {
                                // Disabled radio; keep the current value.
                                this.status_message =
                                    "xterm-rikka は terminfo 配布の仕組みができるまで選択不可".into();
                                cx.notify();
                                return;
                            }
                        };
                        this.settings_tab.term_name = term;
                        this.status_message = notice.into();
                        cx.notify();
                    }));
                // Standing warning while a risky TERM is selected: what it
                // needs and what breaks without it.
                let term_name_warning: Option<SharedString> = match term_name {
                    crate::settings::TermName::Xterm256color => None,
                    crate::settings::TermName::XtermGhostty => Some(
                        "注意: リモート側に Ghostty の terminfo が必要（ghostty 導入済みか、~/.terminfo へ手動配置）。無い接続先では vim/less/tmux が起動しない・表示が崩れる。保存後の新規接続から適用"
                            .into(),
                    ),
                    // Reachable only via a hand-edited settings.toml.
                    crate::settings::TermName::XtermRikka => Some(
                        "警告: xterm-rikka の terminfo は未配布 — リモートに存在せずフルスクリーン系アプリが壊れる。settings.toml の手編集でのみ到達する実験値。保存後の新規接続から適用"
                            .into(),
                    ),
                };
                let font_preset_buttons = h_flex()
                    .gap_2()
                    .child(
                        Button::new("font-preset-hw")
                            .label("HW")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_terminal_font_preset("Moralerspace Neon HW", window, cx);
                            })),
                    )
                    .child(
                        Button::new("font-preset-cica")
                            .label("Cica")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_terminal_font_preset("Cica", window, cx);
                            })),
                    );
                #[cfg(windows)]
                let control_path_selector = {
                    let current = self.settings_tab.control_path.clone();
                    RadioGroup::horizontal("ctrl-path")
                        .selected_index(Some(match current {
                            ControlPathType::Socket => 0,
                            ControlPathType::NamedPipe => 1,
                            ControlPathType::None => 2,
                        }))
                        .child(Radio::new("ctrl-path-socket").label("Socket（%TEMP% ファイル）"))
                        .child(
                            Radio::new("ctrl-path-named-pipe").label("Named Pipe（\\\\.\\pipe\\）"),
                        )
                        .child(Radio::new("ctrl-path-none").label("無効（毎回新規接続）"))
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            let path = match index {
                                0 => ControlPathType::Socket,
                                1 => ControlPathType::NamedPipe,
                                _ => ControlPathType::None,
                            };
                            this.set_control_path(path, cx);
                        }))
                };
                render_settings_tab(
                    &self.settings_tab,
                    self.status_message.clone(),
                    save_btn,
                    test_btn,
                    shell_btn,
                    connection_backend_selector,
                    accept_all_host_keys_toggle,
                    font_preset_buttons,
                    notification_toggles,
                    terminal_identity_selector,
                    term_name_selector,
                    term_name_warning,
                    #[cfg(windows)]
                    Some(control_path_selector),
                    #[cfg(not(windows))]
                    None::<gpui::Empty>,
                )
                .into_any_element()
            }
            5 => {
                let session_name = self.multiagent_session_name.clone();
                self.render_terminal_with_ui(
                    &self.multiagent_session,
                    &self.multiagent_error,
                    &self.multiagent_scroll_handle,
                    false,
                    &session_name,
                    cw,
                    ch,
                    cx,
                )
            }
            _ => div()
                .flex_1()
                .size_full()
                .bg(Colors::shikkoku())
                .into_any_element(),
        };

        // Root carries the bundled-emoji fallback so every descendant text
        // run (UI chrome included) resolves emoji to the embedded font.
        // (Terminal-tab progress is drawn on the 陣幕 status bar in
        // `render_terminal_with_ui`, matching the shell window's placement.)
        crate::terminal::renderer::with_emoji_fallback(div())
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::shikkoku())
            .child(
                // `.w_full()` is load-bearing: gpui's flex does not stretch
                // cross-axis by default, so without a definite width here the
                // nested `.w_full()` status bars (陣幕 / 戦況 / エージェント)
                // resolve against an indefinite width and shrink to their text
                // instead of spanning the pane. The tab bar looks fine only
                // because it is a direct child of the size_full root.
                div()
                    .w_full()
                    .flex_1()
                    .overflow_hidden()
                    .child(content),
            )
            .child(self.render_tab_bar(cx))
    }
}

pub fn open_shogun_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("将軍デスクトップ".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| {
                let mut win = ShogunWindow::new(window, cx);
                win.start_shogun_session(cx);
                win.start_multiagent_session(cx);
                win.start_agents_background(cx);
                win.start_dashboard_background(cx);
                win
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("open main window");
}
