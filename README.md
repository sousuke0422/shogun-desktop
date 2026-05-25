# shogun-desktop

GPUI 0.2.2 ベースの Windows デスクトップクライアント。
multi-agent-shogun の将軍・足軽・軍師を SSH 経由で監視・操作する。

## 要件

- Windows（MSVC toolchain）
- [Rust](https://rustup.rs/)（`x86_64-pc-windows-msvc`）
- Node.js（アイコン再生成時のみ: `scripts/gen_icon_ico.mjs`）
- OpenSSH 互換のリモートホスト（WSL2 上の multi-agent-shogun など）

## ビルド

PowerShell で **Windows ネイティブ** の `cargo` を使用してください（WSL の cargo は不可）。

```powershell
cd C:\Users\aki\work\shogun-desktop
cargo build --release
cargo test
```

## 実行

```powershell
cargo run --release
```

起動すると GPUI ウィンドウが開き、下部に 5 タブが表示されます。

## 機能

| タブ | 内容 |
|------|------|
| 将軍 | PTY ターミナル（shogun tmux セッション監視・操作） |
| エージェント | 全エージェント稼働状態一覧（SSH 経由） |
| 戦況 | dashboard.md リアルタイム表示（SSH 経由） |
| 設定 | SSH 接続情報・project_path・tmux セッション名 |

## 設定

設定タブから SSH 接続情報を入力し **保存** します。
保存先: `%USERPROFILE%\.config\shogun-desktop\settings.toml`

- 認証: 秘密鍵パス / パスワード / ssh-agent
- SSH ControlMaster 多重化で接続オーバーヘッドを削減
- Win32-OpenSSH ControlMaster 非対応時は自動フォールバック

## アーキテクチャ

→ [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## ライセンス

Private / multi-agent-shogun 内部用

アイコン: Copyright (c) 2026 yohey-w — [CREDITS](CREDITS) 参照
