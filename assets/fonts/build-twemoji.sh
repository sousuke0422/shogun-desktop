#!/usr/bin/env bash
# Rebuild assets/fonts/Twemoji.ttf from jdecked/twemoji sources.
#
# This is the recipe of the Arch AUR `twemoji-fonts` package
# (https://aur.archlinux.org/pkgbase/twemoji-fonts, maintainer: Coelacanthus
# <uwu@coelacanthus.name>), reduced to the one target we bundle
# (glyf_colr_0) and made self-contained: the AUR build.sh uses perl-rename
# and sed templating; here the rename and the .toml generation are inline
# python, and every tool comes from a throwaway pip venv — no system
# packages. resvg/pngquant are NOT needed (they are for the CBDT bitmap
# targets only).
#
#   bash assets/fonts/build-twemoji.sh [TAG]     # default: the pinned tag
#
# Output: build/TwemojiCOLRv0.ttf under a work dir printed at the end.
# To adopt a build: copy it to BOTH assets/fonts/Twemoji.ttf and
# crates/rikka-terminal/assets/fonts/Twemoji.ttf, then re-pin the glyph ids
# in crates/rikka-terminal-core/src/emoji_shape.rs (the failing assertions
# print the new ids). See assets/fonts/README.md.
set -eu

TAG="${1:-v17.0.3}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/twemoji-build.XXXXXX")"
echo "work dir: $WORK"
cd "$WORK"

python3 -m venv .venv
.venv/bin/pip -q install nanoemoji ninja

git clone -q --depth 1 --branch "$TAG" https://github.com/jdecked/twemoji.git

# Rename assets/svg/*.svg to nanoemoji's emoji_uXXXX[_YYYY...].svg
# convention (hex parts zero-padded to 4, '-' -> '_'), and generate the
# glyf_colr_0 toml. nanoemoji derives the cmap and the ZWJ/VS16/keycap/tag
# GSUB ligatures from these file names — which is also why Twemoji's
# FE0F-less keycap asset names need the VS16-strip retry in emoji_shape.rs.
python3 - <<'PY'
import os

d = 'twemoji/assets/svg'
for name in os.listdir(d):
    if not name.endswith('.svg'):
        continue
    parts = [f'{int(p, 16):04x}' for p in name[:-4].split('-')]
    new = 'emoji_u' + '_'.join(parts) + '.svg'
    if new != name:
        os.rename(os.path.join(d, name), os.path.join(d, new))

files = sorted(f for f in os.listdir(d) if f.endswith('.svg'))
print(f'{len(files)} svg assets')

toml = [
    'family = "Twemoji"',
    'output_file = "TwemojiCOLRv0.ttf"',
    'color_format = "glyf_colr_0"',
    'clipbox_quantization = 32',
    '',
    '[axis.wght]', 'name = "Weight"', 'default = 400', '',
    '[master.regular]', 'style_name = "Regular"', 'srcs = [',
]
toml += [f'"{d}/{f}",' for f in files]
toml += [']', '', '[master.regular.position]', 'wght = 400', '']
open('twemoji_glyf_colrv0.toml', 'w').write('\n'.join(toml))
PY

PATH="$WORK/.venv/bin:$PATH" nanoemoji twemoji_glyf_colrv0.toml

echo
echo "built: $WORK/build/TwemojiCOLRv0.ttf"
sha256sum "$WORK/build/TwemojiCOLRv0.ttf"
