# shogun-desktop

GPUI 0.2.2 ベースのデスクトップクライアント（Windows / macOS）。
multi-agent-shogun の将軍・家老・足軽・軍師を SSH 経由で監視・操作する。

## 要件

- Windows（MSVC toolchain）または macOS 14+（Apple Silicon）
- [Rust](https://rustup.rs/)（Windows: `x86_64-pc-windows-msvc` / mac: `aarch64-apple-darwin`）
- Node.js（アイコン再生成時のみ: `scripts/gen_icon_ico.mjs`）
- OpenSSH 互換のリモートホスト（WSL2 上の multi-agent-shogun など）

ビルド済みバイナリは GitHub Actions の CI アーティファクト
（Windows exe / macOS .app zip）から取得できる。`v*` タグで Release に恒久添付される。
mac では zip 展開後に `xattr -dr com.apple.quarantine shogun-desktop.app` が必要。

## ビルド

Windows では **Windows ネイティブ** の `cargo` を使用（WSL の cargo は不可）。

```powershell
cd <repo-path>  # 例: C:\work\shogun-desktop
cargo build --release
cargo test
```

## 実行

```powershell
cargo run --release
```

## 機能

下部タブで画面を切り替える:

| タブ | 内容 |
|------|------|
| 将軍 | PTY ターミナル（shogun tmux セッション監視・操作） |
| エージェント | 全エージェント稼働状態一覧（SSH 経由） |
| 戦況 | dashboard.md リアルタイム表示（SSH 経由） |
| 設定 | SSH 接続情報・project_path・tmux セッション名・フォント |
| 家老陣 | PTY ターミナル（multiagent tmux セッション） |

設定タブの「シェルを開く」で、tmux に紐づかない素の SSH シェル窓
（スクロールバック・履歴ページング付き）を別ウィンドウで開ける。

ターミナルは自前レンダラ（alacritty_terminal ベース）:

- 日本語 IME インライン変換・マウス選択コピー（Ctrl+Shift+C / cmd-c）・
  ブラケットペースト（Ctrl+Shift+V / cmd-v）
- マウスレポーティング転送（btop 等）・alternate scroll（less / man）・
  OSC 52 クリップボード双方向・synchronized output (?2026)・truecolor
- 絵文字は Twemoji Mozilla（COLRv0）同梱で Windows / mac 同一絵柄
- イベント駆動描画（アイドル時 CPU ほぼゼロ）

## 設定

設定タブから SSH 接続情報を入力し **保存** する。
保存先: `%USERPROFILE%\.config\shogun-desktop\settings.toml`

- 認証: 秘密鍵パス / パスワード / ssh-agent
- 接続バックエンド: Native (russh) / System (ssh.exe)。Native は keepalive 15s 付き
- SSH ControlMaster 多重化で接続オーバーヘッドを削減
  （Win32-OpenSSH ControlMaster 非対応時は自動フォールバック）

## テスト

- ユニットテスト: `cargo test`
- GUI E2E（実バイナリ＋合成入力）: [e2e/](e2e/) — pwsh 7 で実行、対話デスクトップ必須

## アーキテクチャ

→ [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## ライセンス

本リポジトリのソース（`src/` および `crates/` 配下の独自コード）は [MIT](LICENSE) です。
エージェント状態の YAML 解析・カード組み立ては MIT の
[`crates/shogun-core`](crates/shogun-core/)（shogun-suite からの暫定同梱）に依存し、
GPUI UI 層から分離しています。

**GPL 参照ポリシー**: 実装参照は仕様書 → alacritty (Apache-2.0) →
ghostty / wezterm / Windows Terminal (MIT) の順で、Zed / cosmic-term（GPL）の
ソース参照は最終手段。現時点で GPL 由来コードの取り込みはありません
（取り込んだ場合は配布物全体が GPL-3.0 再ライセンス対象になります）。

同梱フォント・アイコンのクレジットは [CREDITS](CREDITS) 参照
（Twemoji Mozilla: 絵柄 CC-BY 4.0 / フォントコード Apache-2.0、
アイコン: Copyright (c) 2026 yohey-w）。
