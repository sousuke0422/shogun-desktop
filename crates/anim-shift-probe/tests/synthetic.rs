//! The judgement on synthetic PNG sequences: known drifts, spinner-only
//! changes (with and without a mask), drifts past the search radius, and an
//! ROI outside the window. Frames go through the on-disk sequence format so
//! the loader `analyze` uses on real captures is exercised too.

use anim_shift_probe::analyze::{Analyzer, Params, Rect};
use anim_shift_probe::report::{self, CSV_HEADER};
use anim_shift_probe::seq;
use image::{Rgba, RgbaImage};
use std::path::PathBuf;

const W: u32 = 240;
const H: u32 = 96;
const BG: u8 = 30;

fn roi() -> Rect {
    Rect::new(40, 24, 160, 48)
}

/// Spinner cell (absolute, unshifted position).
fn spinner_cell() -> Rect {
    Rect::new(44, 52, 9, 16)
}

/// Braille spinner ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ as dot bits (the low byte of U+28xx:
/// bit n = dot n+1).
const SPINNER: [u8; 10] = [0x0B, 0x19, 0x39, 0x38, 0x3C, 0x34, 0x26, 0x27, 0x07, 0x0F];

/// Terminal-ish content: flat background, rows of hard-edged "glyphs".
fn glyph_luma(x: i64, y: i64) -> u8 {
    let rows = [(30, 14), (48, 14)]; // (top, height) of two text rows
    for (top, h) in rows {
        if y >= top && y < top + h && (44..196).contains(&x) {
            let (col, gx, gy) = ((x - 44) / 9, (x - 44) % 9, y - top);
            if gx == 8 {
                return BG; // inter-glyph gap
            }
            // Deterministic per-glyph stroke pattern.
            let seed = (col * 2654435761 + top * 40503) as u64;
            let stroke = ((seed >> (gy as u64 % 13)) ^ (seed >> (gx as u64 + 3))) & 1 == 1;
            if stroke {
                return 220;
            }
        }
    }
    BG
}

fn draw_spinner(img: &mut RgbaImage, cell: Rect, phase: usize, dx: i32, dy: i32) {
    let bits = SPINNER[phase % SPINNER.len()];
    // Dots 1-3 and 7 in the left column, 4-6 and 8 in the right.
    let pos = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for (n, (c, r)) in pos.iter().enumerate() {
        if bits >> n & 1 == 0 {
            continue;
        }
        let (x0, y0) = (cell.x + dx + 1 + c * 4, cell.y + dy + 1 + r * 4);
        for y in y0..y0 + 3 {
            for x in x0..x0 + 3 {
                img.put_pixel(x as u32, y as u32, Rgba([200, 200, 255, 255]));
            }
        }
    }
}

/// Glyph rows and spinner, the whole picture translated by (dx, dy).
fn text_frame(dx: i32, dy: i32, phase: usize) -> RgbaImage {
    let mut img = RgbaImage::from_fn(W, H, |x, y| {
        let v = glyph_luma(i64::from(x as i32 - dx), i64::from(y as i32 - dy));
        Rgba([v, v, v, 255])
    });
    // Clear the spinner cell's background, then draw the glyph.
    let c = spinner_cell();
    for y in 0..c.h as i32 {
        for x in 0..c.w as i32 {
            img.put_pixel(
                (c.x + dx + x) as u32,
                (c.y + dy + y) as u32,
                Rgba([BG, BG, BG, 255]),
            );
        }
    }
    draw_spinner(&mut img, c, phase, dx, dy);
    img
}

