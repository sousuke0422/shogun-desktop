use crate::settings::{load_settings, save_settings, ConnectionBackend, ControlPathType};
use crate::ssh::SshClient;
use crate::tabs::{
    fetch_agent_cards, render_agents_tab, render_dashboard_tab, render_settings_tab,
    render_terminal_tab, render_terminal_tab_disconnected, render_terminal_tab_empty,
    render_terminal_tab_error, run_fetch_agents, run_fetch_dashboard, SettingsTab,
};
use crate::tabs::AgentCardData;
use crate::terminal::keys::key_to_bytes;
use crate::terminal::pty_session;
use crate::terminal::TerminalSession;
use crate::theme::Colors;
use gpui::{
    div, prelude::*, px, size, App, Bounds, ClickEvent, Context, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Window, WindowBounds, WindowOptions,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, Root,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const TAB_LABELS: [&str; 6] = ["将軍", "エージェント", "戦況", "設定", "──", "家老陣"];

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
    shogun_last_gen: u64,
    multiagent_last_gen: u64,
    terminal_refresh_started: bool,
    status_message: SharedString,
}

impl ShogunWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings().unwrap_or_default();
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
            shogun_last_gen: 0,
            multiagent_last_gen: 0,
            terminal_refresh_started: false,
            status_message: SharedString::default(),
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
                .spawn(async move { pty_session::spawn(&ssh, &tmux_session, 220, 50, control_path) })
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
                .spawn(async move { pty_session::spawn(&ssh, &tmux_session, 220, 50, control_path) })
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
                            scroll_s.scroll_to_bottom();
                        }
                        if m_changed {
                            scroll_m.scroll_to_bottom();
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn handle_terminal_key(&mut self, event: &KeyDownEvent) {
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
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(err) = error {
            return render_terminal_tab_error(err.clone(), cx).into_any_element();
        }
        if let Some(session) = session {
            if session.is_connected() {
                let snap = session
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                render_terminal_tab(&snap, scroll_handle, cx).into_any_element()
            } else {
                let btn_id = if is_shogun { "reconnect-shogun" } else { "reconnect-multiagent" };
                let reconnect_btn = Button::new(btn_id)
                    .label("再接続")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_shogun {
                            this.start_shogun_session(cx);
                        } else {
                            this.start_multiagent_session(cx);
                        }
                    }));
                render_terminal_tab_disconnected(reconnect_btn, cx).into_any_element()
            }
        } else {
            render_terminal_tab_empty(cx).into_any_element()
        }
    }

    /// Start the agents status auto-refresh loop (10 s interval).
    pub fn start_agents_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
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
                        view.agents_state.error_message =
                            Some(format!("SSH接続失敗: {err}"));
                    }
                }
                cx.notify();
            });

            cx.background_executor()
                .timer(Duration::from_secs(10))
                .await;
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
                        view.agents_state.error_message =
                            Some(format!("SSH接続失敗: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Start the dashboard auto-refresh loop (30 s interval).
    pub fn start_dashboard_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
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
                        view.dashboard_state.error_message =
                            Some(format!("SSH接続失敗: {err}"));
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

    pub fn save_settings(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_tab.collect(cx);
        self.status_message = match save_settings(&settings) {
            Ok(()) => "設定を保存しました".into(),
            Err(err) => format!("保存失敗: {err}").into(),
        };
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

        let content: gpui::AnyElement = match self.selected_tab {
            0 => self.render_terminal_for_session(
                &self.shogun_session,
                &self.shogun_error,
                &self.shogun_scroll_handle,
                true,
                cx,
            ),
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
                    connection_backend_selector,
                    #[cfg(windows)]
                    Some(control_path_selector),
                    #[cfg(not(windows))]
                    None::<gpui::Empty>,
                )
                .into_any_element()
            }
            5 => self.render_terminal_for_session(
                &self.multiagent_session,
                &self.multiagent_error,
                &self.multiagent_scroll_handle,
                false,
                cx,
            ),
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
            .tab_stop(true)
            .on_key_down(cx.listener(|this, event, _window, _cx| {
                this.handle_terminal_key(event);
            }))
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
