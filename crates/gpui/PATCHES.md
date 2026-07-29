# Vendored gpui — local patches

gpui is vendored (not a crates.io dependency) because the terminal needs
behavior upstream does not provide. Every local change is listed here so a
future re-vendor is a review of this list rather than an archaeology dig.

Regenerate the current diff against the vendored baseline with:

```sh
git log --oneline -- crates/gpui/
```

| Commit | Area | What it changes | Why the terminal needs it |
|--------|------|-----------------|---------------------------|
| `44b69fd` | DirectWrite font fallback | Embedded fallback fonts resolve instead of being skipped | Bundled fonts (Twemoji, the mono face) were invisible to the fallback chain |
| `6f5f9ae` | COLR glyph rasterization | Union the layer bounds instead of trusting the base outline | COLR emoji have an empty base outline → 0×0 raster → emoji vanished |
| `0f67163` | OpenType features | Per-instance `font_features` plumbed into shaping | ghostty-parity ligature control (`+liga` / `-calt` …) |
| `716d1ee` | Typography defaults | Seed the default feature set into every typography | A `FontFeatures` with no entries otherwise disabled the font's own defaults |
| `89e241c` | Cluster math | Harden RTL / non-monotonic cluster handling | Saturating math; combining marks survive the snapshot round-trip |
| `1031a65` | `force_width` glyph pinning | Pin every glyph to its cell unconditionally (cluster-relative delta preserves combining marks) | A conditional snap made glyph x depend on shaping context, so animated lines whose run boundaries shift each frame danced left-right |
| `dc9f382` | `Window::set_position` (`window.rs`, `platform.rs`, `platform/windows/window.rs`) | New API: move a window without resizing or activating it (`SWP_NOSIZE \| SWP_NOACTIVATE`). Default no-op on other platforms | A window cannot draw outside itself, so carrying a tab drag across the desktop means moving a real borderless unfocused window under the cursor. `SWP_NOACTIVATE` is load-bearing: taking focus would end the drag |
| _this commit_ | Drag preview skipped in popups (`window.rs`) | `Window` remembers its `WindowKind`; `draw` no longer paints `App::active_drag` in a `PopUp` | `active_drag` is App-global, so every window drawn during a drag paints its own copy of the preview. The follower popup exists to BE that preview, so it drew a second one over itself — two overlapping chips |

## Re-vendor checklist

1. Take the new upstream tree, then replay the table above in order — each
   commit is small and touches one concern.
2. `force_width` (`text_system/line_layout.rs`) is the one most likely to
   have moved upstream: it is a rikka-only API. If upstream grows its own
   grid-pinning, prefer theirs and delete ours, but keep the
   *unconditional* pinning property — the conditional form is a
   frame-instability bug, not a micro-optimization.
3. Gate: `cargo fmt --all && cargo test --workspace`, then run a terminal
   with an animated TUI (codex, or `crates/rikka-terminal` + any spinner)
   and confirm the text does not jitter horizontally.
