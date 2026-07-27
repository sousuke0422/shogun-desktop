//! Deploy the vendored ConPTY pair (assets/conpty/) next to the built
//! binaries. portable-pty prefers a sideloaded conpty.dll over the system
//! one, and the modern host is what lets DCS (sixel) pass through ConPTY —
//! the Windows-inbox conhost strips it. See assets/conpty/README.md for
//! provenance; the pair must always move together.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/conpty");
    emit_build_hash();
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

/// `RIKKA_BUILD_HASH` = short commit, `+` when the tree was dirty at build
/// time, `unknown` outside a checkout (release tarballs, vendored builds).
/// The About page pairs it with the crate version so a deployed binary can
/// always be traced back to a commit — the whole point of shipping it.
fn emit_build_hash() {
    // Only the commit *identity* need invalidate this: rerunning on every
    // source edit would rebuild the crate constantly.
    for p in [".git/HEAD", ".git/refs/heads"] {
        println!("cargo:rerun-if-changed=../../{p}");
    }
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let hash = match git(&["rev-parse", "--short=7", "HEAD"]) {
        Some(h) => {
            // `--porcelain` prints one line per change; empty output = clean.
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            if dirty { format!("{h}+") } else { h }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=RIKKA_BUILD_HASH={hash}");
}
