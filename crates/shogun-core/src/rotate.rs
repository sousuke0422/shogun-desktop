//! YAML rotation helpers — done/active split without line-count cutting.
//! Mirrors semantics of multi-agent-shogun `scripts/slim_yaml.py`.

use serde_yml::Value;
use std::path::Path;

pub const TERMINAL_STATUSES: &[&str] = &["done", "cancelled", "paused"];
pub const ACTIVE_STATUSES: &[&str] = &["pending", "in_progress", "blocked"];
pub const TASK_ACTIVE_STATUSES: &[&str] = &["idle", "assigned", "pending_blocked"];
pub const PROTECTED_STATUSES: &[&str] = &[
    "pending",
    "in_progress",
    "blocked",
    "assigned",
    "work",
    "active",
];

pub const ASHIGARU_IDS: &[&str] = &[
    "ashigaru1",
    "ashigaru2",
    "ashigaru3",
    "ashigaru4",
    "ashigaru5",
    "ashigaru6",
    "ashigaru7",
];

/// Status from top-level `status` or nested `task.status`.
pub fn item_status(item: &Value) -> Option<String> {
    let dict = item.as_mapping()?;
    if let Some(s) = dict.get(Value::from("status")).and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    dict.get(Value::from("task"))
        .and_then(|t| t.get("status"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn is_terminal_status(status: &str) -> bool {
    TERMINAL_STATUSES.contains(&status)
}

pub fn is_protected_status(status: &str) -> bool {
    PROTECTED_STATUSES.contains(&status)
}

/// Ashigaru is safe to rotate when status is `idle` or `done` only.
pub fn ashigaru_safe_for_rotation(status: &str) -> bool {
    status == "idle" || status == "done"
}

/// Parsed command queue with optional wrapper key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCmdQueue {
    pub wrapper_key: Option<String>,
    pub active: Vec<Value>,
    pub archived: Vec<Value>,
}

/// Split shogun_to_karo YAML into active vs terminal (done/cancelled/paused) entries.
pub fn split_cmd_queue_yaml(raw: &str) -> Result<SplitCmdQueue, String> {
    let val: Value = serde_yml::from_str(raw).map_err(|e| format!("yaml parse error: {e}"))?;

    let (wrapper_key, mut queue) = match val {
        Value::Sequence(seq) => (None, seq),
        Value::Mapping(mut map) => {
            let key = if map.contains_key("commands") {
                "commands".to_string()
            } else if map.contains_key("queue") {
                "queue".to_string()
            } else {
                return Ok(SplitCmdQueue {
                    wrapper_key: None,
                    active: vec![],
                    archived: vec![],
                });
            };
            let q = map
                .remove(key.as_str())
                .and_then(|v| v.as_sequence().cloned())
                .unwrap_or_default();
            (Some(key), q)
        }
        _ => {
            return Ok(SplitCmdQueue {
                wrapper_key: None,
                active: vec![],
                archived: vec![],
            });
        }
    };

    let mut active = Vec::new();
    let mut archived = Vec::new();
    for item in queue.drain(..) {
        let status = item_status(&item).unwrap_or_default();
        if is_terminal_status(&status) {
            archived.push(item);
        } else {
            active.push(item);
        }
    }

    Ok(SplitCmdQueue {
        wrapper_key,
        active,
        archived,
    })
}

/// Rebuild YAML string preserving list vs wrapped dict shape.
pub fn rebuild_cmd_queue(split: &SplitCmdQueue) -> Result<String, String> {
    let active_seq = Value::Sequence(split.active.clone());
    let out = match &split.wrapper_key {
        None => active_seq,
        Some(key) => {
            let mut map = serde_yml::Mapping::new();
            map.insert(Value::String(key.clone()), active_seq);
            Value::Mapping(map)
        }
    };
    serde_yml::to_string(&out).map_err(|e| format!("yaml serialize error: {e}"))
}

/// Serialize archived entries for zstd blob (list or wrapped).
pub fn serialize_archived_batch(split: &SplitCmdQueue) -> Result<String, String> {
    let archived_seq = Value::Sequence(split.archived.clone());
    let out = match &split.wrapper_key {
        None => archived_seq,
        Some(key) => {
            let mut map = serde_yml::Mapping::new();
            map.insert(Value::String(key.clone()), archived_seq);
            Value::Mapping(map)
        }
    };
    serde_yml::to_string(&out).map_err(|e| format!("yaml serialize error: {e}"))
}

/// Count terminal entries in shogun_to_karo without modifying.
pub fn count_done_commands(raw: &str) -> Result<usize, String> {
    Ok(split_cmd_queue_yaml(raw)?.archived.len())
}

/// True when every ashigaru task file is idle or done.
pub fn all_ashigaru_idle_or_done(tasks_dir: &Path) -> Result<bool, String> {
    for id in ASHIGARU_IDS {
        let path = tasks_dir.join(format!("{id}.yaml"));
        if !path.exists() {
            continue;
        }
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let val: Value =
            serde_yml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        let status = item_status(&val).unwrap_or_default();
        if !ashigaru_safe_for_rotation(&status) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Extract cmd id from a command entry Value.
pub fn cmd_id(item: &Value) -> Option<String> {
    item.as_mapping()
        .and_then(|m| m.get(Value::from("id")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_preserves_active_pending() {
        let raw = r#"
- id: cmd_001
  status: done
- id: cmd_002
  status: in_progress
- id: cmd_003
  status: done
"#;
        let split = split_cmd_queue_yaml(raw).unwrap();
        assert_eq!(split.archived.len(), 2);
        assert_eq!(split.active.len(), 1);
        let active_status = item_status(&split.active[0]).unwrap();
        assert_eq!(active_status, "in_progress");
    }

    #[test]
    fn protected_in_progress_never_archived() {
        let raw = r#"
- id: cmd_100
  status: in_progress
"#;
        let split = split_cmd_queue_yaml(raw).unwrap();
        assert!(split.archived.is_empty());
        assert_eq!(split.active.len(), 1);
    }

    #[test]
    fn roundtrip_rebuild_matches_active_only() {
        let raw = r#"
- id: cmd_a
  status: done
  purpose: old
- id: cmd_b
  status: assigned
  purpose: keep
"#;
        let split = split_cmd_queue_yaml(raw).unwrap();
        let rebuilt = rebuild_cmd_queue(&split).unwrap();
        let again = split_cmd_queue_yaml(&rebuilt).unwrap();
        assert_eq!(again.active.len(), 1);
        assert_eq!(cmd_id(&again.active[0]).as_deref(), Some("cmd_b"));
        assert!(again.archived.is_empty());
    }

    #[test]
    fn nested_task_status_respected() {
        let raw = r#"
task:
  task_id: subtask_1
  status: assigned
"#;
        let val: Value = serde_yml::from_str(raw).unwrap();
        assert_eq!(item_status(&val).as_deref(), Some("assigned"));
        assert!(!ashigaru_safe_for_rotation("assigned"));
    }

    #[test]
    fn all_ashigaru_idle_or_done_detects_work() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ashigaru1.yaml"), "task:\n  status: work\n").unwrap();
        std::fs::write(dir.path().join("ashigaru2.yaml"), "task:\n  status: idle\n").unwrap();
        assert!(!all_ashigaru_idle_or_done(dir.path()).unwrap());
    }
}
