use crate::ansi::parse_ansi_spans;
use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{AgentsState, ShogunWindow};
use gpui::{AlignItems, Context, IntoElement, ParentElement, Styled, div, prelude::*, px, rgb};
use gpui_component::{Sizable, button::Button, scroll::ScrollableElement, v_flex};
use shogun_core::{
    StatusCategory, build_agent_card, build_karo_card, status_category, status_indicator,
    truncate_summary,
};

const CARD_BG: u32 = 0x242424;

pub use shogun_core::AgentCardData;

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

/// Fetch YAML-driven card data for each configured agent via SSH.
pub fn fetch_agent_cards(
    ssh: &SshClient,
    project_path: &str,
    agents: &[String],
) -> Vec<AgentCardData> {
    agents
        .iter()
        .filter_map(|name| fetch_single_agent_card(ssh, project_path, name))
        .collect()
}

fn fetch_single_agent_card(
    ssh: &SshClient,
    project_path: &str,
    name: &str,
) -> Option<AgentCardData> {
    if name == "karo" {
        return fetch_karo_card(ssh, project_path);
    }

    let base = format!("{project_path}/queue");
    let task_yaml = ssh_cat(ssh, &format!("{base}/tasks/{name}.yaml"));
    let inbox_yaml = ssh_cat(ssh, &format!("{base}/inbox/{name}.yaml"));
    let report_yaml = ssh_cat(ssh, &format!("{base}/reports/{name}_report.yaml"));

    build_agent_card(
        name,
        task_yaml.as_deref(),
        inbox_yaml.as_deref(),
        report_yaml.as_deref(),
    )
}

fn fetch_karo_card(ssh: &SshClient, project_path: &str) -> Option<AgentCardData> {
    let base = format!("{project_path}/queue");
    let cmd_yaml = ssh_cat(ssh, &format!("{base}/shogun_to_karo.yaml"));
    let inbox_yaml = ssh_cat(ssh, &format!("{base}/inbox/karo.yaml"));

    build_karo_card(cmd_yaml.as_deref(), inbox_yaml.as_deref())
}

fn ssh_cat(ssh: &SshClient, path: &str) -> Option<String> {
    let cmd = format!("cat {path} 2>/dev/null || true");
    match ssh.exec(&cmd) {
        Ok(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

fn status_color(status: &str) -> gpui::Rgba {
    match status_category(status) {
        StatusCategory::Active => Colors::kinpaku(),
        StatusCategory::Done => Colors::matsuba(),
        StatusCategory::Idle | StatusCategory::Unknown => Colors::muted(),
    }
}

fn render_agent_card(card: &AgentCardData) -> impl IntoElement {
    let status_col = status_color(&card.status);
    let inbox_color = if card.inbox_unread > 0 {
        Colors::kurenai()
    } else {
        Colors::muted()
    };
    let summary = if card.summary.is_empty() {
        String::new()
    } else {
        truncate_summary(&card.summary, 2)
    };

    div()
        .flex_1()
        .min_w(px(200.))
        .max_w(px(360.))
        .m_1()
        .p_3()
        .pb_4()
        .rounded(px(6.))
        .bg(rgb(CARD_BG))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_family(MONO_FONT)
                .text_color(Colors::kinpaku())
                .child(card.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(Colors::zouge())
                .child(card.task_id.clone()),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(status_col)
                .child(format!(
                    "{} {}",
                    card.status,
                    status_indicator(&card.status)
                )),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(inbox_color)
                .child(format!("inbox: {}", card.inbox_unread)),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(Colors::muted())
                .child(format!("{}更新", card.last_report_at)),
        )
        .when(!summary.is_empty(), |el| {
            el.child(
                div()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(Colors::zouge())
                    .line_height(px(16.))
                    .child(summary),
            )
        })
}

fn render_card_grid(cards: &[AgentCardData]) -> impl IntoElement {
    if cards.is_empty() {
        return div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::zouge())
            .child("（エージェントカード未取得）");
    }

    let rows: Vec<_> = cards.chunks(3).collect();
    v_flex().gap_1().children(rows.into_iter().map(|row| {
        let mut children: Vec<gpui::AnyElement> = row
            .iter()
            .map(|c| render_agent_card(c).into_any_element())
            .collect();
        for _ in row.len()..3 {
            children.push(
                div()
                    .flex_1()
                    .min_w(px(200.))
                    .max_w(px(360.))
                    .m_1()
                    .into_any_element(),
            );
        }
        let mut row = div().flex().flex_row().w_full();
        row.style().align_items = Some(AlignItems::Stretch);
        row.children(children)
    }))
}

pub fn render_agents_tab(state: &AgentsState, cx: &mut Context<ShogunWindow>) -> impl IntoElement {
    let bg_color = if state.is_connected {
        Colors::matsuba()
    } else {
        Colors::kurenai()
    };

    let status_text = if let Some(err) = &state.error_message {
        err.clone()
    } else if state.is_connected {
        let secs = state.last_refresh.elapsed().unwrap_or_default().as_secs();
        format!("布陣一覧 — {}秒前に更新", secs)
    } else {
        "未接続".to_string()
    };

    let body: gpui::AnyElement = if let Some(err) = &state.error_message {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::kurenai())
            .child(format!("❌ {err}"))
            .into_any_element()
    } else if !state.cards.is_empty() {
        render_card_grid(&state.cards).into_any_element()
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
                .h(px(crate::window::STATUS_BAR_HEIGHT_PX))
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .bg(bg_color)
                .text_color(Colors::zouge())
                .text_size(px(12.))
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
                // See dashboard_tab: min-height:0 keeps the status bar from
                // being squeezed by tall scrollable content.
                .min_h_0()
                .w_full()
                .bg(Colors::shikkoku())
                .overflow_y_scrollbar()
                .p_2()
                .child(body),
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
