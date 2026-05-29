# aki-term 設計書

> 仮称。Windows Terminal (wt) の代替を目的とした、GPUI ベースの汎用ターミナルエミュレータ。

**ステータス**: 設計フェーズ  
**作成日**: 2026-05-28  
**ベース実装**: shogun-desktop の terminal/ クレート群

---

## 1. 背景・動機

Windows Terminal (wt) が特定環境で不安定なため、同等機能を持つ代替を自作する。
shogun-desktop で構築した以下のインフラをそのまま流用できる。

- `alacritty_terminal` による VT パーサー・グリッド管理
- `portable-pty` による PTY 管理・リサイズ
- GPUI による GPU レンダリング（bold / underline / cursor / 256色 実装済み）

描画性能の目標は **xterm.js（VS Code 組み込みターミナル）を上回ること**。  
Ghostty / wt レベルの GPU レンダラーは将来課題とし、v1 は GPUI の div ベース実装を採用する。

---

## 2. 要件

### 2.1 機能要件

| # | 要件 | 優先度 |
|---|------|--------|
| F-01 | 汎用タブ付きターミナル（動的追加・削除） | Must |
| F-02 | ローカルシェル起動（WSL / pwsh / cmd / カスタム） | Must |
| F-03 | GUI 設定画面（フォント・配色・シェル選択） | Must |
| F-04 | GPU レンダリング（GPUI ベース） | Must |
| F-05 | キーボード入力の PTY への直接転送 | Must |
| F-06 | ウィンドウリサイズ連動 PTY リサイズ | Must |
| F-07 | 256色 / TrueColor 対応 | Must |
| F-08 | 太字・下線・カーソル描画 | Must |
| F-09 | スクロールバック（マウスホイール・Shift+PgUp） | Must |
| F-10 | コピー・ペースト（Ctrl+Shift+C/V） | Must |
| F-11 | ワイド文字（全角・CJK）対応 | Must |
| F-12 | 組み込みカラースキーム複数種 | Should |
| F-13 | OSC 2 によるタブタイトル動的更新 | Should |
| F-14 | 複数ウィンドウ | Should |
| F-15 | フォントサイズ動的変更（Ctrl+= / Ctrl+-） | Should |

### 2.2 非機能要件

- **プラットフォーム**: Windows（WSL2 環境）を第一ターゲット。Linux は二次対応。
- **依存最小化**: Electron / .NET 不使用。Rust ネイティブバイナリ。
- **起動速度**: コールドスタート 1 秒以内。

### 2.3 v1 スコープ外（将来対応）

- スプリットペイン
- タブのドラッグ並び替え
- リガチャ（フォントシェーピング）
- マウスレポーティング（vim / tmux mouse mode）
- 画像プロトコル（Kitty / iTerm2 プロトコル）
- GPU アトラスレンダラー（Ghostty / wt 相当）

---

## 3. アーキテクチャ

### 3.1 クレート構成

```
aki-term/                          (workspace root)
  ├─ Cargo.toml
  └─ crates/
       ├─ aki-term-core/           ターミナルエンジン
       │    ├─ src/
       │    │    ├─ lib.rs
       │    │    ├─ session.rs     TerminalSession, PtyResizer trait
       │    │    ├─ pty.rs         spawn_local, spawn_shell (SSH)
       │    │    ├─ renderer.rs    Run, coalesce_runs, render_grid
       │    │    ├─ snapshot.rs    GridSnapshot, SnapshotCell, ResolvedColor
       │    │    └─ keys.rs        KeyDownEvent → PTY バイト変換
       │    └─ Cargo.toml
       └─ aki-term-app/            GPUI アプリ本体
            ├─ src/
            │    ├─ main.rs
            │    ├─ app.rs         ウィンドウ生成
            │    ├─ window.rs      AkiTermWindow (Render impl)
            │    ├─ tab.rs         Tab 構造体・タブ管理
            │    ├─ settings/
            │    │    ├─ mod.rs    Settings 構造体
            │    │    ├─ store.rs  TOML 読み書き
            │    │    └─ ui.rs     設定画面 render
            │    └─ shell.rs       ShellKind, シェル検出
            └─ Cargo.toml
```

#### shogun-desktop との関係

```
shogun-desktop/Cargo.toml
  └─ aki-term-core = { path = "../aki-term/crates/aki-term-core" }
```

shogun-desktop は `aki-term-core` を依存として参照し、terminal/ を削除する。

### 3.2 データフロー

```
シェルプロセス
  ↕ PTY (portable-pty)
TerminalSession
  ├─ reader thread: バイト列 → alacritty_terminal → GridSnapshot → generation++
  └─ writer: PTY へバイト送信（キー入力・リサイズ）

GPUI render thread (16ms ポーリング)
  ├─ generation 変化検知 → cx.notify()
  └─ GridSnapshot → coalesce_runs → Run → GPUI div ツリー → GPU
```

---

## 4. 主要データ構造

