//! Running the judgement over a saved sequence and writing the results:
//! `shift.csv` (one row per frame), `summary.txt`, and for every frame
//! judged shifted or marked (search edge, unexplained change) a side-by-side (`shift_NNNNN_side.png`: first frame's ROI |
//! this frame's ROI) and a difference image (`shift_NNNNN_diff.png`).

use crate::analyze::{Analyzer, Params, Verdict};
use crate::seq::{self, SeqFrame};
use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use std::fmt::{self, Write as _};
use std::path::Path;

pub const CSV_FILE: &str = "shift.csv";
pub const SUMMARY_FILE: &str = "summary.txt";
pub const CSV_HEADER: &str = "index,t_ms,dx,dy,diff0,best_diff,shifted,at_edge,unexplained";

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub index: usize,
    pub t_ms: Option<f64>,
    pub verdict: Verdict,
}

impl Row {
    pub fn csv(&self) -> String {
        let t = self.t_ms.map(|t| format!("{t:.3}")).unwrap_or_default();
        let v = &self.verdict;
        format!(
            "{},{t},{},{},{:.4},{:.4},{},{},{}",
            self.index,
            v.dx,
            v.dy,
            v.diff0,
            v.best,
            u8::from(v.shifted),
            u8::from(v.at_edge),
            u8::from(v.unexplained)
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub frames: usize,
    pub shifted: usize,
    /// Frames (shifted or not) whose best offset sat on the ±R boundary.
    pub at_edge: usize,
    /// Frames that changed outside the masks without any offset explaining it.
    pub unexplained: usize,
    /// Largest |dx| / |dy| among shifted frames (signed value kept).
    pub max_dx: i32,
    pub max_dy: i32,
    pub first_shift_index: Option<usize>,
    pub first_shift_t_ms: Option<f64>,
}

impl Summary {
    pub fn from_rows(rows: &[Row]) -> Self {
        let mut s = Summary {
            frames: rows.len(),
            ..Default::default()
        };
        s.at_edge = rows.iter().filter(|r| r.verdict.at_edge).count();
        s.unexplained = rows.iter().filter(|r| r.verdict.unexplained).count();
        for r in rows.iter().filter(|r| r.verdict.shifted) {
            s.shifted += 1;
            if r.verdict.dx.abs() > s.max_dx.abs() {
                s.max_dx = r.verdict.dx;
            }
            if r.verdict.dy.abs() > s.max_dy.abs() {
                s.max_dy = r.verdict.dy;
            }
            if s.first_shift_index.is_none() {
                s.first_shift_index = Some(r.index);
                s.first_shift_t_ms = r.t_ms;
            }
        }
        s
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "frames: {}", self.frames)?;
        writeln!(f, "shifted_frames: {}", self.shifted)?;
        writeln!(f, "at_search_edge: {}", self.at_edge)?;
        writeln!(f, "unexplained_change: {}", self.unexplained)?;
        writeln!(f, "max_dx: {}", self.max_dx)?;
        writeln!(f, "max_dy: {}", self.max_dy)?;
        match self.first_shift_index {
            Some(i) => {
                let t = self
                    .first_shift_t_ms
                    .map(|t| format!("{t:.3}"))
                    .unwrap_or_else(|| "-".into());
                writeln!(f, "first_shift: frame {i} at {t} ms")
            }
            None => writeln!(f, "first_shift: none"),
        }
    }
}

/// Judge every frame of `frames` from `base_pos` on against the frame at
/// `base_pos`; `on_shift` sees each frame that is shifted or marked. Frames before it are skipped (e.g. captured before the
/// animation was on screen).
pub fn judge_all(
    frames: &[SeqFrame],
    params: Params,
    base_pos: usize,
    mut on_shift: impl FnMut(&Row, &RgbaImage, &RgbaImage, &Params) -> Result<()>,
) -> Result<Vec<Row>> {
    let frames = frames
        .get(base_pos..)
        .filter(|f| !f.is_empty())
        .with_context(|| format!("base index {base_pos} is past the last frame"))?;
    let base_img = seq::load(&frames[0])?;
    let analyzer = Analyzer::new(params, &base_img)?;
    let mut rows = Vec::with_capacity(frames.len());
    for frame in frames {
        let img = seq::load(frame)?;
        let verdict = analyzer
            .judge(&img)
            .with_context(|| format!("frame {}", frame.index))?;
        let row = Row {
            index: frame.index,
            t_ms: frame.t_ms,
            verdict,
        };
        if verdict.shifted || verdict.at_edge || verdict.unexplained {
            on_shift(&row, &base_img, &img, analyzer.params())?;
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The whole `analyze` step: read `input`, write CSV / summary / PNGs to `out`.
pub fn analyze_dir(input: &Path, params: Params, base_pos: usize, out: &Path) -> Result<Summary> {
    std::fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    let frames = seq::list(input)?;
    let rows = judge_all(&frames, params, base_pos, |row, base, img, p| {
        side_by_side(base, img, p)
            .save(out.join(format!("shift_{:05}_side.png", row.index)))
            .context("write side-by-side PNG")?;
        diff_image(base, img, p)
            .save(out.join(format!("shift_{:05}_diff.png", row.index)))
            .context("write diff PNG")?;
        Ok(())
    })?;
    let mut csv = String::from(CSV_HEADER);
    csv.push('\n');
    for r in &rows {
        let _ = writeln!(csv, "{}", r.csv());
    }
    std::fs::write(out.join(CSV_FILE), csv).context("write shift.csv")?;
    let summary = Summary::from_rows(&rows);
    std::fs::write(out.join(SUMMARY_FILE), summary.to_string()).context("write summary.txt")?;
    Ok(summary)
}

const SEP: u32 = 4;
const SEP_COLOR: Rgba<u8> = Rgba([255, 0, 255, 255]);

fn crop(img: &RgbaImage, p: &Params) -> RgbaImage {
    let r = p.roi;
    image::imageops::crop_imm(img, r.x as u32, r.y as u32, r.w, r.h).to_image()
}

/// First frame's ROI, a magenta bar, this frame's ROI (both unshifted).
pub fn side_by_side(base: &RgbaImage, frame: &RgbaImage, p: &Params) -> RgbaImage {
    let (a, b) = (crop(base, p), crop(frame, p));
    let mut out = RgbaImage::from_pixel(a.width() * 2 + SEP, a.height(), SEP_COLOR);
    image::imageops::replace(&mut out, &a, 0, 0);
    image::imageops::replace(&mut out, &b, i64::from(a.width() + SEP), 0);
    out
}

/// |base − frame| at offset (0,0) inside the ROI, ×4 and shown in red;
/// masked pixels are tinted blue so it is clear they were not compared.
pub fn diff_image(base: &RgbaImage, frame: &RgbaImage, p: &Params) -> RgbaImage {
    let r = p.roi;
    RgbaImage::from_fn(r.w, r.h, |x, y| {
        let (gx, gy) = (r.x as u32 + x, r.y as u32 + y);
        if p.masks
            .iter()
            .any(|m| m.contains(i64::from(gx), i64::from(gy)))
        {
            return Rgba([0, 0, 96, 255]);
        }
        let (a, b) = (base.get_pixel(gx, gy).0, frame.get_pixel(gx, gy).0);
        let d = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
        Rgba([d.saturating_mul(4), 0, 0, 255])
    })
}
