# Bundled fonts

## Twemoji.ttf — colour emoji (COLRv0)

Built with **nanoemoji** (`glyf_colr_0`, `clipbox_quantization = 32`) from
the SVG assets of **jdecked/twemoji v17.0.3** (Emoji 17), following the Arch
`twemoji-fonts` AUR recipe: rename `assets/svg/*.svg` to the
`emoji_uXXXX[_YYYY…].svg` convention (hex parts zero-padded to 4 digits,
`-` → `_`, `emoji_u` prefix), list every file as a `srcs` entry, family
`"Twemoji"`. nanoemoji derives the cmap and the ZWJ/VS16/keycap/tag GSUB
ligatures from the file names.

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
