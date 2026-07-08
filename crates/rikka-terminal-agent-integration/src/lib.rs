//! Infer a coding agent's progress from a terminal's out-of-band signals.
//!
//! Some agents can't (or won't) speak the OSC 9;4 progress protocol on every
//! surface. Claude Code, for instance, animates a Braille spinner in the
//! window title while it works and drops OSC 9;4 entirely inside tmux (it
//! treats a multiplexer as progress-incapable). This crate maps such in-band
//! signals to a normalized progress state, so a host terminal can still draw a
//! real progress bar for agents that only hint at their activity.
//!
//! Layering: deliberately dependency-free and host-agnostic. It knows nothing
//! about the terminal engine ([`rikka-terminal`]), GPUI, or how a bar is
//! painted — it only classifies signals. New agents (Codex, …) plug in here as
//! additional matchers so the engine and the host both stay agnostic.

use std::time::{Duration, Instant};

/// Normalized, host-agnostic agent progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentProgress {
    /// The agent is working with no measurable fraction — draw an
    /// indeterminate / pulsing bar.
    Working,
}

/// Classify an OSC 0/2 window-title string as agent activity.
///
/// Claude Code animates a Braille spinner (`⠋⠙⠹…`) at the head of the title
/// while working. Every mainstream CLI spinner set (cli-spinners' `dots*`,
/// `ora`, and Claude's own) draws its frames from the Braille Patterns block,
/// so a single Braille glyph in the title is a strong "an agent is busy"
/// signal. Returns `None` for a static / idle title.
pub fn progress_from_title(title: &str) -> Option<AgentProgress> {
    title
        .chars()
        .any(is_spinner_glyph)
        .then_some(AgentProgress::Working)
}

/// Braille Patterns block (`U+2800..=U+28FF`) — the home of essentially every
/// terminal spinner frame.
fn is_spinner_glyph(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
}

/// Tracks the last moment an agent looked busy so a host can hold the bar for a
/// short grace period after the final spinner frame.
///
/// Why a grace period: a title spinner has no explicit "done" frame — it simply
/// stops updating when work ends. The host therefore clears the bar once no
/// fresh busy signal has arrived within `grace`. Feed it the current title on
/// every refresh (e.g. each PTY wake); a spinner-free title is not an error, it
/// is just not a fresh signal.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    active_at: Option<Instant>,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record "busy now" if `title` carries a spinner. A spinner-free (or
    /// absent) title leaves the last busy time untouched — the `grace` window
    /// in [`current`](Self::current) is what ends the activity.
    pub fn observe_title(&mut self, title: Option<&str>) {
        if title.and_then(progress_from_title).is_some() {
            self.active_at = Some(Instant::now());
        }
    }

    /// `Some(Working)` only while within `grace` of the last busy signal.
    pub fn current(&self, grace: Duration) -> Option<AgentProgress> {
        match self.active_at {
            Some(at) if at.elapsed() < grace => Some(AgentProgress::Working),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_spinner_title_reads_as_working() {
        // Claude Code's real in-tmux title (spinner + task).
        assert_eq!(
            progress_from_title("⠂ リセット処理の問題を調査"),
            Some(AgentProgress::Working)
        );
        assert_eq!(progress_from_title("⠋ Thinking…"), Some(AgentProgress::Working));
    }

    #[test]
    fn plain_title_reads_as_idle() {
        assert_eq!(progress_from_title("~/work/project"), None);
        assert_eq!(progress_from_title("nvim - main.rs"), None);
        assert_eq!(progress_from_title(""), None);
        // A title with an em dash / other punctuation is not a spinner.
        assert_eq!(progress_from_title("shogun:0:main — done"), None);
    }

    #[test]
    fn tracker_holds_within_grace_and_expires_after() {
        let mut t = ActivityTracker::new();
        assert_eq!(t.current(Duration::from_secs(1)), None);

        t.observe_title(Some("⠹ working"));
        assert_eq!(
            t.current(Duration::from_secs(1)),
            Some(AgentProgress::Working)
        );

        // A spinner-free title is not a fresh signal; with a zero grace the
        // last busy time is already too old, so the bar clears.
        t.observe_title(Some("idle title"));
        assert_eq!(t.current(Duration::ZERO), None);
    }
}
