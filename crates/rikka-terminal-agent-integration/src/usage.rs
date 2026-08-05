//! Subscription-usage report model — the `scripts/usage_status.sh` wire
//! format (key=value lines) and its parser.
//!
//! Same layering rule as the progress classifier: this knows how to READ the
//! report, not how to obtain it (SSH, `wsl.exe`, cron — the host's business)
//! nor how to draw it. shogun-desktop renders it today; rikka-terminal can
//! reuse it for a local gauge tomorrow.

/// Parsed `scripts/usage_status.sh` output (key=value lines). Missing keys
/// stay None — "unknown" must never render as 0%.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct UsageData {
    pub claude_ok: bool,
    pub claude_five_hour_pct: Option<f32>,
    pub claude_five_hour_resets: Option<String>,
    pub claude_seven_day_pct: Option<f32>,
    pub claude_seven_day_resets: Option<String>,
    pub codex_ok: bool,
    pub codex_plan: Option<String>,
    pub codex_age_minutes: Option<u32>,
    pub codex_primary_pct: Option<f32>,
    pub codex_primary_window: Option<u32>,
    pub codex_primary_resets: Option<String>,
    pub codex_secondary_pct: Option<f32>,
    pub codex_secondary_window: Option<u32>,
    pub codex_secondary_resets: Option<String>,
}

impl UsageData {
    pub fn parse(raw: &str) -> Self {
        let mut u = Self::default();
        for line in raw.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "claude.ok" => u.claude_ok = value == "true",
                "claude.five_hour_pct" => u.claude_five_hour_pct = value.parse().ok(),
                "claude.five_hour_resets" => u.claude_five_hour_resets = Some(value.into()),
                "claude.seven_day_pct" => u.claude_seven_day_pct = value.parse().ok(),
                "claude.seven_day_resets" => u.claude_seven_day_resets = Some(value.into()),
                "codex.ok" => u.codex_ok = value == "true",
                "codex.plan" => u.codex_plan = Some(value.into()),
                "codex.age_minutes" => u.codex_age_minutes = value.parse().ok(),
                "codex.primary_pct" => u.codex_primary_pct = value.parse().ok(),
                "codex.primary_window_minutes" => u.codex_primary_window = value.parse().ok(),
                "codex.primary_resets" => u.codex_primary_resets = Some(value.into()),
                "codex.secondary_pct" => u.codex_secondary_pct = value.parse().ok(),
                "codex.secondary_window_minutes" => u.codex_secondary_window = value.parse().ok(),
                "codex.secondary_resets" => u.codex_secondary_resets = Some(value.into()),
                _ => {}
            }
        }
        u
    }
}

/// "10080 minutes" reads as nothing; name the window like a human would.
pub fn window_label(minutes: u32) -> String {
    match minutes {
        10080 => "7日".into(),
        300 => "5時間".into(),
        m if m % 1440 == 0 => format!("{}日", m / 1440),
        m if m % 60 == 0 => format!("{}時間", m / 60),
        m => format!("{m}分"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_full_report() {
        let raw = "\
claude.ok=true
claude.five_hour_pct=69.0
claude.five_hour_resets=08/06 05:49
claude.seven_day_pct=61.0
claude.seven_day_resets=08/06 07:59
codex.ok=true
codex.plan=plus
codex.age_minutes=12
codex.primary_pct=66.0
codex.primary_window_minutes=10080
codex.primary_resets=08/08 15:24
";
        let u = UsageData::parse(raw);
        assert!(u.claude_ok && u.codex_ok);
        assert_eq!(u.claude_five_hour_pct, Some(69.0));
        assert_eq!(u.claude_seven_day_resets.as_deref(), Some("08/06 07:59"));
        assert_eq!(u.codex_plan.as_deref(), Some("plus"));
        assert_eq!(u.codex_age_minutes, Some(12));
        assert_eq!(u.codex_primary_window, Some(10080));
        // absent metrics stay None — never zero
        assert_eq!(u.codex_secondary_pct, None);
    }

    #[test]
    fn failure_and_garbage_do_not_invent_numbers() {
        let u = UsageData::parse("claude.ok=false\nclaude.error=URLError\nnot a kv line\n");
        assert!(!u.claude_ok && !u.codex_ok);
        assert_eq!(u.claude_five_hour_pct, None);
    }

    #[test]
    fn window_labels_read_like_a_human() {
        assert_eq!(window_label(10080), "7日");
        assert_eq!(window_label(300), "5時間");
        assert_eq!(window_label(2880), "2日");
        assert_eq!(window_label(90), "90分");
    }
}
