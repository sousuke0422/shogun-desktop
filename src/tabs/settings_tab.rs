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
        let host = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.host.clone())
                .placeholder("192.168.x.x / ssh config のホスト名")
        });
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.port.to_string())
                .placeholder("22")
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.user.clone())
                .placeholder("空欄可（Coder 等 ProxyCommand 先で決まる接続）")
        });
        let key_path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.key_path.clone())
                .placeholder(r"C:\Users\you\.ssh\id_ed25519（空欄 = ssh-agent）")
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.password.clone())
                .placeholder("鍵も ssh-agent も使えない場合のみ")
                .masked(true)
        });
        let proxy_command = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ssh.proxy_command.clone())
                .placeholder("coder ssh --stdio %h / ssh -W %h:%p jump.host")
        });
        let project_path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.project.path.clone())
                .placeholder("/mnt/c/Users/you/work/multi-agent-shogun")
        });
        let shogun_session = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.sessions.shogun.clone())
                .placeholder("shogun")
        });
        let multiagent_session = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.sessions.multiagent.clone())
                .placeholder("multiagent")
        });
        let terminal_font = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.terminal.font.clone())
                .placeholder("Moralerspace Neon HW")
        });

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
    let mut advanced = section_card(
        "接続の詳細",
        Some("普段は変更不要。接続がうまくいかない時だけ触る"),
    )
    .child(labeled_input("ProxyCommand", &tab.proxy_command))
    .child(hint("Coder / 踏み台経由はここ。%h がホスト名に展開される"))
    .child(field_label("接続バックエンド"))
    .child(connection_backend_selector)
    .child(hint(
        "Native (russh) 推奨。System は OpenSSH (ssh.exe) を使う互換モード",
    ))
    .child(field_label("ホスト鍵"))
    .child(accept_all_host_keys_toggle);

    if let Some(selector) = control_path_selector {
        advanced = advanced
            .child(field_label(
                "ControlPath（Windows / System バックエンド用）",
            ))
            .child(selector);
    }

    v_flex()
        .flex_1()
        .bg(Colors::shikkoku())
        .child(
            v_flex()
                .flex_1()
                .overflow_y_scrollbar()
                .gap_4()
                .p_4()
                .child(
                    section_card(
                        "SSH接続",
                        Some("ユーザー名は空欄でもよい — Coder のように接続先がユーザーを決める場合はホストだけで繋がる"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(labeled_input("ホスト", &tab.host)))
                            .child(div().w(px(120.)).child(labeled_input("ポート", &tab.port))),
                    )
                    .child(labeled_input("ユーザー名", &tab.user)),
                )
                .child(
                    section_card(
                        "認証",
                        Some("上から順に試す: 秘密鍵 → ssh-agent → パスワード。通常は鍵パスだけ埋めれば良い"),
                    )
                    .child(labeled_input("秘密鍵パス", &tab.key_path))
                    .child(labeled_input("パスワード", &tab.password)),
                )
                .child(
                    section_card("ターミナル", None)
                        .child(labeled_input("フォント名", &tab.terminal_font))
                        .child(font_preset_buttons)
                        .child(hint("システムにインストール済みの等幅フォント名を指定")),
                )
                .child(
                    section_card(
                        "プロジェクト / セッション",
                        Some("リモート側 multi-agent-shogun のパスと、監視する tmux セッション名"),
                    )
                    .child(labeled_input("プロジェクトパス", &tab.project_path))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(labeled_input("将軍セッション", &tab.shogun_session)),
                            )
                            .child(div().flex_1().child(labeled_input(
                                "エージェントセッション",
                                &tab.multiagent_session,
                            ))),
                    ),
                )
                .child(advanced),
        )
        // Action bar pinned below the scroll area: the buttons and the
        // save/test status stay visible no matter where the form is
        // scrolled (they used to live at the bottom of the scroll).
        .child(
            h_flex()
                .gap_2()
                .p_3()
                .items_center()
                .border_t_1()
                .border_color(Colors::border())
                .bg(Colors::sumi())
                .child(save_button)
                .child(test_button)
                .child(shell_button)
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(Colors::zouge())
                        .text_right()
                        .child(status_message),
                ),
        )
}

/// A visually separated settings group: bordered card with a bold title and
/// an optional one-line description.
fn section_card(title: &'static str, description: Option<&'static str>) -> gpui::Div {
    let mut header = v_flex().gap_1().child(
        div()
            .text_base()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(Colors::kinpaku())
            .child(title),
    );
    if let Some(desc) = description {
        header = header.child(hint(desc));
    }
    v_flex()
        .gap_3()
        .p_4()
        .bg(Colors::sumi())
        .rounded_lg()
        .border_1()
        .border_color(Colors::border())
        .child(header)
}

fn field_label(text: &'static str) -> impl IntoElement {
    div().text_sm().text_color(Colors::kinpaku()).child(text)
}

fn hint(text: &'static str) -> gpui::Div {
    div().text_xs().text_color(Colors::zouge()).child(text)
}

fn labeled_input(label: &'static str, state: &Entity<InputState>) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(Label::new(label).text_sm().text_color(Colors::kinpaku()))
        .child(Input::new(state).w_full())
}
