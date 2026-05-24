use crate::settings::ShogunDesktopSettings;
use crate::theme::Colors;
use gpui::{
    div, prelude::*, Entity, IntoElement, ParentElement, SharedString, Styled, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};

pub struct SettingsTab {
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key_path: Entity<InputState>,
    password: Entity<InputState>,
    project_path: Entity<InputState>,
    shogun_session: Entity<InputState>,
    multiagent_session: Entity<InputState>,
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
        let host = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.ssh.host.clone())
        });
        let port = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.ssh.port.to_string())
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.ssh.user.clone())
        });
        let key_path = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.ssh.key_path.clone())
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.password.clone())
                .masked(true)
        });
        let project_path = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.project.path.clone())
        });
        let shogun_session = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.sessions.shogun.clone())
        });
        let multiagent_session = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.sessions.multiagent.clone())
        });

        Self {
            host,
            port,
            user,
            key_path,
            password,
            project_path,
            shogun_session,
            multiagent_session,
        }
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
            },
            project: crate::settings::ProjectSettings {
                path: self.project_path.read(cx).value().to_string(),
            },
            sessions: crate::settings::SessionSettings {
                shogun: self.shogun_session.read(cx).value().to_string(),
                multiagent: self.multiagent_session.read(cx).value().to_string(),
            },
        }
    }

}

pub fn render_settings_tab(
    tab: &SettingsTab,
    status_message: SharedString,
    save_button: impl IntoElement,
    test_button: impl IntoElement,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_3()
        .p_4()
        .bg(Colors::shikkoku())
        .child(section_label("SSH設定"))
        .child(labeled_input("SSHホスト", &tab.host))
        .child(labeled_input("SSHポート", &tab.port))
        .child(labeled_input("SSHユーザー", &tab.user))
        .child(labeled_input("SSH秘密鍵パス", &tab.key_path))
        .child(labeled_input("SSHパスワード", &tab.password))
        .child(section_label("プロジェクト設定"))
        .child(labeled_input("プロジェクトパス", &tab.project_path))
        .child(section_label("セッション設定"))
        .child(labeled_input("将軍セッション名", &tab.shogun_session))
        .child(labeled_input("エージェントセッション名", &tab.multiagent_session))
        .child(
            h_flex()
                .gap_2()
                .child(save_button)
                .child(test_button),
        )
        .child(
            div()
                .text_sm()
                .text_color(Colors::zouge())
                .child(status_message),
        )
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(Colors::kinpaku())
        .child(text)
}

fn labeled_input(label: &'static str, state: &Entity<InputState>) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(Label::new(label).text_color(Colors::kinpaku()))
        .child(Input::new(state).w_full())
}
