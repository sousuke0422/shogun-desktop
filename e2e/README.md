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

## 実行（WSL から。`$REPO` = リポジトリの Windows パス）

```bash
pwsh.exe -NoProfile -File "$REPO\\e2e\\drag-copy-test.ps1" \
  -ExePath "$REPO\\target-verify\\release\\shogun-desktop.exe"

pwsh.exe -NoProfile -File "$REPO\\e2e\\scan-highlight.ps1"
```

両方 `PASS` / exit 0 で合格。スクショは既定で `%TEMP%\shogun-e2e-sel.png`。
`-ExePath` 省略時は `<repo>\target\release\` のバイナリを使う。

pwsh 7 を使うこと（RemoteSigned でフラグ不要）。powershell.exe 5.1 は WSL 継承の
PSModulePath が v7 モジュールを誤ロードして Security モジュールごと壊れる環境がある。

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
- **画面ロック中は不成立**: 合成キーはロック画面に落ち、スクショもロック画面を写す
  （scan-highlight はロック画面の青で偽 PASS しうる）。対話デスクトップ必須。

## アニメーションの領域のずれ（`anim-shift/`）

rt の中で、刻つきの録りを流し直す。その間に `anim-shift-probe` で窓を撮り、指定した領域（ROI）が最初の frame からずれた frame を拾う。道具そのものの説明は `crates/anim-shift-probe/README.md` にある。

| file | 役目 |
|---|---|
| `anim-shift/synth-anim.sh` | 合成の擬似アニメーション。spinner と二行の状態の塊を `\r`・行消し・カーソル上げで描き換える。何も起こさず、何も読まない。三つ目の引数を与えると、その frame で塊を一行下げる（ずれの陽性対照） |
| `anim-shift/make-synth-recording.sh` | `synth-anim.sh` を `script --log-timing` で録る |
| `anim-shift/replay-in-rt.sh` | rt の中で `scriptreplay` を用いて録りを流し直す |
| `anim-shift/anim-shift-run.ps1` | rt を起こし、流し直しながら撮り、ROI があれば解く。終われば窓を WM_CLOSE で閉じる |

### 手順（WSL から。`$REPO` = リポジトリの Windows パス）

```bash
cargo.exe build --release -p rikka-terminal -p anim-shift-probe   # Windows の cargo
bash e2e/anim-shift/make-synth-recording.sh /tmp/rec 80 0.08 0     # 合成の録り（ずれ無し）
pwsh.exe -NoProfile -File "$REPO\\e2e\\anim-shift\\anim-shift-run.ps1" \
  -Timing /tmp/rec/timing -Typescript /tmp/rec/typescript -OutDir "$REPO\\target\\anim-shift"
# 一度目は ROI 無しで撮り、frames\frame_00000.png を見て ROI と mask を決める。
# 決めたら撮り直すか、撮った列をそのまま解く:
anim-shift-probe.exe analyze --in ...\frames --roi x,y,w,h --mask x,y,w,h --out ...\result
```

- ROI と mask は窓の client 座標で、物理 px である（150% なら cell も 1.5 倍）。
- `-Isolated`（既定で on）を付けると、`--window-process` で起こす。既に rt が動いている時でも、その窓をこの exe が描く。付けなければ、起動が動いている rt へ渡され、そちらの exe が描く。
  - 窓が開いている間は、動いている rt の窓の台帳へ顔を出す。

### 本物の録り方（Linux 側。殿が録る）

codex や Claude Code の画面を録るには、録りたい端末で次を打つ。録りを止めるには `exit` する。

```bash
script -q --log-timing ~/rec/codex.timing --log-out ~/rec/codex.typescript
codex            # または claude。動かして見たい所まで進め、終えたら exit
exit
```

流し直しは `anim-shift-run.ps1 -Timing ~/rec/codex.timing -Typescript ~/rec/codex.typescript …`（WSL の道）で行う。

**録りは commit するな。** 録りには、画面に出た物がそのまま残る。鍵・token・道・私事の文を含みうる。`e2e/anim-shift/.gitignore` は `timing`・`typescript`・`rec*/`・`out*/` を弾くが、それより外の場所（`/tmp` や `~/rec`）に置くこと。