```rust
// ── アプリケーション状態 ─────────────────────────────────────────────────────

pub struct AkiTermWindow {
    tabs: Vec<Tab>,
    active_tab: usize,
    settings: Settings,
    settings_open: bool,
    terminal_cols: u16,
    terminal_rows: u16,
}

// ── タブ ─────────────────────────────────────────────────────────────────────

pub struct Tab {
    pub id: TabId,
    pub title: String,               // OSC 2 または ShellKind 由来
    pub session: TerminalSession,
    pub scroll_handle: ScrollHandle,
    pub scroll_locked: bool,
    pub prev_offset_y: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(u64);

// ── シェル ───────────────────────────────────────────────────────────────────

pub enum ShellKind {
    Wsl(String),      // ディストリビューション名（例: "Ubuntu"）
    Pwsh,             // PowerShell Core — pwsh.exe
    PowerShell,       // Windows PowerShell — powershell.exe
    Cmd,              // cmd.exe
    Custom(String),   // フルパス
}

impl ShellKind {
    /// portable-pty に渡す CommandBuilder を生成する
    pub fn build_command(&self, cwd: Option<&str>) -> CommandBuilder { ... }

    /// タブの初期タイトル
    pub fn default_title(&self) -> String { ... }
}

// ── 設定 ─────────────────────────────────────────────────────────────────────

pub struct Settings {
    pub default_shell: ShellKind,
    pub font_family: String,
    pub font_size: f32,
    pub color_scheme: ColorScheme,
    pub scrollback_lines: usize,
    pub key_bindings: KeyBindings,
}

pub struct ColorScheme {
    pub name: String,
    pub foreground: Rgba,
    pub background: Rgba,
    pub cursor: Rgba,
    /// ANSI 標準 16色（index 0–15）
    pub ansi: [Rgba; 16],
}

pub struct KeyBindings {
    pub new_tab: Keystroke,
    pub close_tab: Keystroke,
    pub next_tab: Keystroke,
    pub prev_tab: Keystroke,
    pub copy: Keystroke,
    pub paste: Keystroke,
    pub font_increase: Keystroke,
    pub font_decrease: Keystroke,
}
```

---

## 5. UI 設計

### 5.1 通常モード

```
┌──────────────────────────────────────────────┐
│ [Ubuntu ×] [pwsh ×] [+]            [⚙]      │  ← タブバー 32px
├──────────────────────────────────────────────┤
│                                              │
│                                              │
│              ターミナル本体                   │  flex_1
│                                              │
│                                              │
└──────────────────────────────────────────────┘
```

- キーボタン行・Send バー: **なし**（キーボード直接入力）
- ⚙ ボタンで設定オーバーレイを開く

### 5.2 設定オーバーレイ

```
┌──────────────────────────────────────────────┐
│ [Ubuntu ×] [pwsh ×] [+]            [⚙]      │
├──────────────────────────────────────────────┤
│ ╔════════════════════════════════════════╗   │
│ ║  設定                          [×]    ║   │
│ ║                                        ║   │
│ ║  デフォルトシェル                      ║   │
│ ║  ○ Ubuntu  ○ pwsh  ○ cmd  ○ カスタム ║   │
│ ║                                        ║   │
│ ║  フォント                              ║   │
│ ║  [MoralerspaceHW Neon    ] [13 ▲▼]   ║   │
│ ║                                        ║   │
│ ║  カラースキーム                        ║   │
│ ║  ○ Dark  ○ Light  ○ Solarized  ○ ... ║   │
│ ║                                        ║   │
│ ║  スクロールバック行数                  ║   │
│ ║  [10000                             ]  ║   │
│ ║                                        ║   │
│ ║              [保存]  [キャンセル]      ║   │
│ ╚════════════════════════════════════════╝   │
└──────────────────────────────────────────────┘
```

ターミナルの上に GPUI の `div` でオーバーレイ描画。別ウィンドウ不要。

---

## 6. シェル検出

起動時に利用可能なシェルを自動列挙してプルダウンに表示する。

```rust
pub fn detect_shells() -> Vec<ShellKind> {
    let mut shells = vec![];

    // WSL ディストリビューション
    // `wsl --list --quiet` の出力をパース
    if let Ok(output) = Command::new("wsl").args(["--list", "--quiet"]).output() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let name = line.trim().to_string();
            if !name.is_empty() {
                shells.push(ShellKind::Wsl(name));
            }
        }
    }

    // PowerShell Core
    if which("pwsh.exe").is_ok() {
        shells.push(ShellKind::Pwsh);
    }

    // Windows PowerShell（常にある）
    shells.push(ShellKind::PowerShell);

    // cmd（常にある）
    shells.push(ShellKind::Cmd);

    shells
}
```

---

## 7. 組み込みカラースキーム

| 名前 | 背景 | 前景 | 備考 |
|------|------|------|------|
| Dark | `#1e1e1e` | `#d4d4d4` | VS Code Dark+ 準拠 |
| Light | `#ffffff` | `#1e1e1e` | |
| Solarized Dark | `#002b36` | `#839496` | Ethan Schoonover |
| Dracula | `#282a36` | `#f8f8f2` | |
| One Dark | `#282c34` | `#abb2bf` | Atom 由来 |

