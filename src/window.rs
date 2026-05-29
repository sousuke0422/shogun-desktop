use crate::image_upload::{self, UploadState};
use crate::settings::{ConnectionBackend, ControlPathType, load_settings, save_settings};
use crate::ssh::SshClient;
use crate::tabs::AgentCardData;
use crate::tabs::{
    SettingsTab, fetch_agent_cards, render_agents_tab, render_dashboard_tab, render_settings_tab,
    render_terminal_tab, render_terminal_tab_disconnected, render_terminal_tab_empty,
    render_terminal_tab_error, run_fetch_agents, run_fetch_dashboard,
};
use crate::terminal::TerminalSession;
use crate::terminal::keys::key_to_bytes;
use crate::terminal::pty_session;
use crate::terminal::renderer::{CELL_H, cell_width_for_font};
use crate::theme::Colors;
use gpui::{
    App, Bounds, ClickEvent, Context, ExternalPaths, IntoElement, KeyDownEvent, ParentElement,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{
    Disableable, Root, Sizable,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

const TAB_LABELS: [&str; 6] = ["将軍", "エージェント", "戦況", "設定", "──", "家老陣"];

/// Measure cell dimensions from the active GPUI `TextSystem`.
///
/// Follows Windows Terminal's approach (PR #16729 / #13549):
///   - **cw** = `ch_advance(font_id, font_size)` — advance width of the '0' glyph
///     (CSS `ch` unit, the canonical monospace cell width).
///   - **ch** = `ascent(font_id, font_size) + descent(font_id, font_size)` — natural
///     line height without extra line-gap, so box-drawing geometry fills cells exactly.
///
/// Falls back to the static [`cell_width_for_font`] / [`CELL_H`] table on error
/// so the terminal keeps working even if a font fails to resolve.
pub fn measure_cell_metrics(ts: &std::sync::Arc<gpui::TextSystem>, font_name: &str) -> (f32, f32) {
    // `gpui::font` requires `Into<SharedString>` which expects 'static for &str.
    // Convert to owned String first to satisfy the lifetime bound.
    let font_spec = gpui::font(font_name.to_string());
    let font_id = ts.resolve_font(&font_spec);
    let font_size = px(13.0);

    // ch_advance returns Result<Pixels, _>.  Guard against both Err and Ok(0.0):
    // GPUI may return Ok(Pixels(0.0)) while the font is still being measured
    // (lazy load).  A zero cw causes (viewport / 0) = f32::INFINITY, which casts
    // to u16::MAX = 65535 — sending a 65535-col resize to tmux and breaking layout.
    let measured_cw = ts
        .ch_advance(font_id, font_size)
        .map(f32::from)
        .unwrap_or(0.0);
    let cw = if measured_cw > 0.5 {
        measured_cw
    } else {
        cell_width_for_font(font_name)
    };

    // ascent and descent are both positive in GPUI (absolute distances from baseline).
    // Total natural line height = ascent + descent.
    // Guard against zero/near-zero values (font not yet measured) with CELL_H fallback.
    let ascent = f32::from(ts.ascent(font_id, font_size));
    let descent = f32::from(ts.descent(font_id, font_size));
    let natural_ch = ascent + descent;
    let ch = if natural_ch > 1.0 { natural_ch } else { CELL_H };

    (cw, ch)
}

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
    pub(crate) shogun_scroll_locked: bool,
    pub(crate) multiagent_scroll_locked: bool,
    pub(crate) shogun_prev_offset_y: f32,
    pub(crate) multiagent_prev_offset_y: f32,
    shogun_last_gen: u64,
    multiagent_last_gen: u64,
    terminal_refresh_started: bool,
    status_message: SharedString,
    /// Last known terminal size, used to detect viewport changes and resize sessions.
    terminal_cols: u16,
    terminal_rows: u16,
    terminal_font: String,
    upload_state: UploadState,
    dragged_paths: Option<Vec<std::path::PathBuf>>,
}

impl ShogunWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings().unwrap_or_default();
        let terminal_font = settings.terminal.font.clone();
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
            shogun_scroll_locked: false,
            multiagent_scroll_locked: false,
            shogun_prev_offset_y: 0.0,
            multiagent_prev_offset_y: 0.0,
            shogun_last_gen: 0,
            multiagent_last_gen: 0,
            terminal_refresh_started: false,
            status_message: SharedString::default(),
            terminal_cols: 0,
            terminal_rows: 0,
            terminal_font,
            upload_state: UploadState::Idle,
            dragged_paths: None,
        }
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

    fn maybe_start_terminal_refresh(&mut self, cx: &mut Context<Self>) {
        if self.terminal_refresh_started {
            return;
        }
        if self.shogun_session.is_some() || self.multiagent_session.is_some() {
            self.terminal_refresh_started = true;
            self.start_terminal_refresh(cx);
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
                        view.maybe_start_terminal_refresh(cx);
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
                        view.maybe_start_terminal_refresh(cx);
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

    fn start_terminal_refresh(&self, cx: &mut Context<Self>) {
        let gen_s = self
            .shogun_session
            .as_ref()
            .map(|s| Arc::clone(&s.generation));
        let gen_m = self
            .multiagent_session
            .as_ref()
            .map(|s| Arc::clone(&s.generation));
        let scroll_s = self.shogun_scroll_handle.clone();
        let scroll_m = self.multiagent_scroll_handle.clone();

        cx.spawn(async move |this, cx| {
            let mut last_s = 0u64;
            let mut last_m = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let cur_s = gen_s
                    .as_ref()
                    .map(|g| g.load(Ordering::Relaxed))
                    .unwrap_or(0);
                let cur_m = gen_m
                    .as_ref()
                    .map(|g| g.load(Ordering::Relaxed))
                    .unwrap_or(0);

                if cur_s != last_s || cur_m != last_m {
                    let s_changed = cur_s != last_s;
                    let m_changed = cur_m != last_m;
                    last_s = cur_s;
                    last_m = cur_m;
                    let _ = this.update(cx, |view, cx| {
                        view.shogun_last_gen = cur_s;
                        view.multiagent_last_gen = cur_m;
                        if s_changed {
                            if !view.shogun_scroll_locked {
                                scroll_s.scroll_to_bottom();
                            }
                            view.shogun_prev_offset_y = scroll_s.offset().y / px(1.);
                        }
                        if m_changed {
                            if !view.multiagent_scroll_locked {
                                scroll_m.scroll_to_bottom();
                            }
                            view.multiagent_prev_offset_y = scroll_m.offset().y / px(1.);
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn handle_terminal_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key.as_str() == "end" {
            match self.selected_tab {
                0 => {
                    self.shogun_scroll_locked = false;
                    self.shogun_scroll_handle.scroll_to_bottom();
                }
                5 => {
                    self.multiagent_scroll_locked = false;
                    self.multiagent_scroll_handle.scroll_to_bottom();
                }
                _ => {}
            }
            cx.notify();
            return;
        }

        let bytes = key_to_bytes(&event.keystroke);
        if bytes.is_empty() {
            return;
        }
        match self.selected_tab {
            0 => {
                if let Some(ref session) = self.shogun_session {
                    session.send_bytes(&bytes);
                }
            }
            5 => {
                if let Some(ref session) = self.multiagent_session {
                    session.send_bytes(&bytes);
                }
            }
            _ => {}
        }
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
                    is_shogun,
                    &self.terminal_font,
                    cw,
                    ch,
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
        cx.notify();
    }

    fn set_control_path(&mut self, path: ControlPathType, cx: &mut Context<Self>) {
        self.settings_tab.control_path = path;
        cx.notify();
    }

    fn set_connection_backend(&mut self, backend: ConnectionBackend, cx: &mut Context<Self>) {
        self.settings_tab.connection_backend = backend;
        cx.notify();
    }

    fn toggle_accept_all_host_keys(
        &mut self,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_tab.accept_all_host_keys = !self.settings_tab.accept_all_host_keys;
        cx.notify();
    }

    pub fn save_settings(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_tab.collect(cx);
        self.terminal_font = settings.terminal.font.clone();
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
                    .w_full()
                    .h(px(24.))
                    .bg(jinmaku_bg)
                    .flex()
                    .items_center()
                    .px_2()
                    .text_color(Colors::zouge())
                    .text_size(px(12.))
                    .child(jinmaku_text),
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
                div()
                    .id(("tab", index))
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
                    .on_click(cx.listener(move |this, event, window, cx| {
                        if index != 4 {
                            this.select_tab(index, event, window, cx);
                        }
                    }))
            }))
    }
}

impl Render for ShogunWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = (self.shogun_last_gen, self.multiagent_last_gen);

        // ── PTY resize on viewport change ─────────────────────────────────────
        // Calculate the terminal dimensions from the current viewport.
        // Chrome heights: jinmaku status bar (24) + key buttons (32) + tab bar (48) = 104 px.
        //
        // Cell dimensions are measured from the active font via TextSystem::ch_advance
        // (Windows Terminal–style; see measure_cell_metrics).
        let (cw, ch) = measure_cell_metrics(&cx.text_system(), &self.terminal_font);
        {
            let vp = window.viewport_size();
            let content_w = vp.width / px(1.);
            let content_h = ((vp.height / px(1.)) - 104.0).max(ch);
            let new_cols = (content_w / cw) as u16;
            let new_rows = (content_h / ch) as u16;

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
                if let Some(s) = &self.shogun_session {
                    s.resize(new_cols, new_rows);
                }
                if let Some(s) = &self.multiagent_session {
                    s.resize(new_cols, new_rows);
                }
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        let content: gpui::AnyElement = match self.selected_tab {
            0 => {
                let session_name = load_settings()
                    .map(|s| s.sessions.shogun)
                    .unwrap_or_else(|_| "shogun".to_string());
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
                let connection_backend_selector = h_flex()
                    .gap_2()
                    .child(
                        Button::new("conn-backend-native")
                            .label("Native (russh)")
                            .when(backend == ConnectionBackend::Native, |b| b.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_connection_backend(ConnectionBackend::Native, cx);
                            })),
                    )
                    .child(
                        Button::new("conn-backend-system")
                            .label("System (ssh.exe)")
                            .when(backend == ConnectionBackend::System, |b| b.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_connection_backend(ConnectionBackend::System, cx);
                            })),
                    );
                let accept_all = self.settings_tab.accept_all_host_keys;
                let accept_all_host_keys_toggle = Button::new("accept-all-host-keys")
                    .label("ホスト鍵を常に受け入れる（known_hosts スキップ）")
                    .when(accept_all, |b| b.primary())
                    .on_click(cx.listener(Self::toggle_accept_all_host_keys));
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
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("ctrl-path-socket")
                                .label("Socket（%TEMP% ファイル）")
                                .when(current == ControlPathType::Socket, |b| b.primary())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_control_path(ControlPathType::Socket, cx);
                                })),
                        )
                        .child(
                            Button::new("ctrl-path-named-pipe")
                                .label("Named Pipe（\\\\.\\pipe\\）")
                                .when(current == ControlPathType::NamedPipe, |b| b.primary())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_control_path(ControlPathType::NamedPipe, cx);
                                })),
                        )
                        .child(
                            Button::new("ctrl-path-none")
                                .label("無効（毎回新規接続）")
                                .when(current == ControlPathType::None, |b| b.primary())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_control_path(ControlPathType::None, cx);
                                })),
                        )
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
                    #[cfg(windows)]
                    Some(control_path_selector),
                    #[cfg(not(windows))]
                    None::<gpui::Empty>,
                )
                .into_any_element()
            }
            5 => {
                let session_name = load_settings()
                    .map(|s| s.sessions.multiagent)
                    .unwrap_or_else(|_| "multiagent".to_string());
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

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::shikkoku())
            .child(div().flex_1().overflow_hidden().child(content))
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
