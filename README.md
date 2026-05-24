# shogun-desktop

GPUI ベースのデスクトップクライアント。multi-agent-shogun の将軍・足軽・軍師を SSH 経由で監視・操作するための Phase 1 基盤です。

## 要件

- Windows（MSVC toolchain）
- [Rust](https://rustup.rs/)（`x86_64-pc-windows-msvc`）
- OpenSSH 互換のリモートホスト（WSL2 上の multi-agent-shogun など）

## ビルド

PowerShell で **Windows ネイティブ** の `cargo` を使用してください（WSL の cargo は不可）。

```powershell
cd C:\Users\aki\work\shogun-desktop
cargo build --release
```

警告なしで完了することを確認してください。ビルドログをローカルに残す場合（リポジトリには含めない）:

```powershell
cargo build --release 2>&1 | Tee-Object -FilePath build.log
```

`build.log` は `.gitignore` 済み。PowerShell の stderr リダイレクトでエラー風の行が混ざることがあるため、コミット対象にしない。

## 実行

```powershell
cargo run --release
```

起動すると GPUI ウィンドウが開き、下部に 4 タブ（将軍 / エージェント / 戦況 / 設定）が表示されます。

## 設定

設定タブから SSH 接続情報・プロジェクトパス・tmux セッション名を入力し **保存** します。
保存先: `%USERPROFILE%\.config\shogun-desktop\settings.toml`（Linux/WSL では `~/.config/shogun-desktop/settings.toml`）

- **SSH接続テスト**: リモートで `echo ok` を実行し結果を表示
- 認証: 秘密鍵パス、パスワード、または ssh-agent

## Phase 1 完成内容（cmd_170）

| 機能 | 状態 |
|------|------|
| 4 タブ骨格（将軍/エージェント/戦況/設定） | ✅ |
| 設定タブ（SSH・project_path・session 保存） | ✅ |
| `SshClient`（connect / exec / is_connected） | ✅ |
| `strip_ansi`（ANSI エスケープ除去） | ✅ |
| `theme.rs`（GPUI 0.2.2 カラーパレット） | ✅ |
| 将軍/エージェント/戦況タブの中身 | プレースホルダ（Phase 2） |

## Phase 2 以降の課題

- エージェント状態のリアルタイム更新（tmux / inbox / タスク YAML）
- 将軍・戦況タブへのダッシュボード連携
- ターミナル出力の ANSI 除去表示（`alacritty_terminal` 統合）
- SSH 経由コマンドの非同期ストリーミング

## プロジェクト構成

```
src/
  main.rs          # エントリポイント
  app.rs           # ウィンドウ公開 API
  window.rs        # メイン UI・タブ切替
  settings.rs      # TOML 設定の読み書き
  ssh.rs           # SSH クライアント
  ansi.rs          # ANSI 除去
  theme.rs         # 配色
  tabs/            # 各タブ UI
```

## ライセンス

Private / multi-agent-shogun 内部用