/// Smooth content (no repeats within ±20 px), so the cost falls steadily
/// toward the true offset even when it lies beyond the search radius.
fn smooth_frame(dx: i32, dy: i32) -> RgbaImage {
    RgbaImage::from_fn(W, H, |x, y| {
        let (x, y) = (f64::from(x as i32 - dx), f64::from(y as i32 - dy));
        let v =
            128.0 + 50.0 * (x / 6.0).sin() + 40.0 * (y / 5.0).cos() + 20.0 * ((x + y) / 9.0).sin();
        let v = v.clamp(0.0, 255.0) as u8;
        Rgba([v, v, v, 255])
    })
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "anim-shift-probe-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Save frames as a sequence (16.7 ms apart) and judge them via the loader.
fn judge_seq(tag: &str, frames: &[RgbaImage], params: Params) -> Vec<report::Row> {
    let dir = TempDir::new(tag);
    let mut times = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        seq::save_frame(&dir.0, i, f).unwrap();
        times.push((i, i as f64 * 16.7));
    }
    seq::write_index(&dir.0, &times).unwrap();
    let listed = seq::list(&dir.0).unwrap();
    assert_eq!(listed.len(), frames.len());
    report::judge_all(&listed, params, 0, |_, _, _, _| Ok(())).unwrap()
}

#[test]
fn known_shifts_are_found() {
    let frames = vec![
        text_frame(0, 0, 0),
        text_frame(1, 0, 0),
        text_frame(0, 2, 0),
        text_frame(-3, 1, 0),
    ];
    let rows = judge_seq("known", &frames, Params::new(roi()));
    let got: Vec<_> = rows
        .iter()
        .map(|r| (r.verdict.dx, r.verdict.dy, r.verdict.shifted))
        .collect();
    assert_eq!(
        got,
        vec![(0, 0, false), (1, 0, true), (0, 2, true), (-3, 1, true)]
    );
    assert!(rows.iter().all(|r| !r.verdict.at_edge));
    assert!(
        rows[1].verdict.best < 1e-9,
        "a pure translation lines up exactly"
    );
}

#[test]
fn known_shifts_are_found_while_the_spinner_turns() {
    let frames = vec![
        text_frame(0, 0, 0),
        text_frame(1, 0, 3),
        text_frame(0, 2, 6),
        text_frame(-3, 1, 9),
    ];
    let mut params = Params::new(roi());
    params.masks.push(spinner_cell());
    let rows = judge_seq("known-spin", &frames, params);
    let got: Vec<_> = rows
        .iter()
        .map(|r| (r.verdict.dx, r.verdict.dy, r.verdict.shifted))
        .collect();
    assert_eq!(
        got,
        vec![(0, 0, false), (1, 0, true), (0, 2, true), (-3, 1, true)]
    );
}

fn spinner_only_frames() -> Vec<RgbaImage> {
    (0..20).map(|phase| text_frame(0, 0, phase)).collect()
}

#[test]
fn spinner_glyph_changes_are_not_shifts_with_mask() {
    let mut params = Params::new(roi());
    params.masks.push(spinner_cell());
    let rows = judge_seq("spin-mask", &spinner_only_frames(), params);
    for r in &rows {
        assert!(
            !r.verdict.shifted,
            "frame {} judged shifted: {:?}",
            r.index, r.verdict
        );
        assert_eq!(
            r.verdict.diff0, 0.0,
            "masked spinner leaves nothing to compare"
        );
    }
}

#[test]
fn spinner_glyph_changes_are_not_shifts_without_mask() {
    let rows = judge_seq("spin-nomask", &spinner_only_frames(), Params::new(roi()));
    // The spinner really does change pixels inside the ROI…
    assert!(rows.iter().skip(1).any(|r| r.verdict.diff0 > 0.0));
    // …but no frame may be called a drift.
    for r in &rows {
        assert!(
            !r.verdict.shifted,
            "frame {} judged shifted: {:?}",
            r.index, r.verdict
        );
    }
}

/// Flat background and one unmasked spinner: the worst case for the
/// judgement, because successive braille glyphs are translations of one
/// another and a moved ROI really does fit better.
fn lone_spinner(phase: usize) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(W, H, Rgba([BG, BG, BG, 255]));
    draw_spinner(&mut img, spinner_cell(), phase, 0, 0);
    img
}

