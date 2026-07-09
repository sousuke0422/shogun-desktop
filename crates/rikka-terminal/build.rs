//! Deploy the vendored ConPTY pair (assets/conpty/) next to the built
//! binaries. portable-pty prefers a sideloaded conpty.dll over the system
//! one, and the modern host is what lets DCS (sixel) pass through ConPTY —
//! the Windows-inbox conhost strips it. See assets/conpty/README.md for
//! provenance; the pair must always move together.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/conpty");
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    // OUT_DIR = <target>/<profile>/build/<pkg-hash>/out — the binaries live
    // three levels up. A layout mismatch (custom runners) just skips the
    // copy; the terminal then falls back to the system ConPTY.
    let Ok(out) = std::env::var("OUT_DIR") else {
        return;
    };
    let mut dir = PathBuf::from(out);
    for _ in 0..3 {
        dir = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };
    }
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/conpty");
    for name in ["conpty.dll", "OpenConsole.exe"] {
        let src = assets.join(name);
        if src.exists() {
            let _ = std::fs::copy(&src, dir.join(name));
        }
    }
}
