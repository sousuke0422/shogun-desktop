# E2E — 実 GUI 自動テスト

実バイナリを起動し、合成マウス/キーボード入力で操作して結果を検証する
PowerShell スクリプト群。2026-07-03 の「選択ハイライト不可視」調査で
実際に原因切り分け・修正検証に使ったもの。

CI には組み込めない（対話デスクトップと SSH/tmux 接続が必要）。
入力・描画まわりを触ったときの手動リグレッション用。

## スクリプト

| script | 検証内容 |
|---|---|
| `drag-copy-test.ps1` | 起動 → 端末ペインをドラッグ選択 → スクショ保存 → Ctrl+Shift+C → クリップボードに選択テキストが入れば PASS |
| `scan-highlight.ps1` | 上記スクショを画素走査し、選択ハイライト色（青系）が閾値以上あれば PASS |

## 実行（WSL から）

```bash
powershell.exe -NoProfile -ExecutionPolicy Bypass \
  -File 'C:\Users\dev\work\shogun-desktop\e2e\drag-copy-test.ps1' \
  -ExePath 'C:\Users\dev\work\shogun-desktop\target-verify\release\shogun-desktop.exe'

powershell.exe -NoProfile -ExecutionPolicy Bypass \
  -File 'C:\Users\dev\work\shogun-desktop\e2e\scan-highlight.ps1'
```

両方 `PASS` / exit 0 で合格。スクショは既定で `%TEMP%\shogun-e2e-sel.png`。

## ハマりどころ（実測済み）

- **合成キーはフォアグラウンド依存**: 合成マウスはカーソル位置のウィンドウに
  届くが、合成キーはキーボードフォーカスに従う。クリック後でも foreground が
  奪われているとキーが別窓に落ちる（初回実装はこれで偽 FAIL）。スクリプトは
  送信前に `GetForegroundWindow` を検証する。
- **ウィンドウ特定は PID で**: `FindWindow` の日本語タイトル一致は不安定。
  `EnumWindows` + `GetWindowThreadProcessId` で探す。
- **.ps1 の文字コード**: Windows PowerShell 5.1 は BOM なし UTF-8 を ANSI と
  して解釈し、日本語リテラルがパースエラーになる。スクリプトは ASCII のみで
  書くか、UTF-8 with BOM で保存する。
- 実行中はカーソルを実際に動かすので、テスト中はマウスに触らない。
