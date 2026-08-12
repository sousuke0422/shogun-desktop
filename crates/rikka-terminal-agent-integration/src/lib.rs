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
/// The signal is the animated glyph a CLI puts at the head of its title while
/// working. Returns `None` for a static / idle title.
pub fn progress_from_title(title: &str) -> Option<AgentProgress> {
    title
        .chars()
        .any(is_spinner_glyph)
        .then_some(AgentProgress::Working)
}

/// A glyph that only appears in an animating spinner.
///
/// Two families, and the second is a lesson: for a long time this was the
/// Braille Patterns block alone (`U+2800..=U+28FF`), the home of cli-spinners'
/// `dots*`, `ora`, and Claude Code's own frames at the time. A Claude Code
/// update then moved its spinner to a two-frame half-circle alternation, and
/// because the check only knew Braille, agent progress **silently stopped
/// being detected at all** — no error, no test failure, just a feature that
/// quietly did nothing until someone noticed the taskbar had gone dark.
///
/// The half-circle pair is exactly what live sampling showed (0.3 s × 20 over
/// the running formation): `◑ → ◐ → ◑ → ◐`, alternating left/right, never a
/// four-phase rotation — so `U+25D2`/`U+25D3` are deliberately NOT here;
/// guessing at frames nobody has observed is how the previous entry aged into
/// a lie. Equally deliberate: Claude Code's IDLE mark `✳` (`U+2733`) must not
/// match, or a finished agent would report Working forever. The tests below
/// pin both halves against real captured titles.
fn is_spinner_glyph(c: char) -> bool {
    matches!(c, '\u{2800}'..='\u{28FF}' | '\u{25D0}' | '\u{25D1}')
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
        assert_eq!(
            progress_from_title("⠋ Thinking…"),
            Some(AgentProgress::Working)
        );
    }

    /// Captured live from the running formation on 2026-08-13 (0.3 s × 20):
    /// the busy pane alternated `◑ → ◐ → ◑ → ◐`, seven changes in six
    /// seconds. This is the positive control for the half-circle family — if
    /// it ever fails, detection has gone dark again.
    #[test]
    fn half_circle_spinner_title_reads_as_working() {
        assert_eq!(
            progress_from_title("◑ Claude Code"),
            Some(AgentProgress::Working)
        );
        assert_eq!(
            progress_from_title("◐ Claude Code"),
            Some(AgentProgress::Working)
        );
    }

    #[test]
    fn plain_title_reads_as_idle() {
        assert_eq!(progress_from_title("~/work/project"), None);
        assert_eq!(progress_from_title("nvim - main.rs"), None);
        assert_eq!(progress_from_title(""), None);
        // A title with an em dash / other punctuation is not a spinner.
        assert_eq!(progress_from_title("shogun:0:main — done"), None);
    }

    /// The other half of the same capture: idle panes sat on `✳ Claude Code`
    /// and finished work left `✳ <result>`. Matching `✳` would light the
    /// taskbar forever — the negative control that keeps the fix from
    /// becoming the older bug.
    #[test]
    fn claude_idle_mark_reads_as_idle() {
        assert_eq!(progress_from_title("✳ Claude Code"), None);
        assert_eq!(progress_from_title("✳ PR #388 マージ完了"), None);
        // Sibling half-circle frames were never observed in the animation;
        // they stay unmatched until something real shows them.
        assert_eq!(progress_from_title("◒ Claude Code"), None);
        assert_eq!(progress_from_title("◓ Claude Code"), None);
        // Other agents on this machine keep static titles.
        assert_eq!(progress_from_title("Cursor Agent"), None);
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

pub mod usage;
