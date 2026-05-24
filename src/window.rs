use crate::settings::{load_settings, save_settings};
use crate::ssh::SshClient;
use crate::tabs::{
    render_agents_tab, render_dashboard_tab, render_settings_tab, render_shogun_tab, SettingsTab,
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

const TAB_LABELS: [&str; 4] = ["将軍", "エージェント", "戦況", "設定"];

pub struct ShogunWindow {
    selected_tab: usize,
    settings_tab: SettingsTab,
    status_message: SharedString,
}

impl ShogunWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings().unwrap_or_default();
        Self {
            selected_tab: 0,
            settings_tab: SettingsTab::new(window, cx, &settings),
            status_message: SharedString::default(),
        }
    }

    fn select_tab(&mut self, index: usize, _event: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_tab = index;
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
            let message = match tokio::task::spawn_blocking(move || {
                SshClient::from_settings(&settings).and_then(|mut client| {
                    if !client.is_connected() {
                        anyhow::bail!("SSH未接続");
                    }
                    let output = client.exec("echo ok")?;
                    Ok(format!("✅ 接続成功 (Host: {host}, echo: {})", output.trim()))
                })
            })
            .await
            {
                Ok(Ok(msg)) => msg,
                Ok(Err(err)) => format!("❌ 接続失敗: {err}"),
                Err(err) => format!("❌ テストエラー: {err}"),
            };

            let _ = this.update(cx, |view, cx| {
                view.status_message = message.into();
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
            0 => render_shogun_tab(window, cx).into_any_element(),
            1 => render_agents_tab(window, cx).into_any_element(),
            2 => render_dashboard_tab(window, cx).into_any_element(),
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
            let view = cx.new(|cx| ShogunWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("open main window");
}
