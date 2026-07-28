# Sideloaded ConPTY (matched pair)

Microsoft の ConPTY 再配布パッケージから取り出した **同一ビルドのペア**。
portable-pty は起動時に実行ファイルの隣の `conpty.dll` を優先ロードし、
conpty.dll が同じ場所の `OpenConsole.exe` を PTY ホストとして起動する
（wezterm と同じ機構）。Windows 同梱の古い conhost は DCS（sixel 等）を
剥がすため、これが**ローカル sixel の前提**になる。

- 出典: NuGet `Microsoft.Windows.Console.ConPTY` **1.24.260710001**
  (https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY —
  microsoft/terminal プロジェクト公式・MIT License)
- `runtimes/win-x64/native/conpty.dll`
  sha256 39fba2713e249511… (109,920 bytes)
- `build/native/runtimes/x64/OpenConsole.exe`
  sha256 b7fd936c2668b87b… (1,066,296 bytes)

この `OpenConsole.exe` は **Windows Terminal v1.24.11911.0 が同梱している物とバイト単位で同一**
（同梱パッケージ内の実体と sha256 一致を確認済み）。つまり広く実運用されている版である。

**必ずペアで更新すること** — conpty.dll と OpenConsole.exe の世代が
食い違うと PTY が無出力になる（wezterm 2024-02 の dll × 1.24 の exe で実証）。
build.rs がビルドのたびにこの 2 ファイルをバイナリの隣へコピーする。

## 更新手順

1. 新版確認: <https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY>
   （API: `https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/index.json`）
2. nupkg を取得して展開（nupkg = zip）。**必ず同一版から 2 ファイル取ること**:
   - `runtimes/win-x64/native/conpty.dll`
   - `build/native/runtimes/x64/OpenConsole.exe`

   ```powershell
   $v = "1.24.260512001"   # 新しい版に置き換える
   $u = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$v/microsoft.windows.console.conpty.$v.nupkg"
   Invoke-WebRequest $u -OutFile conpty.nupkg
   Expand-Archive conpty.nupkg -DestinationPath conpty-pkg
   Copy-Item conpty-pkg/runtimes/win-x64/native/conpty.dll .
   Copy-Item conpty-pkg/build/native/runtimes/x64/OpenConsole.exe .
   Get-FileHash conpty.dll, OpenConsole.exe -Algorithm SHA256
   ```

3. この README の版数と sha256 を書き換える。
4. `cargo build --release -p rikka-terminal`（build.rs が隣へ配置し直す）。
5. 検証: `pwsh -File e2e/rikka-sixel-local.ps1` を流し、スクショで
   **シェルのバナー/プロンプトが出ている**（ペア整合 OK）かつ
   **赤ブロックが描画されている**（DCS 素通し OK）ことを確認。
   ペイン無出力ならペア世代不整合——両ファイルを同一版で取り直すこと。

## なぜ 1.25 系（preview）を採らないか — 2026-07-28 実測

1.25 の売りに **kitty keyboard protocol 対応**があり、`src/terminal/input/`
（cascadia ではなく**ホスト側の共有入力層**）に実装が入っている
（`terminalInput.hpp`: 1.24 で 0 箇所 → 1.25 で 22 箇所）。`mark_conpty()` で
kitty keyboard の広告を止めている我々には効きそうに見えるが、**効かない**。

1.25.260710002-preview のペアを実際に積んで ConPTY 越しに問い合わせた結果:

| クエリ | 応答 |
|---|---|
| XTVERSION `CSI >0q` | `rikka-terminal 0.1.0` — 素通しで**我々**が応答 |
| DA1 `CSI c` | `?62;4;22c`（`4` = sixel 健在） |
| **kitty kbd `CSI ?u`** | **無応答** |
| DECRQM `?2026` | `?2026;2$y`（認識済み） |

ホストは XTVERSION と同じくクエリを**素通しするだけ**で、kitty keyboard を
引き受けない。1.25 の実装は Windows Terminal 自身が端末として振る舞うための
物であって、ConPTY 経路には出てこない。能力プローブ 12 項目は 1.25 でも全通過
するので「動かない」わけではないが、**preview を積む見返りが無い**。

1.25 固有で価値があるのは `winconpty.h` に増えた
`PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE (0x20)`（East Asian Ambiguous 幅の解釈を
ホストと合意する口）だが、これは `CreatePseudoConsole` に渡して初めて効く。
portable-pty がこのフラグを通せるか確認するまでは使えない。**採るならその
確認が先。**
