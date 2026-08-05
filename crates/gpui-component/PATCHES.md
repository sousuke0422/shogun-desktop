# Vendored gpui-component — local patches

Vendored from crates.io `gpui-component 0.5.1` (path-patched via the
workspace `[patch.crates-io]`). Same drill as `crates/gpui/PATCHES.md`:
every local change is listed here so a re-vendor can replay them.

| Commit | Area | What it changes | Why we need it |
|--------|------|-----------------|----------------|
| _this commit_ | `TextView::selection_of` (`src/text/text_view.rs`) | New pub fn: look up the keyed `TextViewState` for an element id and return its current selection text | `TextViewState` and `selection_text()` are `pub(crate)`, so an embedder cannot offer "copy selection" in a context menu. One deliberate crack in the wall; everything else stays crate-private |
| _this commit_ | Selection mouse handlers left-only (`src/text/text_view.rs`) | The selectable mouse-down handlers ignore every button but left | They fired for ANY button, so a right-click restarted (or cleared) the selection — collapsing exactly what a context menu's "copy selection" was about to act on |

## Re-vendor checklist

1. Copy the new upstream tree over `crates/gpui-component/`, drop
   `.cargo-checksum.json` / `.cargo-ok` / `Cargo.lock`.
2. Replay the table above.
3. `cargo build --release -p shogun-desktop` must update `Cargo.lock`
   (the vte lesson: a `[patch.crates-io]` entry that the lockfile does not
   reflect fails every `--locked` invocation).
