use crate::ansi::parse_ansi_spans;
use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{AgentsState, ShogunWindow};
use gpui::{
    div, prelude::*, px, rgb, Context, IntoElement, ParentElement, Styled,
};
use gpui_component::{button::Button, h_flex, scroll::ScrollableElement, v_flex, Sizable};
use serde_yml::Value;

const PLACEHOLDER: &str = "---";
const CARD_BG: u32 = 0x242424;

/// Structured agent status for the card grid.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct AgentCardData {
    pub name: String,
    pub task_id: String,
    pub status: String,
    pub inbox_unread: usize,
    pub last_report_at: String,
    pub summary: String,
}

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
        .map(|name| fetch_single_agent_card(ssh, project_path, name))
        .collect()
}

fn fetch_single_agent_card(ssh: &SshClient, project_path: &str, name: &str) -> AgentCardData {
    let base = format!("{project_path}/queue");
    let task_path = format!("{base}/tasks/{name}.yaml");
    let inbox_path = format!("{base}/inbox/{name}.yaml");
    let report_path = format!("{base}/reports/{name}_report.yaml");

    let task_yaml = ssh_cat(ssh, &task_path);
    let inbox_yaml = ssh_cat(ssh, &inbox_path);
    let report_yaml = ssh_cat(ssh, &report_path);

    let (task_id, status) = parse_task_yaml(&task_yaml);
    let inbox_unread = parse_inbox_unread(&inbox_yaml);
    let (last_report_at, summary) = parse_report_yaml(&report_yaml);

    AgentCardData {
        name: name.to_string(),
        task_id,
        status,
        inbox_unread,
        last_report_at,
        summary,
    }
}

fn ssh_cat(ssh: &SshClient, path: &str) -> Option<String> {
    let cmd = format!("cat {path} 2>/dev/null || true");
    match ssh.exec(&cmd) {
        Ok(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

fn parse_yaml(raw: &Option<String>) -> Option<Value> {
    let raw = raw.as_ref()?;
    serde_yml::from_str(raw).ok()
}

fn yaml_str(v: &Value, keys: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in keys {
        cur = cur.get(key)?;
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn parse_task_yaml(raw: &Option<String>) -> (String, String) {
    let Some(v) = parse_yaml(raw) else {
        return (PLACEHOLDER.into(), PLACEHOLDER.into());
    };
    let task_id = yaml_str(&v, &["task", "task_id"])
        .or_else(|| yaml_str(&v, &["task_id"]))
        .unwrap_or_else(|| PLACEHOLDER.into());
    let status = yaml_str(&v, &["task", "status"])
        .or_else(|| yaml_str(&v, &["status"]))
        .unwrap_or_else(|| PLACEHOLDER.into());
    (task_id, status)
}

pub fn parse_inbox_unread(raw: &Option<String>) -> usize {
    let Some(v) = parse_yaml(raw) else {
        return 0;
    };
    let messages = v.get("messages").and_then(|m| m.as_sequence());
    let Some(msgs) = messages else {
        return 0;
    };
    msgs.iter()
        .filter(|m| m.get("read").and_then(|r| r.as_bool()) == Some(false))
        .count()
}

pub fn parse_report_yaml(raw: &Option<String>) -> (String, String) {
    let Some(v) = parse_yaml(raw) else {
        return (PLACEHOLDER.into(), String::new());
    };
    let ts = yaml_str(&v, &["timestamp"]).unwrap_or_else(|| PLACEHOLDER.into());
    let last_report_at = format_timestamp_hhmm(&ts);
    let summary = yaml_str(&v, &["result", "summary"])
        .map(|s| first_line(&s))
        .unwrap_or_default();
    (last_report_at, summary)
}

fn format_timestamp_hhmm(ts: &str) -> String {
    if ts == PLACEHOLDER {
        return PLACEHOLDER.into();
    }
    // "2026-05-25T17:00:00" or "2026-05-25T17:00:00+09:00"
    if let Some(rest) = ts.split('T').nth(1) {
        let time_part = rest.split('+').next().unwrap_or(rest);
        let hhmm: String = time_part.chars().take(5).collect();
        if hhmm.len() >= 4 && hhmm.contains(':') {
            return hhmm;
        }
    }
    // fallback: last 5 chars if looks like time
    if ts.len() >= 5 {
        let tail: String = ts.chars().rev().take(8).collect::<String>().chars().rev().collect();
        if tail.contains(':') {
            return tail;
        }
    }
    PLACEHOLDER.into()
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn status_color(status: &str) -> gpui::Rgba {
    match status {
        "assigned" => Colors::kinpaku(),
        "done" => Colors::matsuba(),
        _ => Colors::muted(),
    }
}

fn status_indicator(status: &str) -> &'static str {
    match status {
        "assigned" => "🟡",
        "done" => "🟢",
        "idle" => "⚪",
        _ => "⚪",
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

fn truncate_summary(s: &str, max_lines: usize) -> String {
    let lines: Vec<_> = s.lines().take(max_lines).collect();
    let joined = lines.join("\n");
    if s.lines().count() > max_lines {
        format!("{joined}…")
    } else {
        joined
    }
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
        h_flex()
            .w_full()
            .items_start()
            .children(row.iter().map(render_agent_card))
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_yaml_extracts_fields() {
        let raw = r#"
task:
  task_id: cmd_177
  status: assigned
"#;
        let (id, st) = parse_task_yaml(&Some(raw.into()));
        assert_eq!(id, "cmd_177");
        assert_eq!(st, "assigned");
    }

    #[test]
    fn parse_inbox_unread_counts_false_read() {
        let raw = r#"
messages:
  - read: true
  - read: false
  - read: false
"#;
        assert_eq!(parse_inbox_unread(&Some(raw.into())), 2);
    }

    #[test]
    fn parse_report_yaml_summary_and_time() {
        let raw = r#"
timestamp: "2026-05-25T11:32:00"
result:
  summary: |
    第一行の要約
    第二行
"#;
        let (at, sum) = parse_report_yaml(&Some(raw.into()));
        assert_eq!(at, "11:32");
        assert_eq!(sum, "第一行の要約");
    }

    #[test]
    fn format_timestamp_missing_returns_placeholder() {
        assert_eq!(format_timestamp_hhmm("---"), "---");
    }
}
