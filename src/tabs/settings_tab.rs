use crate::settings::{ConnectionBackend, ControlPathType, ShogunDesktopSettings};
use crate::theme::Colors;
use gpui::{Entity, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{
    h_flex,
    input::{Input, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};

pub struct SettingsTab {
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key_path: Entity<InputState>,
    password: Entity<InputState>,
    proxy_command: Entity<InputState>,
    pub accept_all_host_keys: bool,
    pub control_path: ControlPathType,
    pub connection_backend: ConnectionBackend,
    project_path: Entity<InputState>,
    shogun_session: Entity<InputState>,
    multiagent_session: Entity<InputState>,
    terminal_font: Entity<InputState>,
    agents: Vec<String>,
}

impl SettingsTab {
    pub fn new<E>(
        window: &mut Window,
        cx: &mut gpui::Context<E>,
        settings: &ShogunDesktopSettings,
    ) -> Self
    where
        E: 'static,
    {
        let host =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.ssh.host.clone()));
        let port =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.ssh.port.to_string()));
        let user =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.ssh.user.clone()));
        let key_path =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.ssh.key_path.clone()));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.password.clone())
                .masked(true)
        });
        let proxy_command = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.ssh.proxy_command.clone())
        });
        let project_path =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.project.path.clone()));
        let shogun_session = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.sessions.shogun.clone()));
        let multiagent_session = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.sessions.multiagent.clone())
        });
        let terminal_font =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.terminal.font.clone()));

        Self {
            host,
            port,
            user,
            key_path,
            password,
            proxy_command,
            accept_all_host_keys: settings.ssh.accept_all_host_keys,
            control_path: settings.ssh.control_path.clone(),
            connection_backend: settings.ssh.connection_backend.clone(),
            project_path,
            shogun_session,
            multiagent_session,
            terminal_font,
            agents: settings.sessions.agents.clone(),
        }
    }

    pub fn set_terminal_font_preset<E>(
        &self,
        font: &'static str,
        window: &mut Window,
        cx: &mut gpui::Context<E>,
    ) where
        E: 'static,
    {
        let value = SharedString::from(font);
        self.terminal_font.update(cx, |state, cx| {
            state.set_value(value.clone(), window, cx);
        });
    }

    pub fn collect<E>(&self, cx: &gpui::Context<E>) -> ShogunDesktopSettings
    where
        E: 'static,
    {
        ShogunDesktopSettings {
            ssh: crate::settings::SshSettings {
                host: self.host.read(cx).value().to_string(),
                port: self.port.read(cx).value().parse().unwrap_or(22),
                user: self.user.read(cx).value().to_string(),
                key_path: self.key_path.read(cx).value().to_string(),
                password: self.password.read(cx).unmask_value().to_string(),
                proxy_command: self.proxy_command.read(cx).value().to_string(),
                accept_all_host_keys: self.accept_all_host_keys,
                control_path: self.control_path.clone(),
                connection_backend: self.connection_backend.clone(),
            },
            project: crate::settings::ProjectSettings {
                path: self.project_path.read(cx).value().to_string(),
            },
            sessions: crate::settings::SessionSettings {
                shogun: self.shogun_session.read(cx).value().to_string(),
                multiagent: self.multiagent_session.read(cx).value().to_string(),
                agents: self.agents.clone(),
            },
            terminal: crate::settings::TerminalSettings {
                font: self.terminal_font.read(cx).value().to_string(),
            },
        }
    }
}

pub fn render_settings_tab(
    tab: &SettingsTab,
    status_message: SharedString,
    save_button: impl IntoElement,
    test_button: impl IntoElement,
    shell_button: impl IntoElement,
    connection_backend_selector: impl IntoElement,
    accept_all_host_keys_toggle: impl IntoElement,
    font_preset_buttons: impl IntoElement,
    control_path_selector: Option<impl IntoElement>,
) -> impl IntoElement {
    let mut panel = v_flex()
        .flex_1()
        .overflow_y_scrollbar()
        .gap_3()
        .p_4()
        .bg(Colors::shikkoku())
        .child(section_label("SSH設定"))
        .child(labeled_input("SSHホスト", &tab.host))
        .child(labeled_input("SSHポート", &tab.port))
        .child(labeled_input("SSHユーザー", &tab.user))
        .child(labeled_input("SSH秘密鍵パス", &tab.key_path))
        .child(labeled_input("SSHパスワード", &tab.password))
        .child(labeled_input("SSH ProxyCommand", &tab.proxy_command))
        .child(
            div()
                .text_xs()
                .text_color(Colors::zouge())
                .child("例: coder ssh --stdio %h  /  ssh -W %h:%p jump.host"),
        )
        .child(section_label("ホスト鍵"))
        .child(accept_all_host_keys_toggle)
        .child(section_label("接続バックエンド"))
        .child(connection_backend_selector);

    if let Some(selector) = control_path_selector {
        panel = panel
            .child(section_label("ControlPath（Windows）"))
            .child(selector);
    }

    panel
        .child(section_label("ターミナル設定"))
        .child(labeled_input("フォント名", &tab.terminal_font))
        .child(font_preset_buttons)
        .child(
            div()
                .text_xs()
                .text_color(Colors::zouge())
                .child("例: Moralerspace Neon HW / Cica / 任意のシステムフォント名"),
        )
        .child(section_label("プロジェクト設定"))
        .child(labeled_input("プロジェクトパス", &tab.project_path))
        .child(section_label("セッション設定"))
        .child(labeled_input("将軍セッション名", &tab.shogun_session))
        .child(labeled_input(
            "エージェントセッション名",
            &tab.multiagent_session,
        ))
        .child(
            div()
                .min_h(px(24.))
                .text_sm()
                .text_color(Colors::zouge())
                .child(status_message),
        )
        .child(
            h_flex()
                .gap_2()
                .child(save_button)
                .child(test_button)
                .child(shell_button),
        )
}

fn section_label(text: &'static str) -> impl IntoElement {
    div().text_sm().text_color(Colors::kinpaku()).child(text)
}

fn labeled_input(label: &'static str, state: &Entity<InputState>) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(Label::new(label).text_color(Colors::kinpaku()))
        .child(Input::new(state).w_full())
}