fn lone_spinner_roi() -> Rect {
    Rect::new(36, 44, 28, 32)
}

#[test]
fn lone_spinner_is_not_a_shift_with_default_thresholds() {
    let frames: Vec<_> = (0..20).map(lone_spinner).collect();
    let rows = judge_seq("lone", &frames, Params::new(lone_spinner_roi()));
    for r in &rows {
        assert!(
            !r.verdict.shifted,
            "frame {} judged shifted: {:?}",
            r.index, r.verdict
        );
    }
    // Control: the thresholds are what keeps it out. Without them the same
    // frames DO come back as drifts, so the test above can fail.
    let mut bare = Params::new(lone_spinner_roi());
    bare.min_gain = 0.0;
    bare.min_gain_ratio = 0.0;
    let rows = judge_seq("lone-bare", &frames, bare);
    assert!(
        rows.iter().any(|r| r.verdict.shifted),
        "expected false drifts without thresholds"
    );
}

#[test]
fn known_shifts_are_found_with_an_unmasked_turning_spinner() {
    let frames = vec![
        text_frame(0, 0, 0),
        text_frame(1, 0, 3),
        text_frame(0, 2, 6),
        text_frame(-3, 1, 9),
    ];
    let rows = judge_seq("known-spin-nomask", &frames, Params::new(roi()));
    let got: Vec<_> = rows
        .iter()
        .map(|r| (r.verdict.dx, r.verdict.dy, r.verdict.shifted))
        .collect();
    assert_eq!(
        got,
        vec![(0, 0, false), (1, 0, true), (0, 2, true), (-3, 1, true)]
    );
    for r in rows.iter().skip(1) {
        let explained = 1.0 - r.verdict.best / r.verdict.diff0;
        assert!(
            explained >= 0.9,
            "frame {}: only {explained:.3} explained",
            r.index
        );
    }
}

#[test]
fn shift_beyond_radius_is_flagged_at_the_edge() {
    let frames = vec![
        smooth_frame(0, 0),
        smooth_frame(11, 0),
        smooth_frame(0, -12),
        smooth_frame(5, 0),
    ];
    let rows = judge_seq("edge", &frames, Params::new(roi()));
    let v = |i: usize| rows[i].verdict;
    // Past R the best offset sticks to the boundary on the right side, with
    // the edge mark set (whether it also passes the thresholds is not the
    // point: it is only partly explained at R).
    assert_eq!((v(1).dx, v(1).dy, v(1).at_edge), (8, 0, true), "{:?}", v(1));
    assert_eq!(
        (v(2).dx, v(2).dy, v(2).at_edge),
        (0, -8, true),
        "{:?}",
        v(2)
    );
    // Inside the radius: found exactly, judged shifted, no edge mark.
    assert_eq!(
        (v(3).dx, v(3).dy, v(3).shifted, v(3).at_edge),
        (5, 0, true, false)
    );
    assert!(!v(0).at_edge);
    let s = report::Summary::from_rows(&rows);
    assert_eq!(s.at_edge, 2);

    // A larger radius then finds the true drift, unmarked.
    let mut wide = Params::new(roi());
    wide.radius = 14;
    let rows = judge_seq("edge-wide", &frames, wide);
    let got: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.verdict.dx,
                r.verdict.dy,
                r.verdict.shifted,
                r.verdict.at_edge,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (0, 0, false, false),
            (11, 0, true, false),
            (0, -12, true, false),
            (5, 0, true, false)
        ]
    );
}

