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

   **⚠ 既定ターミナルを使っているなら、配備先の `OpenConsole.exe` を
   上書きした後で CLSID パッチを当て直すこと**（2026-08-03 に実際に壊した）。
   同梱の OpenConsole は Stable ブランドで、`-Embedding` COM サーバとしては
   **Windows Terminal の**コンソール CLSID `{2EACA947-…}` を名乗る。
   `packaging/install-default-terminal.ps1` の手順 1b が、**配備されたコピー
   だけ**をこちらの `{77F531BA-…}` にバイナリ書き換えしている。原本を
   そのまま配ると委譲が解決せず、Windows は黙って conhost に戻る
   （エラーは出ない。`rikka-handoff.exe` すら起動しない）。

   したがって **配備物の sha256 は原本と一致してはならない**。「原本と同じ
   なら最新」という配備スクリプトの検証は、この一点において逆である。
   確認は CLSID のバイト列で行う:

   ```powershell
   # 1 以上なら我々のブランドが入っている
   (Select-String -Path "$env:LOCALAPPDATA\RikkaTerminal\OpenConsole.exe" `
     -Encoding Byte -Pattern 'never-matches' -AllMatches) # 実用にはxxd等を使う
   ```
   ```sh
   xxd -p "$LOCALAPPDATA/RikkaTerminal/OpenConsole.exe" | tr -d '\n' \
     | grep -c ba31f577bd46804eb0df8e45e1f7183b
   ```
5. 検証: `pwsh -File e2e/rikka-sixel-local.ps1` を流し、スクショで
   **シェルのバナー/プロンプトが出ている**（ペア整合 OK）かつ
   **赤ブロックが描画されている**（DCS 素通し OK）ことを確認。
   ペイン無出力ならペア世代不整合——両ファイルを同一版で取り直すこと。

## 1.25 系（preview）を今は採らない — 2026-07-28 実測

1.25 の売りに **kitty keyboard protocol 対応**があり、実装は cascadia ではなく
`src/terminal/input/`（`terminalInput.hpp`: 1.24 で 0 箇所 → 1.25 で 22 箇所）に
入っている。そこは **OpenConsole/conhost もリンクする共有ライブラリ**である
（`src/host/VtIo.cpp` `input.cpp` `inputBuffer.cpp` `outputStream.cpp` が参照）。

見送る理由は「効かないと分かったから」では**ない**。以下が実測できた範囲。

### 測れたこと

1.25 のペアを積んでも**能力プローブ 12 項目は全通過**する（DECSLRM・sixel 含む）。
`RIKKA_PTY_DUMP` で「ホストが我々へ何を転送したか」を見ると、**両世代とも
kitty のシーケンスは素通ししている**:

| 送ったもの | 1.24.260710001 | 1.25.260710002-preview |
|---|---|---|
| query `CSI ?u` | 届く | 届く |
| PUSH `CSI >1u` | 届く | 届く |
| SET `CSI =5;1u` | 届く | 届く |
| POP `CSI <u` | 届く | 届く |
| `?1049h` / `?1049l` | 届く | 届く |

つまり **「client の push は conhost に食われるので ConPTY 経由では原理的に
機能しない」という従来の断定は、現行ホストでは再現しない。** 単発クエリに
`CSI ?u` の応答が返らないのは、こちらが `mark_conpty()` で広告を止めている
からであって、ホストの能力を示す証拠ではない（XTVERSION は同じ経路を通って
`rikka-terminal 0.1.0` を返す）。

### 測れていないこと（ここが本丸）

上の試験は各シーケンスを **0.4 秒間隔で個別に**送っている。yazi 事件の実態は
**プロセス終了時の teardown burst を途中から丸呑みされる**ことで、その条件は
一切再現していない。`mark_conpty()` の kitty 無効化を外してよいかは、
`alt_exit_probe`（要 yazi）でバーストを再現するまで**判断材料が無い**。

### 判断

したがって 1.25 は「見返りが無い」のではなく、**見返りがあるか未確認**。
preview を配布物に積む前に上のバースト検証を通すのが順序。1.25 固有で確実に
価値があるのは `winconpty.h` に増えた `PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE (0x20)`
（East Asian Ambiguous 幅の解釈をホストと合意する口）だが、これは
`CreatePseudoConsole` に渡して初めて効くので、portable-pty がこのフラグを
通せるかの確認が先。
