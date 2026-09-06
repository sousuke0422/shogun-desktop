//! Process-wide graphics-protocol switches (`[terminal] sixel` /
//! `kitty_graphics` in the embedder's config).
//!
//! Disabling is two-sided by design: the payload path is skipped AND the
//! advertisement goes with it — DA1 drops its `;4` when sixel is off, and a
//! kitty `a=q` query gets no reply when kitty graphics is off, so a client
//! following the protocol's own detection (query, then DA1 as the fence)
//! concludes "unsupported" instead of transmitting images the terminal
//! would silently discard. A switch that only stopped drawing would be the
//! worst of both: apps still sending megabytes, nothing on screen.

use std::sync::atomic::{AtomicBool, Ordering};

static SIXEL: AtomicBool = AtomicBool::new(true);
static KITTY: AtomicBool = AtomicBool::new(true);

/// Apply the embedder's switches. Safe to call on every config reload.
pub fn configure(sixel: bool, kitty: bool) {
    SIXEL.store(sixel, Ordering::Relaxed);
    KITTY.store(kitty, Ordering::Relaxed);
    alacritty_terminal::term::ADVERTISE_SIXEL.store(sixel, Ordering::Relaxed);
}

pub fn sixel_enabled() -> bool {
    SIXEL.load(Ordering::Relaxed)
}

pub fn kitty_enabled() -> bool {
    KITTY.load(Ordering::Relaxed)
}
