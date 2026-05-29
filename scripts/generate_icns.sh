#!/usr/bin/env bash
# scripts/generate_icns.sh
# icon.svg から macOS 用 .icns を生成する
# 依存: ImageMagick (magick), iconutil (macOS 標準)
# CI: brew install imagemagick  で magick を導入すること

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SVG="$REPO_ROOT/assets/icon.svg"
ICNS="$REPO_ROOT/assets/icon.icns"
TMP_DIR="$(mktemp -d)"
ICONSET="$TMP_DIR/shogun.iconset"
mkdir -p "$ICONSET"

# ImageMagick で各サイズの PNG を生成
# -density 300: 高解像度でラスタライズしてから resize（品質向上）
# -background none: 透過を保持
for SIZE in 16 32 64 128 256 512 1024; do
    magick -density 300 -background none "$SVG" \
        -resize "${SIZE}x${SIZE}" \
        "$ICONSET/icon_${SIZE}x${SIZE}.png"
done

# @2x バリアント（HiDPI）
cp "$ICONSET/icon_32x32.png"     "$ICONSET/icon_16x16@2x.png"
cp "$ICONSET/icon_64x64.png"     "$ICONSET/icon_32x32@2x.png"
cp "$ICONSET/icon_256x256.png"   "$ICONSET/icon_128x128@2x.png"
cp "$ICONSET/icon_512x512.png"   "$ICONSET/icon_256x256@2x.png"
cp "$ICONSET/icon_1024x1024.png" "$ICONSET/icon_512x512@2x.png"

iconutil -c icns "$ICONSET" -o "$ICNS"
rm -rf "$TMP_DIR"
echo "Generated: $ICNS"
