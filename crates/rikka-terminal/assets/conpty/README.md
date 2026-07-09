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
