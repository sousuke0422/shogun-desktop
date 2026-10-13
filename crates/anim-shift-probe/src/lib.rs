//! Finds frames where a region of a window has drifted away from where it
//! sat in the first frame, as an integer pixel offset `(dx, dy)`.
//!
//! - [`analyze`]: the judgement (pure, any OS).
//! - [`seq`]: the on-disk PNG sequence both halves share.
//! - [`report`]: CSV, summary and the side-by-side / diff PNGs.
//! - `capture` (Windows only): records one PNG per window update.

pub mod analyze;
#[cfg(windows)]
pub mod capture;
pub mod report;
pub mod seq;

pub use analyze::{Analyzer, Params, Rect, Verdict};
