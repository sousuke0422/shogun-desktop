use crate::settings::{load_settings, save_settings};
use crate::ssh::SshClient;
use crate::tabs::{
    render_agents_tab, render_dashboard_tab, render_settings_tab, render_shogun_tab,
    run_fetch_agents, run_fetch_dashboard, run_send_command, run_send_special_key, SettingsTab,
    ShogunTab,
};
use crate::theme::Colors;
use gpui::{
    div, prelude::*, px, size, App, Bounds, ClickEvent, Context, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, Root,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const TAB_LABELS: [&str; 4] = ["将軍", "エージェント", "戦況", "設定"];

/// State for the Shogun tab (subtask_171_001).
pub struct ShogunState {
    pub pane_content: String,
    pub is_connected: bool,
    pub error_message: Option<String>,
    #[allow(dead_code)]
    pub input_text: String,
    /// Live session is held in the background refresh `Arc<Mutex<SshClient>>`.
    #[allow(dead_code)]
    pub ssh_client: Option<SshClient>,
    pub last_refresh: SystemTime,
}

/// State for the Agents tab.
pub struct AgentsState {
    pub content: String,
    pub is_connected: bool,
    pub error_message: Option<String>,
    pub last_refresh: SystemTime,
}

impl Default for AgentsState {
    fn default() -> Self {
        Self {
            content: String::new(),
            is_connected: false,
            error_message: None,
            last_refresh: SystemTime::UNIX_EPOCH,
        }
    }
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

impl Default for ShogunState {
    fn default() -> Self {
        Self {
            pane_content: String::new(),
            is_connected: false,
            error_message: None,
            input_text: String::new(),
            ssh_client: None,
            last_refresh: SystemTime::UNIX_EPOCH,
        }
    }
}

fn capture_pane_command(session: &str) -> String {
    format!("tmux capture-pane -t {session}:main -p -e -S -500")
}

fn ssh_error_message(err: &anyhow::Error) -> String {
    format!("SSH接続失敗: {err}")
}

pub struct ShogunWindow {
    selected_tab: usize,
    settings_tab: SettingsTab,
    shogun_tab: ShogunTab,
    shogun_state: ShogunState,
    agents_state: AgentsState,
    dashboard_state: DashboardState,
    shogun_session: String,
    status_message: SharedString,
}

impl ShogunWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings().unwrap_or_default();
        let shogun_session = settings.sessions.shogun.clone();
        Self {
            selected_tab: 0,
            settings_tab: SettingsTab::new(window, cx, &settings),
            shogun_tab: ShogunTab::new(window, cx),
            shogun_state: ShogunState::default(),
            agents_state: AgentsState::default(),
            dashboard_state: DashboardState::default(),
            shogun_session,
            status_message: SharedString::default(),
        }
    }

    /// Auto SSH connect + 3s tmux pane refresh (subtask_171_001).
    pub fn start_shogun_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let settings = load_settings().unwrap_or_default();
            let session = settings.sessions.shogun.clone();
            let settings_connect = settings.clone();

            let connect = cx
                .background_executor()
                .spawn(async move { SshClient::from_settings(&settings_connect) })
                .await;

            let client = match connect {
                Ok(client) => client,
                Err(err) => {
                    let msg = ssh_error_message(&err);
                    let _ = this.update(cx, |view, cx| {
                        view.shogun_state.is_connected = false;
                        view.shogun_state.error_message = Some(msg);
                        cx.notify();
                    });
                    return;
                }
            };

            let client = Arc::new(Mutex::new(client));

            let _ = this.update(cx, |view, cx| {
                view.shogun_state.is_connected = true;
                view.shogun_state.error_message = None;
                cx.notify();
            });

            loop {
                let client = Arc::clone(&client);
                let session = session.clone();
                let refresh = cx
                    .background_executor()
                    .spawn(async move {
                        let guard = client
                            .lock()
                            .map_err(|_| anyhow::anyhow!("SSHクライアントのロックに失敗しました"))?;
                        guard.exec(&capture_pane_command(&session))
                    })
                    .await;

                match refresh {
                    Ok(content) => {
                        let now = SystemTime::now();
                        let _ = this.update(cx, |view, cx| {
                            view.shogun_state.pane_content = content;
                            view.shogun_state.is_connected = true;
                            view.shogun_state.error_message = None;
                            view.shogun_state.last_refresh = now;
                            cx.notify();
                        });
                    }
                    Err(err) => {
                        let msg = ssh_error_message(&err);
                        let _ = this.update(cx, |view, cx| {
                            view.shogun_state.is_connected = false;
                            view.shogun_state.error_message = Some(msg);
                            cx.notify();
                        });
                        break;
                    }
                }

                cx.background_executor()
                    .timer(Duration::from_secs(1))
                    .await;
            }
        })
        .detach();
    }

    pub fn send_shogun_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shogun_state.is_connected {
            return;
        }
        let text = self.shogun_tab.command_input.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        self.shogun_tab.command_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });

        let settings = load_settings().unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { run_send_command(settings, text) })
                .await;

            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(content) if !content.is_empty() => {
                        view.shogun_state.pane_content = content;
                        view.shogun_state.last_refresh = SystemTime::now();
                    }
                    Err(err) => {
                        view.shogun_state.error_message =
                            Some(format!("コマンド送信失敗: {err}"));
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn send_shogun_special_key(
        &mut self,
        key_value: &'static str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.shogun_state.is_connected {
            return;
        }
        let settings = load_settings().unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { run_send_special_key(settings, key_value) })
                .await;

            let _ = this.update(cx, |view, cx| {
                if let Err(err) = result {
                    view.shogun_state.error_message =
                        Some(format!("特殊キー送信失敗: {err}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Start the agents status auto-refresh loop (10 s interval).
    pub fn start_agents_background(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let settings = load_settings().unwrap_or_default();
            let result = cx
                .background_executor()
                .spawn(async move { run_fetch_agents(settings) })
                .await;

            let now = SystemTime::now();
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(content) => {
                        view.agents_state.content = content;
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
                .spawn(async move { run_fetch_agents(settings) })
                .await;

            let now = SystemTime::now();
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(content) => {
                        view.agents_state.content = content;
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

    pub fn save_settings(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_tab.collect(cx);
        self.shogun_session = settings.sessions.shogun.clone();
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
            .children((0..4).map(|index| {
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
                    } else {
                        Colors::muted()
                    })
                    .child(TAB_LABELS[index])
                    .on_click(cx.listener(move |this, event, window, cx| {
                        this.select_tab(index, event, window, cx);
                    }))
            }))
    }
}

impl Render for ShogunWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: gpui::AnyElement = match self.selected_tab {
            0 => render_shogun_tab(
                &self.shogun_tab,
                &self.shogun_state,
                &self.shogun_session,
                cx,
            )
            .into_any_element(),
            1 => render_agents_tab(&self.agents_state, cx).into_any_element(),
            2 => render_dashboard_tab(&self.dashboard_state, cx).into_any_element(),
            _ => {
                let save_btn = Button::new("save-settings")
                    .primary()
                    .label("保存")
                    .on_click(cx.listener(Self::save_settings));
                let test_btn = Button::new("test-ssh")
                    .label("SSH接続テスト")
                    .on_click(cx.listener(Self::test_ssh));
                render_settings_tab(
                    &self.settings_tab,
                    self.status_message.clone(),
                    save_btn,
                    test_btn,
                )
                .into_any_element()
            }
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
                let win = ShogunWindow::new(window, cx);
                win.start_shogun_background(cx);
                win.start_agents_background(cx);
                win.start_dashboard_background(cx);
                win
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("open main window");
}
