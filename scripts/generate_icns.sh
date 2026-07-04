#!/usr/bin/env bash
# scripts/generate_icns.sh
# icon_macos.svg から macOS 用 .icns を生成する
# 依存: ImageMagick (magick), iconutil (macOS 標準)
# CI: brew install imagemagick  で magick を導入すること
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# macOS-specific artwork: dark rounded-square plate per platform convention
# (the bare-glyph icon.svg is for the Windows titlebar / exe resource).
SVG="$REPO_ROOT/assets/icon_macos.svg"
ICNS="$REPO_ROOT/assets/icon.icns"
TMP_DIR="$(mktemp -d)"
ICONSET="$TMP_DIR/shogun.iconset"
mkdir -p "$ICONSET"

render() { # size, output name
    magick -density 300 -background none "$SVG" -resize "$1x$1" "$ICONSET/$2"
}

# Apple's iconset member names are fixed: icon_<pt>x<pt>[@2x].png with
# pt ∈ {16,32,128,256,512}. Anything else (icon_64x64.png,
# icon_1024x1024.png) is not a valid member and gets dropped by iconutil,
# leaving the icns with missing representations.
for PT in 16 32 128 256 512; do
    render "$PT" "icon_${PT}x${PT}.png"
    render "$((PT * 2))" "icon_${PT}x${PT}@2x.png"
done

iconutil -c icns "$ICONSET" -o "$ICNS"
rm -rf "$TMP_DIR"
echo "Generated: $ICNS"
