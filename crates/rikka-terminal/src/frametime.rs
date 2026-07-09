//! Frame-time measurement harness (アンチ硬直 groundwork).
//!
//! Set `SHOGUN_FRAMETIME=<path>` before launch and drive a workload
//! (`cat` a huge file, full-screen tmux redraws, a `yes` flood); every
//! [`REPORT_EVERY`] grid builds a stats line is appended to the file:
//!
//! ```text
//! [ft] frames=300 rows_avg=41 build_ms p50=0.3 p95=0.8 p99=1.4 max=3.1 | \
//!      paint_ms p50=1.9 p95=4.2 p99=7.0 max=12.8 | \
//!      gap_ms p50=16.6 p95=17.4 p99=33.9 max=181.0 stalls>50ms=2
//! ```
//!
//! Three signals per grid-build "frame":
//! - `build_ms` — [`render_grid`](crate::renderer::render_grid) element
//!   construction (the per-frame `coalesce_runs` row rebuild lives here).
//! - `paint_ms` — sum of the row-canvas paint closures that ran between two
//!   builds (shaping + quad emission; the box-drawing loops live here).
//! - `gap_ms` — interval between successive builds. Under a sustained flood
//!   this approximates the real frame cadence, so a multi-second hitch
//!   (blocking IO, lock convoy) shows up as a huge gap even though build and
//!   paint stay cheap. Gaps above [`IDLE_CUTOFF_MS`] are dropped as idle
//!   boundaries — the renderer is event-driven, so "nothing happened" gaps
//!   are normal and not hitches.
//!
//! Caveats (v1): all visible grids feed one global series — run workloads in
//! one window at a time for clean numbers. Off (one env lookup per call
//! site) unless the variable is set.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Report cadence, in grid builds (~5 s at 60 fps under sustained redraw).
const REPORT_EVERY: usize = 300;
/// Gaps longer than this are idle boundaries, not hitches; skip them.
const IDLE_CUTOFF_MS: f32 = 500.0;
/// A gap above this (and below the idle cutoff) counts as a stall.
const STALL_MS: f32 = 50.0;

fn out_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var("SHOGUN_FRAMETIME").ok())
        .as_deref()
}

/// Whether the harness is armed (`SHOGUN_FRAMETIME` set).
pub fn enabled() -> bool {
    out_path().is_some()
}

#[derive(Default)]
struct State {
    /// Entry instant of the previous grid build (gap measurement).
    last_build: Option<Instant>,
    /// Paint time accumulated since the last build (flushed by the next one).
    paint_accum_ms: f32,
    build_ms: Vec<f32>,
    paint_ms: Vec<f32>,
    gap_ms: Vec<f32>,
    stalls: u32,
    rows_sum: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Times one `render_grid` element build; its `Drop` is the frame mark that
/// also flushes the previous frame's paint accumulation and the build gap.
pub struct BuildGuard {
    t0: Instant,
    rows: usize,
}

/// Start timing a grid build (`None` when the harness is off).
pub fn build_guard(rows: usize) -> Option<BuildGuard> {
    enabled().then(|| BuildGuard {
        t0: Instant::now(),
        rows,
    })
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        let build_ms = self.t0.elapsed().as_secs_f32() * 1000.0;
        let Ok(mut s) = state().lock() else { return };
        let paint = std::mem::take(&mut s.paint_accum_ms);
        if paint > 0.0 {
            s.paint_ms.push(paint);
        }
        if let Some(last) = s.last_build
            && let Some(gap) = self.t0.checked_duration_since(last)
        {
            let gap_ms = gap.as_secs_f32() * 1000.0;
            if gap_ms <= IDLE_CUTOFF_MS {
                s.gap_ms.push(gap_ms);
                if gap_ms > STALL_MS {
                    s.stalls += 1;
                }
            }
        }
        s.last_build = Some(self.t0);
        s.build_ms.push(build_ms);
        s.rows_sum += self.rows as u64;
        if s.build_ms.len() >= REPORT_EVERY {
            report(&mut s);
        }
    }
}

/// Times one row-canvas paint closure; `Drop` adds to the frame accumulator.
pub struct PaintGuard {
    t0: Instant,
}

/// Start timing a paint closure (`None` when the harness is off).
pub fn paint_guard() -> Option<PaintGuard> {
    enabled().then(|| PaintGuard { t0: Instant::now() })
}

impl Drop for PaintGuard {
    fn drop(&mut self) {
        let ms = self.t0.elapsed().as_secs_f32() * 1000.0;
        if let Ok(mut s) = state().lock() {
            s.paint_accum_ms += ms;
        }
    }
}

/// `q`-quantile (0.0..=1.0) of an already sorted, non-empty slice.
fn pct(sorted: &[f32], q: f32) -> f32 {
    let idx = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// `p50=… p95=… p99=… max=…` for a sample series (sorts in place).
fn fmt_pcts(v: &mut [f32]) -> String {
    if v.is_empty() {
        return "n/a".to_string();
    }
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    format!(
        "p50={:.1} p95={:.1} p99={:.1} max={:.1}",
        pct(v, 0.50),
        pct(v, 0.95),
        pct(v, 0.99),
        pct(v, 1.0),
    )
}

fn report(s: &mut State) {
    let frames = s.build_ms.len();
    let rows_avg = s.rows_sum as f32 / frames.max(1) as f32;
    let line = format!(
        "[ft] frames={frames} rows_avg={rows_avg:.0} build_ms {} | paint_ms {} | gap_ms {} stalls>{STALL_MS:.0}ms={}\n",
        fmt_pcts(&mut s.build_ms),
        fmt_pcts(&mut s.paint_ms),
        fmt_pcts(&mut s.gap_ms),
        s.stalls,
    );
    if let Some(path) = out_path() {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
    s.build_ms.clear();
    s.paint_ms.clear();
    s.gap_ms.clear();
    s.stalls = 0;
    s.rows_sum = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_picks_expected_ranks() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(pct(&v, 0.0), 1.0);
        assert_eq!(pct(&v, 0.5), 6.0); // (n-1)*0.5 = 4.5 → rounds to index 5
        assert_eq!(pct(&v, 1.0), 10.0);
        assert_eq!(pct(&[42.0], 0.99), 42.0);
    }

    #[test]
    fn fmt_pcts_sorts_and_formats() {
        let mut v = vec![3.0, 1.0, 2.0];
        let s = fmt_pcts(&mut v);
        assert!(s.starts_with("p50=2.0"), "{s}");
        assert!(s.ends_with("max=3.0"), "{s}");
        assert_eq!(fmt_pcts(&mut []), "n/a");
    }
}
