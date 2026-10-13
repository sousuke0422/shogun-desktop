//! The judgement: for each frame, the integer offset `(dx, dy)` within ±R
//! that best maps the first frame's ROI onto this frame, and whether that
//! offset is a real drift or just changed pixels (a spinner glyph, a
//! blinking cursor) that happen to fit a little better elsewhere.
//!
//! Coordinates are window client pixels as captured: physical pixels, top
//! left (0,0). `dx > 0` means the content moved right, `dy > 0` down: the
//! base pixel at `p` is found at `p + (dx, dy)` in the frame.

use anyhow::{Result, bail};
use image::RgbaImage;
use std::fmt;

/// Search radius in pixels when none is given. A cell is ~8–10 px wide at
/// 100% scale, so ±8 covers a one-column drift and a few px of vertical
/// jitter without the search cost (17² offsets) getting silly.
pub const DEFAULT_RADIUS: u32 = 8;
/// Minimum drop in mean absolute difference (0–255 luma scale) from the
/// `(0,0)` offset to the best one. Below this the two are the same picture
/// as far as we can tell (GPU output is deterministic, so identical content
/// diffs to 0; anything under ~1 is a handful of anti-aliased pixels).
pub const DEFAULT_MIN_GAIN: f64 = 1.0;
/// Minimum fractional drop: the best offset must explain away at least this
/// share of the `(0,0)` difference. A real translation of the ROI explains
/// nearly all of it (the shifted content lines up exactly, best ≈ 0; what
/// is left is whatever else changed, e.g. a spinner that also turned). A
/// glyph swap is only partly explained by moving: braille spinner frames
/// are dot patterns that ARE translations of one another, and with an
/// unmasked spinner alone in an empty ROI the best offset removed at most
/// 0.667 of the difference (measured on ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ in the tests). 0.8 keeps
/// that out while a whole-ROI drift (≥ 0.9 in the tests) still counts.
/// An ROI where only part of the content moves explains less and may be
/// missed: put the ROI on the region that moves.
pub const DEFAULT_MIN_GAIN_RATIO: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// `x,y,w,h` (client pixels).
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        if parts.len() != 4 {
            bail!("rectangle must be x,y,w,h: {s:?}");
        }
        let x: i32 = parts[0].parse()?;
        let y: i32 = parts[1].parse()?;
        let w: u32 = parts[2].parse()?;
        let h: u32 = parts[3].parse()?;
        if w == 0 || h == 0 {
            bail!("rectangle has no area: {s:?}");
        }
        Ok(Self { x, y, w, h })
    }

    pub fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.w)
    }

    pub fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.h)
    }

    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= i64::from(self.x) && x < self.right() && y >= i64::from(self.y) && y < self.bottom()
    }

    /// Whether the rectangle lies entirely inside a `w`×`h` image.
    pub fn fits(&self, w: u32, h: u32) -> bool {
        self.x >= 0 && self.y >= 0 && self.right() <= i64::from(w) && self.bottom() <= i64::from(h)
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{},{}", self.x, self.y, self.w, self.h)
    }
}

#[derive(Clone, Debug)]
pub struct Params {
    pub roi: Rect,
    /// Areas inside the ROI whose pixels may change every frame (spinner
    /// cell, cursor). Excluded by their position in the BASE frame.
    pub masks: Vec<Rect>,
    pub radius: u32,
    pub min_gain: f64,
    pub min_gain_ratio: f64,
}

impl Params {
    pub fn new(roi: Rect) -> Self {
        Self {
            roi,
            masks: Vec::new(),
            radius: DEFAULT_RADIUS,
            min_gain: DEFAULT_MIN_GAIN,
            min_gain_ratio: DEFAULT_MIN_GAIN_RATIO,
        }
    }
}

/// One frame's result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    pub dx: i32,
    pub dy: i32,
    /// Mean absolute luma difference at offset (0,0).
    pub diff0: f64,
    /// Mean absolute luma difference at the best offset.
    pub best: f64,
    /// Judged a real drift (see [`DEFAULT_MIN_GAIN`] / [`DEFAULT_MIN_GAIN_RATIO`]).
    pub shifted: bool,
    /// The best offset sits on the ±R boundary: the cost was still falling
    /// there, so the content may have moved further than the search covers.
    /// Independent of `shifted` — a drift past R is only partly explained
    /// at R and can miss the thresholds; rerun with a larger radius.
    pub at_edge: bool,
    /// Something outside the masks changed (`diff0 ≥ min_gain`) but no
    /// offset within ±R explains it. Either a change that is not a drift,
    /// or a drift larger than R over sharp content (text), where the cost
    /// does not fall toward the boundary and `at_edge` cannot fire — e.g. a
    /// one-row drift at 150 % is 24 px. Rerun with a larger radius.
    pub unexplained: bool,
}

