//! MIT-licensed core logic for shogun agent status cards.
//! No GPUI/GPL dependencies — pure YAML parsing and card building.

pub mod rotate;

use serde_yml::Value;

pub const PLACEHOLDER: &str = "---";

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

/// Status classification derived from agent task status strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusCategory {
    Active,
    Done,
    Idle,
    Unknown,
}

pub fn status_category(status: &str) -> StatusCategory {
    match status {
        "assigned" | "work" | "active" => StatusCategory::Active,
        "done" => StatusCategory::Done,
        "idle" => StatusCategory::Idle,
        _ => StatusCategory::Unknown,
    }
}

pub fn status_indicator(status: &str) -> &'static str {
    match status {
        "assigned" | "work" | "active" => "🟡",
        "done" => "🟢",
        "idle" => "⚪",
        _ => "⚪",
    }
}

/// Build an agent card from raw YAML strings. Returns `None` when all inputs are absent.
pub fn build_agent_card(
    name: &str,
    task_yaml: Option<&str>,
    inbox_yaml: Option<&str>,
    report_yaml: Option<&str>,
) -> Option<AgentCardData> {
    if task_yaml.is_none() && inbox_yaml.is_none() && report_yaml.is_none() {
        return None;
    }

    let task_opt = task_yaml.map(String::from);
    let inbox_opt = inbox_yaml.map(String::from);
    let report_opt = report_yaml.map(String::from);

    let (task_id, status) = parse_task_yaml(&task_opt);
    let inbox_unread = parse_inbox_unread(&inbox_opt);
    let (last_report_at, summary) = parse_report_yaml(&report_opt);

    Some(AgentCardData {
        name: name.to_string(),
        task_id,
        status,
        inbox_unread,
        last_report_at,
        summary,
    })
}

/// Build the karo card from command queue and inbox YAML. Returns `None` when both inputs are absent.
pub fn build_karo_card(cmd_yaml: Option<&str>, inbox_yaml: Option<&str>) -> Option<AgentCardData> {
    if cmd_yaml.is_none() && inbox_yaml.is_none() {
        return None;
    }

    let cmd_opt = cmd_yaml.map(String::from);
    let inbox_opt = inbox_yaml.map(String::from);

    let (task_id, status) = parse_karo_cmd_yaml(&cmd_opt);
    let inbox_unread = parse_inbox_unread(&inbox_opt);

    Some(AgentCardData {
        name: "karo".to_string(),
        task_id,
        status,
        inbox_unread,
        last_report_at: String::new(),
        summary: String::new(),
    })
}

pub fn parse_karo_cmd_yaml(raw: &Option<String>) -> (String, String) {
    let Some(raw) = raw.as_ref() else {
        return (PLACEHOLDER.into(), PLACEHOLDER.into());
    };
    let Ok(val) = serde_yml::from_str::<Value>(raw) else {
        return (PLACEHOLDER.into(), PLACEHOLDER.into());
    };
    let first = val.as_sequence().and_then(|s| s.first());
    let task_id = first
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or(PLACEHOLDER)
        .to_string();
    (task_id, "active".to_string())
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

pub fn format_timestamp_hhmm(ts: &str) -> String {
    if ts == PLACEHOLDER {
        return PLACEHOLDER.into();
    }
    if let Some(rest) = ts.split('T').nth(1) {
        let time_part = rest.split('+').next().unwrap_or(rest);
        let hhmm: String = time_part.chars().take(5).collect();
        if hhmm.len() >= 4 && hhmm.contains(':') {
            return hhmm;
        }
    }
    if ts.len() >= 5 {
        let tail: String = ts
            .chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if tail.contains(':') {
            return tail;
        }
    }
    PLACEHOLDER.into()
}

pub fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

pub fn truncate_summary(s: &str, max_lines: usize) -> String {
    let lines: Vec<_> = s.lines().take(max_lines).collect();
    let joined = lines.join("\n");
    if s.lines().count() > max_lines {
        format!("{joined}…")
    } else {
        joined
    }
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

    #[test]
    fn status_category_classifies_known_statuses() {
        assert_eq!(status_category("assigned"), StatusCategory::Active);
        assert_eq!(status_category("work"), StatusCategory::Active);
        assert_eq!(status_category("active"), StatusCategory::Active);
        assert_eq!(status_category("done"), StatusCategory::Done);
        assert_eq!(status_category("idle"), StatusCategory::Idle);
        assert_eq!(status_category("failed"), StatusCategory::Unknown);
    }

    #[test]
    fn status_indicator_returns_emoji() {
        assert_eq!(status_indicator("assigned"), "🟡");
        assert_eq!(status_indicator("done"), "🟢");
        assert_eq!(status_indicator("idle"), "⚪");
        assert_eq!(status_indicator("unknown"), "⚪");
    }

    #[test]
    fn build_agent_card_returns_none_when_all_absent() {
        assert!(build_agent_card("ashigaru1", None, None, None).is_none());
    }

    #[test]
    fn build_agent_card_assembles_fields() {
        let task = r#"
task:
  task_id: subtask_001
  status: work
"#;
        let inbox = r#"
messages:
  - read: false
"#;
        let report = r#"
timestamp: "2026-06-03T13:20:00"
result:
  summary: "完了報告"
"#;
        let card = build_agent_card("ashigaru1", Some(task), Some(inbox), Some(report)).unwrap();
        assert_eq!(card.name, "ashigaru1");
        assert_eq!(card.task_id, "subtask_001");
        assert_eq!(card.status, "work");
        assert_eq!(card.inbox_unread, 1);
        assert_eq!(card.last_report_at, "13:20");
        assert_eq!(card.summary, "完了報告");
    }

    #[test]
    fn build_karo_card_returns_none_when_all_absent() {
        assert!(build_karo_card(None, None).is_none());
    }

    #[test]
    fn build_karo_card_parses_cmd_queue() {
        let cmd = r#"
- id: cmd_231
  status: assigned
"#;
        let inbox = r#"
messages:
  - read: false
  - read: true
"#;
        let card = build_karo_card(Some(cmd), Some(inbox)).unwrap();
        assert_eq!(card.name, "karo");
        assert_eq!(card.task_id, "cmd_231");
        assert_eq!(card.status, "active");
        assert_eq!(card.inbox_unread, 1);
    }

    #[test]
    fn parse_karo_cmd_yaml_extracts_first_cmd() {
        let raw = r#"
- id: cmd_100
- id: cmd_099
"#;
        let (id, status) = parse_karo_cmd_yaml(&Some(raw.into()));
        assert_eq!(id, "cmd_100");
        assert_eq!(status, "active");
    }

    #[test]
    fn truncate_summary_limits_lines() {
        let s = "line1\nline2\nline3";
        assert_eq!(truncate_summary(s, 2), "line1\nline2…");
        assert_eq!(truncate_summary("one line", 2), "one line");
    }

    #[test]
    fn first_line_skips_blank_lines() {
        assert_eq!(first_line("\n\nhello\nworld"), "hello");
    }
}
