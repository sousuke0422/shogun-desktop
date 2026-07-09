# Sideloaded ConPTY (matched pair)

Microsoft の ConPTY 再配布パッケージから取り出した **同一ビルドのペア**。
portable-pty は起動時に実行ファイルの隣の `conpty.dll` を優先ロードし、
conpty.dll が同じ場所の `OpenConsole.exe` を PTY ホストとして起動する
（wezterm と同じ機構）。Windows 同梱の古い conhost は DCS（sixel 等）を
剥がすため、これが**ローカル sixel の前提**になる。

- 出典: NuGet `Microsoft.Windows.Console.ConPTY` **1.24.260512001**
  (https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY —
  microsoft/terminal プロジェクト公式・MIT License)
- `runtimes/win-x64/native/conpty.dll`
  sha256 c46dcd04f52b97f6… (109,880 bytes)
- `build/native/runtimes/x64/OpenConsole.exe`
  sha256 47828c3fe080212f… (1,066,296 bytes)

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
