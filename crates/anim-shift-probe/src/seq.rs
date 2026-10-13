//! The PNG sequence on disk: `frame_NNNNN.png` files plus `frames.csv`
//! (`index,t_ms,file`) carrying each frame's capture time. `capture` writes
//! it; `analyze` and the tests read it. A directory of bare PNGs without
//! `frames.csv` also loads (sorted by name, no timestamps).

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const INDEX_FILE: &str = "frames.csv";

#[derive(Clone, Debug, PartialEq)]
pub struct SeqFrame {
    pub index: usize,
    /// Milliseconds since the first captured frame; `None` without frames.csv.
    pub t_ms: Option<f64>,
    pub path: PathBuf,
}

pub fn frame_file_name(index: usize) -> String {
    format!("frame_{index:05}.png")
}

/// Write `frames.csv` for frames already saved as [`frame_file_name`].
pub fn write_index(dir: &Path, frames: &[(usize, f64)]) -> Result<()> {
    let mut out = String::from("index,t_ms,file\n");
    for &(index, t_ms) in frames {
        let _ = writeln!(out, "{index},{t_ms:.3},{}", frame_file_name(index));
    }
    std::fs::write(dir.join(INDEX_FILE), out).with_context(|| format!("write {INDEX_FILE}"))
}

/// Save one frame and return its file name (used by tests and tools that
/// build sequences by hand).
pub fn save_frame(dir: &Path, index: usize, img: &RgbaImage) -> Result<PathBuf> {
    let path = dir.join(frame_file_name(index));
    img.save(&path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn list(dir: &Path) -> Result<Vec<SeqFrame>> {
    let index = dir.join(INDEX_FILE);
    let frames = if index.exists() {
        let raw =
            std::fs::read_to_string(&index).with_context(|| format!("read {}", index.display()))?;
        let mut frames = Vec::new();
        for (n, line) in raw.lines().enumerate() {
            if n == 0 || line.trim().is_empty() {
                continue; // header
            }
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() != 3 {
                bail!("{}:{}: expected index,t_ms,file", index.display(), n + 1);
            }
            frames.push(SeqFrame {
                index: cols[0].trim().parse()?,
                t_ms: Some(cols[1].trim().parse()?),
                path: dir.join(cols[2].trim()),
            });
        }
        frames
    } else {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("read {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
            .collect();
        paths.sort();
        paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| SeqFrame {
                index,
                t_ms: None,
                path,
            })
            .collect()
    };
    if frames.is_empty() {
        bail!("no frames in {}", dir.display());
    }
    Ok(frames)
}

pub fn load(frame: &SeqFrame) -> Result<RgbaImage> {
    Ok(image::open(&frame.path)
        .with_context(|| format!("read {}", frame.path.display()))?
        .to_rgba8())
}
