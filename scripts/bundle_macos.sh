#!/usr/bin/env bash
# scripts/bundle_macos.sh
# shogun-desktop macOS .app バンドルを作成する
# 使用法: bash scripts/bundle_macos.sh [binary_path] [output_dir]
#   binary_path: shogun-desktop バイナリのパス（デフォルト: target/release/shogun-desktop）
#   output_dir:  出力先ディレクトリ（デフォルト: dist/）

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="${1:-$REPO_ROOT/target/release/shogun-desktop}"
OUT_DIR="${2:-$REPO_ROOT/dist}"
APP="$OUT_DIR/Shogun Desktop.app"

mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BINARY"               "$APP/Contents/MacOS/shogun-desktop"
cp "$REPO_ROOT/assets/Info.plist"   "$APP/Contents/Info.plist"

if [ -f "$REPO_ROOT/assets/icon.icns" ]; then
    cp "$REPO_ROOT/assets/icon.icns" "$APP/Contents/Resources/shogun-desktop.icns"
else
    echo "Warning: assets/icon.icns not found. App will use default icon." >&2
fi

chmod +x "$APP/Contents/MacOS/shogun-desktop"

# zip for distribution
cd "$OUT_DIR"
zip -r "shogun-desktop-macos.zip" "Shogun Desktop.app"
echo "Created: $OUT_DIR/shogun-desktop-macos.zip"