カスタムスキームは設定ファイルに `[[color_scheme]]` セクションで追加可能。

---

## 8. キーバインド（デフォルト）

| 操作 | ショートカット |
|------|--------------|
| 新規タブ | `Ctrl+Shift+T` |
| タブを閉じる | `Ctrl+Shift+W` |
| 次のタブ | `Ctrl+Tab` |
| 前のタブ | `Ctrl+Shift+Tab` |
| タブ番号指定 | `Ctrl+1`〜`Ctrl+9` |
| コピー | `Ctrl+Shift+C` |
| ペースト | `Ctrl+Shift+V` |
| フォント拡大 | `Ctrl+=` |
| フォント縮小 | `Ctrl+-` |
| フォントリセット | `Ctrl+0` |
| スクロール上 | `Shift+PageUp` |
| スクロール下 | `Shift+PageDown` |
| 最下部へ | `End` |
| 設定を開く | `Ctrl+,` |

---

## 9. 設定ファイル仕様

パス: `%APPDATA%\aki-term\config.toml`（Windows）/ `~/.config/aki-term/config.toml`（Linux）

```toml
[general]
default_shell = "wsl:Ubuntu"   # "wsl:<distro>" | "pwsh" | "powershell" | "cmd" | "custom:<path>"
scrollback_lines = 10000

[font]
family = "MoralerspaceHW Neon"
size = 13.0

[color_scheme]
name = "Dark"    # 組み込み名 or カスタム定義の name

[key_bindings]
new_tab        = "ctrl+shift+t"
close_tab      = "ctrl+shift+w"
next_tab       = "ctrl+tab"
prev_tab       = "ctrl+shift+tab"
copy           = "ctrl+shift+c"
paste          = "ctrl+shift+v"
font_increase  = "ctrl+="
font_decrease  = "ctrl+-"

# カスタムカラースキーム（オプション）
[[custom_color_scheme]]
name       = "My Theme"
background = "#1a1a2e"
foreground = "#e0e0e0"
cursor     = "#ffffff"
ansi = [
  "#1e1e1e", "#cc0000", "#4e9a06", "#c4a000",  # 0–3
  "#3465a4", "#75507b", "#06989a", "#d3d7cf",  # 4–7
  "#555753", "#ef2929", "#8ae234", "#fce94f",  # 8–11
  "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",  # 12–15
]
```

---

## 10. 主要依存クレート

| クレート | 用途 |
|---------|------|
| `gpui` | UI フレームワーク・GPU レンダリング |
| `gpui-component` | ボタン・入力・レイアウト部品 |
| `alacritty_terminal` | VT パーサー・グリッド管理 |
| `portable-pty` | PTY 生成・プロセス管理 |
| `parking_lot` | FairMutex（スレッド飢餓防止） |
| `unicode-width` | ワイド文字幅計算 |
| `serde` + `toml` | 設定ファイル読み書き |
| `dirs` | OS 標準設定パス取得 |
| `anyhow` | エラーハンドリング |

---

## 11. 将来の GPU レンダラー移行方針

現状の div ベースレンダラーは xterm.js を上回る性能を持つため v1 では採用しない。
将来の移行候補：

| 方式 | 条件 | 備考 |
|------|------|------|
| GPUI Canvas + 自前グリフアトラス | いつでも可 | 実装規模大（Ghostty 相当） |
| libghostty（Vulkan バックエンド） | Windows 対応 + Vulkan 安定後 | GL 混在を避けるため Vulkan 必須 |

レンダラーは `aki-term-core` 内で trait として抽象化し、差し替え可能にしておく。

```rust
pub trait TermRenderer: Send {
    fn render(&self, snap: &GridSnapshot) -> impl IntoElement;
}
```

---

## 12. マイルストーン

| フェーズ | 内容 |
|---------|------|
| **Phase 0** | `aki-term-core` クレート作成（shogun-desktop から terminal/ を移植） |
| **Phase 1** | ローカルシェル起動（`spawn_local`）・単一タブで動作確認 |
| **Phase 2** | 動的タブ（追加・削除・切り替え）|
| **Phase 3** | GUI 設定画面・カラースキーム・設定ファイル |
| **Phase 4** | フォントサイズ動的変更・コピペ・スクロールバック |
| **Phase 5** | OSC 2 タイトル更新・複数ウィンドウ |
| **Future** | GPU アトラスレンダラー |

---

## 未決定事項

| # | 問題 | 候補 |
|---|------|------|
| U-01 | タイトルバーのスタイル（OS ネイティブ vs カスタム） | v1 は OS デフォルト |
| U-02 | アプリアイコン | — |
| U-03 | インストーラー（MSI / NSIS / ポータブル exe） | — |
| U-04 | 自動更新機能 | v1 スコープ外 |