/// Luma plane of a frame (Rec. 601 weights). Comparing one channel instead
/// of three keeps the 17×17 search cheap; colored text still differs in luma.
struct Luma {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Luma {
    fn from_rgba(img: &RgbaImage) -> Self {
        let px = img
            .pixels()
            .map(|p| {
                let [r, g, b, _] = p.0;
                ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000) as u8
            })
            .collect();
        Self {
            w: img.width(),
            h: img.height(),
            px,
        }
    }

    fn at(&self, x: i64, y: i64) -> Option<u8> {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) {
            return None;
        }
        Some(self.px[(y as usize) * (self.w as usize) + x as usize])
    }
}

pub struct Analyzer {
    params: Params,
    base: Luma,
    /// Unmasked ROI pixels: (x, y, base luma).
    samples: Vec<(i64, i64, u8)>,
}

impl Analyzer {
    /// `base` is the reference frame (normally the first one). Refuses an
    /// ROI that does not lie inside it rather than silently clipping, since
    /// a clipped ROI measures a different region than the one asked for.
    pub fn new(params: Params, base: &RgbaImage) -> Result<Self> {
        let roi = params.roi;
        if !roi.fits(base.width(), base.height()) {
            bail!(
                "ROI {roi} lies outside the window (frame is {}x{}); \
                 ROI is in client pixels x,y,w,h",
                base.width(),
                base.height()
            );
        }
        let base = Luma::from_rgba(base);
        let mut samples = Vec::with_capacity((roi.w * roi.h) as usize);
        for y in i64::from(roi.y)..roi.bottom() {
            for x in i64::from(roi.x)..roi.right() {
                if params.masks.iter().any(|m| m.contains(x, y)) {
                    continue;
                }
                samples.push((x, y, base.at(x, y).expect("inside ROI")));
            }
        }
        if samples.is_empty() {
            bail!("the masks cover the whole ROI {roi}");
        }
        Ok(Self {
            params,
            base,
            samples,
        })
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Mean absolute difference between the base samples and `frame` read at
    /// `p + (dx, dy)`. Samples that land outside the frame are skipped; an
    /// offset that loses more than half of them is not a candidate.
    fn cost(&self, frame: &Luma, dx: i32, dy: i32) -> Option<f64> {
        let (dx, dy) = (i64::from(dx), i64::from(dy));
        let mut sum: u64 = 0;
        let mut n: u64 = 0;
        for &(x, y, b) in &self.samples {
            if let Some(f) = frame.at(x + dx, y + dy) {
                sum += u64::from(b.abs_diff(f));
                n += 1;
            }
        }
        (n * 2 >= self.samples.len() as u64).then(|| sum as f64 / n as f64)
    }

    pub fn judge(&self, frame: &RgbaImage) -> Result<Verdict> {
        if frame.width() != self.base.w || frame.height() != self.base.h {
            bail!(
                "frame is {}x{} but the first frame was {}x{} (window resized?)",
                frame.width(),
                frame.height(),
                self.base.w,
                self.base.h
            );
        }
        let frame = Luma::from_rgba(frame);
        let diff0 = self.cost(&frame, 0, 0).expect("(0,0) keeps every sample");
        if diff0 == 0.0 {
            return Ok(Verdict {
                dx: 0,
                dy: 0,
                diff0,
                best: 0.0,
                shifted: false,
                at_edge: false,
                unexplained: false,
            });
        }
        let r = self.params.radius as i32;
        let (mut bdx, mut bdy, mut best) = (0i32, 0i32, diff0);
        for dy in -r..=r {
            for dx in -r..=r {
                let Some(c) = self.cost(&frame, dx, dy) else {
                    continue;
                };
                // Ties go to the smaller offset, so (0,0) wins a draw.
                let closer = dx.abs() + dy.abs() < bdx.abs() + bdy.abs();
                if c < best || (c == best && closer) {
                    (bdx, bdy, best) = (dx, dy, c);
                }
            }
        }
        let moved = (bdx, bdy) != (0, 0);
        let gain = diff0 - best;
        let shifted =
            moved && gain >= self.params.min_gain && gain >= diff0 * self.params.min_gain_ratio;
        let at_edge = moved && (bdx.abs() == r || bdy.abs() == r);
        let unexplained = !shifted && diff0 >= self.params.min_gain;
        Ok(Verdict {
            dx: bdx,
            dy: bdy,
            diff0,
            best,
            shifted,
            at_edge,
            unexplained,
        })
    }
}
