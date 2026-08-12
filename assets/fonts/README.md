# Bundled fonts

## Twemoji.ttf — colour emoji (COLRv0)

Built with **nanoemoji** (`glyf_colr_0`, `clipbox_quantization = 32`) from
the SVG assets of **jdecked/twemoji v17.0.3** (Emoji 17). The exact build
is scripted: **`build-twemoji.sh`** in this directory — run it, then copy
the output over both bundled copies and re-pin the glyph-id tests. The
recipe is that of the Arch AUR **`twemoji-fonts`** package (maintainer:
Coelacanthus <uwu@coelacanthus.name>), reduced to the one target we bundle
and made self-contained (pip venv only; resvg/pngquant are needed only for
the CBDT targets we don't build). nanoemoji derives the cmap and the
ZWJ/VS16/keycap/tag GSUB ligatures from the renamed asset file names.

- Graphics: CC-BY 4.0 (jdecked/twemoji, the Twemoji continuation)
- sha256 of the bundled build: `159b826079554b99…` (see git history for
  the full hash of each revision)
- The engine's ligature tests pin glyph ids of this exact build
  (`emoji_shape.rs`) — rebuild ⇒ re-pin.

The same file must exist at both `assets/fonts/Twemoji.ttf` (shogun-desktop)
and `crates/rikka-terminal/assets/fonts/Twemoji.ttf` (rikka-terminal +
`emoji_shape` `include_bytes!`).

## MoralerspaceHWNeon-Regular.ttf / font-logos.ttf

See the respective upstream licences (unchanged by this note).
