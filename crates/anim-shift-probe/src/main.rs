//! anim-shift-probe — see the crate README.
//!
//! ```text
//! anim-shift-probe capture (--title SUBSTR | --hwnd HWND) --duration-ms N --out DIR
//! anim-shift-probe analyze --in DIR --roi x,y,w,h [--mask x,y,w,h]... [--radius R]
//!                          [--min-gain G] [--min-gain-ratio Q] [--base-index I] --out DIR
//! ```

use anim_shift_probe::analyze::{Params, Rect};
use anim_shift_probe::report;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const USAGE: &str = "usage:
  anim-shift-probe capture (--title SUBSTR | --hwnd HWND) --duration-ms N --out DIR
  anim-shift-probe analyze --in DIR --roi x,y,w,h [--mask x,y,w,h]... [--radius 8]
                           [--min-gain 1.0] [--min-gain-ratio 0.5] [--base-index 0] --out DIR";

struct Args(std::iter::Peekable<std::vec::IntoIter<String>>);

impl Args {
    fn value(&mut self, flag: &str) -> Result<String> {
        self.0
            .next()
            .with_context(|| format!("{flag} needs a value"))
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("anim-shift-probe: {e:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let mut args = Args(
        std::env::args()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .peekable(),
    );
    match args.0.next().as_deref() {
        Some("capture") => capture(args),
        Some("analyze") => analyze(args),
        _ => bail!("{USAGE}"),
    }
}

fn analyze(mut args: Args) -> Result<()> {
    let (mut input, mut out, mut roi) = (None, None, None);
    let (mut masks, mut radius, mut min_gain, mut ratio, mut base) =
        (Vec::new(), None, None, None, 0usize);
    while let Some(flag) = args.0.next() {
        match flag.as_str() {
            "--in" => input = Some(PathBuf::from(args.value(&flag)?)),
            "--out" => out = Some(PathBuf::from(args.value(&flag)?)),
            "--roi" => roi = Some(Rect::parse(&args.value(&flag)?)?),
            "--mask" => masks.push(Rect::parse(&args.value(&flag)?)?),
            "--radius" => radius = Some(args.value(&flag)?.parse()?),
            "--min-gain" => min_gain = Some(args.value(&flag)?.parse()?),
            "--min-gain-ratio" => ratio = Some(args.value(&flag)?.parse()?),
            "--base-index" => base = args.value(&flag)?.parse()?,
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    let (Some(input), Some(out), Some(roi)) = (input, out, roi) else {
        bail!("analyze needs --in, --out and --roi\n{USAGE}");
    };
    let mut params = Params::new(roi);
    params.masks = masks;
    if let Some(r) = radius {
        params.radius = r;
    }
    if let Some(g) = min_gain {
        params.min_gain = g;
    }
    if let Some(q) = ratio {
        params.min_gain_ratio = q;
    }
    let summary = report::analyze_dir(&input, params, base, &out)?;
    print!("{summary}");
    println!("csv: {}", out.join(report::CSV_FILE).display());
    Ok(())
}

#[cfg(windows)]
fn capture(mut args: Args) -> Result<()> {
    use anim_shift_probe::capture::{self, Options, Target};
    let (mut target, mut duration, mut out) = (None, None, None);
    while let Some(flag) = args.0.next() {
        match flag.as_str() {
            "--title" => target = Some(Target::Title(args.value(&flag)?)),
            "--hwnd" => {
                let v = args.value(&flag)?;
                let h = match v.strip_prefix("0x") {
                    Some(hex) => isize::from_str_radix(hex, 16)?,
                    None => v.parse()?,
                };
                target = Some(Target::Hwnd(h));
            }
            "--duration-ms" => {
                duration = Some(std::time::Duration::from_millis(
                    args.value(&flag)?.parse()?,
                ))
            }
            "--out" => out = Some(PathBuf::from(args.value(&flag)?)),
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    let (Some(target), Some(duration), Some(out)) = (target, duration, out) else {
        bail!("capture needs --title or --hwnd, --duration-ms and --out\n{USAGE}");
    };
    let stats = capture::run(&Options {
        target,
        duration,
        out: out.clone(),
    })?;
    println!(
        "frames: {} (dropped identical: {}) client: {}x{} dpi: {} size_changes: {} out: {}",
        stats.frames,
        stats.duplicates,
        stats.client_w,
        stats.client_h,
        stats.dpi,
        stats.resized_frames,
        out.display()
    );
    Ok(())
}

#[cfg(not(windows))]
fn capture(_args: Args) -> Result<()> {
    bail!("capture is Windows-only (Windows.Graphics.Capture); analyze works anywhere")
}
