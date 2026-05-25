use crate::ansi::parse_ansi_spans;
use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{AgentsState, ShogunWindow};
use gpui::{div, prelude::*, px, rgb, Context, IntoElement, ParentElement, Styled};
use gpui_component::{button::Button, scroll::ScrollableElement, v_flex, Sizable};

pub fn run_fetch_agents(settings: ShogunDesktopSettings) -> anyhow::Result<String> {
    if settings.project.path.is_empty() {
        anyhow::bail!("プロジェクトパスが未設定です（設定タブで project_path を入力してください）");
    }
    let client = SshClient::from_settings(&settings)?;
    client.exec(&format!(
        "bash {}/scripts/agent_status.sh",
        settings.project.path
    ))
}

pub fn render_agents_tab(
    state: &AgentsState,
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
        format!("布陣一覧 — {}秒前に更新", secs)
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
            .child("（稼働確認中...）")
            .into_any_element()
    } else {
        render_ansi_lines(&state.content).into_any_element()
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
                    Button::new("agents-refresh")
                        .small()
                        .label("更新")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_agents(cx);
                        })),
                ),
        )
        .child(
            div()
                .id("agents-pane-content")
                .flex_1()
                .w_full()
                .bg(Colors::shikkoku())
                .overflow_y_scrollbar()
                .p_2()
                .child(inner),
        )
}

fn render_ansi_lines(raw: &str) -> impl IntoElement {
    let lines = parse_ansi_spans(raw);
    v_flex().children(lines.into_iter().map(|spans| {
        div()
            .flex()
            .flex_row()
            .children(spans.into_iter().map(|span| {
                let color = span
                    .rgb
                    .map(|(r, g, b)| rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
                    .unwrap_or(Colors::zouge());
                div()
                    .text_sm()
                    .font_family(MONO_FONT)
                    .text_color(color)
                    .child(span.text)
            }))
    }))
}
