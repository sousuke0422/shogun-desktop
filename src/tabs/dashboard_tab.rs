use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{DashboardState, ShogunWindow};
use gpui::{div, prelude::*, px, rgb, Context, IntoElement, ParentElement, Styled, Window};
use gpui_component::{
    button::Button,
    highlighter::HighlightTheme,
    scroll::ScrollableElement,
    text::{TextView, TextViewStyle},
    v_flex,
    Sizable,
};

pub fn run_fetch_dashboard(settings: ShogunDesktopSettings) -> anyhow::Result<String> {
    if settings.project.path.is_empty() {
        anyhow::bail!("プロジェクトパスが未設定です（設定タブで project_path を入力してください）");
    }
    let client = SshClient::from_settings(&settings)?;
    client.exec(&format!("cat {}/dashboard.md", settings.project.path))
}

pub fn render_dashboard_tab(
    state: &DashboardState,
    window: &mut Window,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let bg_color = if state.is_connected {
        Colors::matsuba()
    } else {
        Colors::kurenai()
    };

    let status_text = if let Some(err) = &state.error_message {
        err.clone()
    } else if state.is_connected {
        let secs = state
            .last_refresh
            .elapsed()
            .unwrap_or_default()
            .as_secs();
        format!("戦況 — {}秒前に更新", secs)
    } else {
        "未接続".to_string()
    };

    let inner: gpui::AnyElement = if let Some(err) = &state.error_message {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::kurenai())
            .child(format!("❌ {err}"))
            .into_any_element()
    } else if state.content.is_empty() {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::zouge())
            .child("（dashboard.md 読み込み中...）")
            .into_any_element()
    } else {
        TextView::markdown("dashboard-md", state.content.clone(), window, cx)
            .style(TextViewStyle {
                highlight_theme: HighlightTheme::default_dark(),
                is_dark: true,
                ..Default::default()
            })
            .text_color(Colors::zouge())
            .scrollable(true)
            .into_any_element()
    };

    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(
            div()
                .w_full()
                .h(px(48.))
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .bg(bg_color)
                .text_color(rgb(0xFFFFFF))
                .text_sm()
                .child(status_text)
                .child(
                    Button::new("dashboard-refresh")
                        .small()
                        .label("更新")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_dashboard(cx);
                        })),
                ),
        )
        .child(
            div()
                .id("dashboard-pane-content")
                .flex_1()
                .w_full()
                .bg(Colors::shikkoku())
                .overflow_y_scrollbar()
                .p_2()
                .child(inner),
        )
}
