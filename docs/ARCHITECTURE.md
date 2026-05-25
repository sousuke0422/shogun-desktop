# shogun-desktop — アーキテクチャ設計書

## 概要

`shogun-desktop` は [multi-agent-shogun](https://github.com/yohey-w/multi-agent-shogun) の
Windows デスクトップクライアント。SSH 経由でリモート WSL2 上のエージェント群を監視・操作する。

**技術スタック**

| 役割 | クレート / ツール |
|------|-----------------|
| UI フレームワーク | GPUI 0.2.2 (Zed 由来) |
| PTY エミュレーション | portable-pty 0.8 (ConPTY backend) |
| VTE 状態機械 | alacritty_terminal 0.24.2 |
| SSH 接続 | システム `ssh.exe` サブプロセス + ControlMaster |
| アイコン埋め込み | winres 0.1 (Windows RC) |
| ビルドターゲット | `x86_64-pc-windows-msvc` |

---

## モジュール構成

```
src/
  main.rs             エントリポイント。GPUI App 初期化・ウィンドウ起動
  app.rs              open_window() 公開 API
  window.rs           ShogunWindow — メインビュー・タブ切替・ScrollHandle
  settings.rs         Settings TOML 読み書き (~/.config/shogun-desktop/settings.toml)
  ssh.rs              SshClient — ssh.exe ラッパー + ControlMaster 多重化
  ansi.rs             strip_ansi() — ANSI エスケープ除去
  theme.rs            GPUI カラーパレット定数

  tabs/
    shogun_tab.rs     将軍タブ — PTY terminal (shogun tmux session)
    agents_tab.rs     エージェントタブ — agent_status.sh SSH 実行・一覧表示
    dashboard_tab.rs  戦況タブ — dashboard.md SSH 取得・表示
    settings_tab.rs   設定タブ — SSH ホスト/鍵/セッション名 設定 UI
    terminal_tab.rs   共通 PTY ターミナル描画コンポーネント

  terminal/
    mod.rs            take_snapshot() — Term<VoidListener> → GridSnapshot 変換
    keys.rs           key_to_bytes() — gpui::Keystroke → PTY バイト列
    pty_session.rs    TerminalSession — portable-pty + alacritty_terminal 統合
    renderer.rs       coalesce_runs() — 隣接同色セルをテキストランに結合

assets/
  icon.svg            Kabuto シルエット SVG (ソース)
  icon.ico            マルチサイズ Windows アイコン (16/32/48/256px, PNG-in-ICO)

scripts/
  gen_icon_ico.mjs    PNG-in-ICO ジェネレーター (Node.js)

build.rs              Windows 実行ファイルへのアイコン埋め込み (winres)
CREDITS               サードパーティ帰属 (yohey-w kabuto icon, MIT)
```

---

## 主要コンポーネント設計

### SSH 接続レイヤー (`ssh.rs`)

libssh2 ではなく **システム `ssh.exe`** をサブプロセスとして起動する設計を採用。

**理由**: libssh2/WinCNG は Windows の OpenSSH 9.x との KEX ネゴシエーションに失敗するバグがある。
システム ssh.exe はアップデートと共に修正されるため互換性が高い。

**ControlMaster 多重化**: `ssh -M -S <socket>` でマスター接続を一度確立し、
以降の `exec()` 呼び出しは `-S <socket>` で既存セッションを再利用。
Win32-OpenSSH が ControlMaster ソケット共有をサポートしない場合 (`AtomicBool` で検出)、
毎回新規接続にフォールバックする。

```
SshClient::connect()
  └─ ssh -M -S <socket_path> -fN host   ← master プロセス起動

SshClient::exec(cmd)
  └─ ssh -S <socket_path> host cmd      ← 既存ソケット経由 (高速)
     (フォールバック時) ssh host cmd    ← 毎回新規接続
```

### PTY ターミナル (`terminal/`)

```
キー入力
  └─ key_to_bytes(Keystroke) → Vec<u8>
       └─ PtySession::write()

PTY 出力
  └─ pty_reader_thread
       └─ Processor::advance(byte) → Term<VoidListener> 更新
            └─ generation.fetch_add(1)  ← GPUI 再描画トリガー

GPUI 描画
  └─ take_snapshot(&Term) → GridSnapshot
       └─ coalesce_runs(cells) → Vec<TextRun>
            └─ GPUI text_element() でレンダリング
```

**16ms 更新ループ**: `TerminalSession` は PTY 読み取りスレッドで `generation` カウンタを
インクリメントし、GPUI の `cx.notify()` を介して再描画をトリガーする。

### アイコン埋め込み (`build.rs` + `winres`)

```rust
// build.rs — Windows ターゲット時のみ実行
if CARGO_CFG_TARGET_OS == "windows" {
    WindowsResource::new()
        .set_icon("assets/icon.ico")
        .compile()
}
```

`assets/icon.ico` は PNG-in-ICO 形式 (BMP 形式は Windows RC に拒否される)。
`scripts/gen_icon_ico.mjs` で Kabuto SVG パスデータから直接生成。

---

## データフロー

```
設定 TOML
  └─ settings.rs (load/save)
       └─ SshClient (connect)
            ├─ 将軍タブ: PTY ssh -t + alacritty_terminal
            ├─ エージェントタブ: agent_status.sh → SSH exec
            └─ 戦況タブ: cat dashboard.md → SSH exec
```

---

## ビルド

```powershell
# Windows PowerShell — WSL cargo は不可
cd C:\Users\aki\work\shogun-desktop
cargo build --release

# テスト (GPU 不要、純粋関数のみ)
cargo test
```

**注意**: `cargo test` はネットワーク・GPU 不要のユニットテストのみ。
実機 UI テストはビルド済み exe を手動起動して確認する。

---

## 今後の課題

| 項目 | 概要 |
|------|------|
| macOS ビルド | GPUI は Metal/macOS 対応。GitHub Actions macOS ランナーで CI ビルド予定 |
| CI/CD | push → Windows/macOS matrix build → リリース artifact 自動生成 |
| follow-mode | 手動スクロール中は auto-scroll を無効にするトグル |
| PTY リサイズ | ウィンドウリサイズ時に ConPTY の列/行数を更新 |
| パスワード保護 | 下記参照 |

### パスワード保護 (TODO)

現在、SSH パスワードは `settings.toml` に**平文で保存**される。
OSS 公開・macOS 対応のタイミングで OS キーストアへ移行する。

**方針**: `keyring` クレートを使い、Windows Credential Manager / macOS Keychain /
libsecret に委譲する。keyring が失敗した場合（環境未対応・デーモン不在など）は
`settings.toml` の平文にフォールバックする。

```
パスワード取得の優先順位:
  1. keyring::Entry::get_password()  ← OS キーストア (暗号化)
  2. settings.toml [ssh] password    ← 平文フォールバック
  3. (なし) → 接続時にプロンプト
```

設定 UI では保存方法を選択可能にする（Copilot トークン管理と同様の設計）:
- **キーストアに保存** (keyring が使える環境で推奨)
- **設定ファイルに保存** (平文、手動管理)
- **保存しない** (メモリ上のみ、起動ごとに入力)

**代替案**: パスワード認証を廃止し、秘密鍵 / ssh-agent 専用にする。
設定ファイルに秘密情報が残らないため最もシンプル。
