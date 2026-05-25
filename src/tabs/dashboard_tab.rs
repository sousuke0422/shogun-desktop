use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{DashboardState, ShogunWindow};
use gpui::{
    div, prelude::*, px, rgb, Context, Fill, Hsla, IntoElement, ParentElement, StyleRefinement,
    Styled, TextStyleRefinement, Window,
};
use gpui_component::{
    button::Button,
    highlighter::HighlightTheme,
    scroll::ScrollableElement,
    text::{TextView, TextViewStyle},
    v_flex, Theme, ThemeMode, Sizable,
};

/// Markdown inline `` `code` `` uses `cx.theme().accent` as background (gpui-component node.rs).
/// On a dark page with a light system theme, accent is near-white and body text stays pale — unreadable.
fn apply_dashboard_markdown_theme(window: &mut Window, cx: &mut Context<ShogunWindow>) {
    Theme::change(ThemeMode::Dark, Some(window), cx);
    let theme = Theme::global_mut(cx);
    theme.colors.accent = Hsla::from(Colors::sumi());
    theme.colors.accent_foreground = Hsla::from(Colors::zouge());
    theme.colors.foreground = Hsla::from(Colors::zouge());
}

fn dashboard_text_view_style() -> TextViewStyle {
    TextViewStyle {
        highlight_theme: HighlightTheme::default_dark(),
        is_dark: true,
        code_block: StyleRefinement {
            background: Some(Fill::from(Colors::sumi())),
            text: Some(TextStyleRefinement {
                color: Some(Hsla::from(Colors::zouge())),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

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
        apply_dashboard_markdown_theme(window, cx);
        TextView::markdown("dashboard-md", state.content.clone(), window, cx)
            .style(dashboard_text_view_style())
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
