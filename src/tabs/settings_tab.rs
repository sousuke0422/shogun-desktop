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
    font_features: Entity<InputState>,
    /// No UI yet — carried through so saving does not reset them
    /// (edit settings.json directly).
    font_size: f32,
    line_height: f32,
    pub desktop_notifications: bool,
    pub desktop_notifications_multiagent: bool,
    pub tmux_forward_titles: bool,
    pub tsf: bool,
    pub terminal_identity: crate::settings::TerminalIdentity,
    pub term_name: crate::settings::TermName,
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
        let font_features = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.terminal.font_features.clone())
                .placeholder("例: ss01, ss03（空欄 = フォント既定のみ）")
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
            font_features,
            font_size: settings.terminal.font_size,
            line_height: settings.terminal.line_height,
            desktop_notifications: settings.terminal.desktop_notifications,
            desktop_notifications_multiagent: settings.terminal.desktop_notifications_multiagent,
            tmux_forward_titles: settings.terminal.tmux_forward_titles,
            tsf: settings.terminal.tsf,
            terminal_identity: settings.terminal.identity,
            term_name: settings.terminal.term,
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
                font_features: self.font_features.read(cx).value().to_string(),
                font_size: self.font_size,
                line_height: self.line_height,
                desktop_notifications: self.desktop_notifications,
                desktop_notifications_multiagent: self.desktop_notifications_multiagent,
                tmux_forward_titles: self.tmux_forward_titles,
                tsf: self.tsf,
                identity: self.terminal_identity,
                term: self.term_name,
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
    notification_toggles: impl IntoElement,
    tsf_toggle: impl IntoElement,
    terminal_identity_selector: impl IntoElement,
    term_name_selector: impl IntoElement,
    term_name_warning: Option<SharedString>,
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
        // size_full (percent of the parent's definite size), NOT flex_1: a
        // flex item's automatic min-height is its content height, so with
        // flex_1 this column grew past the viewport, the action bar fell off
        // screen and the inner scroll area never scrolled.
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            v_flex()
                .flex_1()
                // Flex items default to min-height:auto (= content height);
                // without this the scroll area grows to fit the cards, never
                // scrolls, and pushes the action bar off-screen.
                .min_h(px(0.))
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
                        .child(hint("システムにインストール済みの等幅フォント名を指定"))
                        .child(field_label("フォント機能（OpenType features・機能ごとに有効化）"))
                        .child(Input::new(&tab.font_features).w_full())
                        .child(hint(
                            "4文字タグをカンマ区切りで。Moralerspace/Monaspace: ss01 ==/!=・ss02 >=/<=・ss03 矢印・ss04 </ />・ss05 |>・ss07 ::・ss08 .=、calt=0 で texture healing 停止。保存で即時反映",
                        ))
                        .child(field_label("デスクトップ通知（OSC 9 / 777）"))
                        .child(notification_toggles)
                        .child(field_label("日本語入力（IME / TSF）"))
                        .child(tsf_toggle)
                        .child(hint(
                            "オンでタスクバーの あ/A がこの窓に追従し、Google 日本語入力・Mozc が正しく変換できる。オフは従来の IMM32 のみ（インジケータ非追従）。切替は次に端末へ入った時に反映",
                        ))
                        .child(field_label("端末の名乗り（XTVERSION）"))
                        .child(terminal_identity_selector)
                        .child(hint(
                            "アプリが端末名で機能を出し分ける時に効く。Ghostty偽装で yazi 等が kitty 画像を有効化する（実装済み能力のみ名乗る）",
                        ))
                        .child(field_label("TERM（リモートの terminfo 検索キー）"))
                        .child(term_name_selector)
                        .children(term_name_warning.map(|w| {
                            div()
                                .text_sm()
                                .text_color(crate::theme::Colors::kurenai())
                                .child(w)
                        }))
                        .child(hint(
                            "見ているタブの通知は出ない（Ghostty と同じフォーカス抑止）。\
                             家老陣は多エージェントで鳴りやすいため既定オフ",
                        )),
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
