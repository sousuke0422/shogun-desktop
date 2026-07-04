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
# The zip carries one top-level folder holding the .app plus a
# double-clickable first-run helper, so unzipping never scatters files.
PKG_DIR="$OUT_DIR/shogun-desktop-macos"
APP="$PKG_DIR/Shogun Desktop.app"

mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BINARY"               "$APP/Contents/MacOS/shogun-desktop"
# Inject the crate version into the bundle plist (the asset file carries a
# placeholder so the two can never drift apart).
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
sed "s/__VERSION__/${VERSION}/g" "$REPO_ROOT/assets/Info.plist" > "$APP/Contents/Info.plist"

if [ -f "$REPO_ROOT/assets/icon.icns" ]; then
    cp "$REPO_ROOT/assets/icon.icns" "$APP/Contents/Resources/shogun-desktop.icns"
else
    echo "Warning: assets/icon.icns not found. App will use default icon." >&2
fi

chmod +x "$APP/Contents/MacOS/shogun-desktop"

# First-run helper: strips the Gatekeeper quarantine that the zip download
# puts on the app, then launches it. Double-clickable from Finder
# (right-click → Open the first time, since the script itself is
# quarantined too).
cat > "$PKG_DIR/setup.command" <<'EOS'
#!/bin/bash
# shogun-desktop 初回セットアップ: ダウンロード隔離属性を外して起動する。
# 初回はこのファイル自体も隔離されているので 右クリック → 開く で実行のこと。
set -e
cd "$(dirname "$0")"
xattr -dr com.apple.quarantine "Shogun Desktop.app" 2>/dev/null || true
echo "quarantine を解除した。起動する…"
open "Shogun Desktop.app"
EOS
chmod +x "$PKG_DIR/setup.command"

# zip for distribution (one folder at the zip root)
cd "$OUT_DIR"
zip -r "shogun-desktop-macos.zip" "shogun-desktop-macos"
echo "Created: $OUT_DIR/shogun-desktop-macos.zip"