#[test]
fn sharp_drift_beyond_radius_is_marked_unexplained() {
    // A one-"row" drift of terminal-like text (sharp edges) well past R: the
    // cost does not fall toward the boundary, so no edge mark — but the
    // frame must not pass as unchanged either.
    let frames = vec![text_frame(0, 0, 0), text_frame(0, 18, 0)];
    let rows = judge_seq("sharp-far", &frames, Params::new(roi()));
    let v = rows[1].verdict;
    assert!(!v.shifted, "{v:?}");
    assert!(v.unexplained, "{v:?}");
    // With a radius that covers it, the drift is found and explained.
    let mut wide = Params::new(roi());
    wide.radius = 20;
    let rows = judge_seq("sharp-far-wide", &frames, wide);
    let v = rows[1].verdict;
    assert_eq!(
        (v.dx, v.dy, v.shifted, v.unexplained),
        (0, 18, true, false),
        "{v:?}"
    );
}

#[test]
fn spinner_without_mask_is_unexplained_not_shifted() {
    let rows = judge_seq(
        "spin-unexplained",
        &spinner_only_frames(),
        Params::new(roi()),
    );
    assert!(rows.iter().any(|r| r.verdict.unexplained));
    assert!(rows.iter().all(|r| !r.verdict.shifted));
    let mut masked = Params::new(roi());
    masked.masks.push(spinner_cell());
    let rows = judge_seq("spin-explained", &spinner_only_frames(), masked);
    assert!(rows.iter().all(|r| !r.verdict.unexplained));
}

#[test]
fn roi_outside_the_window_is_refused() {
    let base = text_frame(0, 0, 0);
    for bad in [
        Rect::new(200, 24, 60, 48), // past the right edge
        Rect::new(-1, 24, 60, 48),  // left of the client area
        Rect::new(40, 80, 60, 48),  // past the bottom
    ] {
        let Err(err) = Analyzer::new(Params::new(bad), &base) else {
            panic!("ROI {bad} must be refused");
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("outside the window") && msg.contains(&bad.to_string()),
            "{msg}"
        );
    }
    // Same through the directory path the CLI uses.
    let dir = TempDir::new("outside");
    seq::save_frame(&dir.0, 0, &base).unwrap();
    seq::write_index(&dir.0, &[(0, 0.0)]).unwrap();
    let out = TempDir::new("outside-out");
    let err = report::analyze_dir(&dir.0, Params::new(Rect::new(200, 24, 60, 48)), 0, &out.0)
        .expect_err("must refuse");
    assert!(format!("{err:#}").contains("outside the window"));
}

#[test]
fn analyze_dir_writes_csv_summary_and_pictures() {
    let dir = TempDir::new("dir");
    let frames = [
        text_frame(0, 0, 0),
        text_frame(0, 0, 0),
        text_frame(2, 0, 0),
    ];
    let mut times = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        seq::save_frame(&dir.0, i, f).unwrap();
        times.push((i, 10.0 * i as f64));
    }
    seq::write_index(&dir.0, &times).unwrap();
    let out = TempDir::new("dir-out");
    let s = report::analyze_dir(&dir.0, Params::new(roi()), 0, &out.0).unwrap();
    assert_eq!((s.frames, s.shifted, s.max_dx, s.max_dy), (3, 1, 2, 0));
    assert_eq!(
        (s.first_shift_index, s.first_shift_t_ms),
        (Some(2), Some(20.0))
    );
    let csv = std::fs::read_to_string(out.0.join(report::CSV_FILE)).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], CSV_HEADER);
    assert_eq!(lines.len(), 4);
    assert!(lines[3].starts_with("2,20.000,2,0,"), "{}", lines[3]);
    assert!(lines[3].ends_with(",1,0,0"), "{}", lines[3]);
    let side = image::open(out.0.join("shift_00002_side.png")).unwrap();
    assert_eq!((side.width(), side.height()), (160 * 2 + 4, 48));
    assert!(out.0.join("shift_00002_diff.png").exists());
    assert!(
        !out.0.join("shift_00001_side.png").exists(),
        "unchanged frames get no pictures"
    );
    assert!(
        std::fs::read_to_string(out.0.join(report::SUMMARY_FILE))
            .unwrap()
            .contains("first_shift: frame 2 at 20.000 ms")
    );
}
