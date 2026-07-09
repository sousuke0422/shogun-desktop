# RikkaTerminal

rikka-terminal-core を核にしたスタンドアロンのターミナルエミュレータ。
shogun-desktop（SSH 前提のエージェント操作卓）から抽出したエンジンを、
「素のローカルターミナル」として成立させるプロダクト側の殻。

## 設計

```
┌──────────────────────────────────────────────┐
│ rikka-terminal (このクレート = プロダクト殻)     │
│   ローカル PTY (ConPTY / forkpty)・窓・タブ計画  │
├──────────────────────────────────────────────┤
│ rikka-terminal-core (エンジン)                 │
│   alacritty_terminal ラッパ + gpui レンダラ     │
│   session / keys / selection / IME / 描画      │
│   kitty gfx・Sixel・OSC 8/9/52・?2026・?1004…  │
├───────────────┬──────────────────────────────┤
│ -ssh-integration │ -agent-integration │ -gpui-ime │  (衛星: 任意)
└───────────────┴──────────────────────────────┘
```

- **エンジンは transport 非依存**: `build_terminal_session` は任意の
  Read/Write 対 + resizer を取る。shogun-desktop は SSH チャネルを差し、
  本クレートはローカル ConPTY を差すだけ。プロトコル実装
  （マウス3エンコーディング・カーソル形状/blink・色クエリ・寸法報告・
  kitty graphics・選択のグリッド追従…）は全て core 側で済んでいる。
- **依存方向は一方通行**: 殻 → core。core はこの殻を知らない。
- **identity**: TERM_PROGRAM / XTVERSION は "rikka-terminal"（プロダクト名）。

## プロトタイプ範囲（現状）

- **タブ**: Ctrl+Shift+T 新規 / W 閉じる / D 新窓へ分離 / A 全窓統合、
  Ctrl+PageUp/PageDown（届く環境では Ctrl+Tab も）で循環、クリックで切替。
  タブ=窓非依存セッション（hub.rs）: 分離結合は UI スレッド上の同期 Vec 移動で、
  PTY/parse スレッドは移送を知らない — wt がクラッシュする「ライブコントロールの
  窓間移送」という失敗クラスが構造的に存在しない。ガチャ耐性は
  `e2e/rikka-tabs-stress.ps1`（分離結合5連打+生存タイプ）で回帰固定。
  既知: gpui-Windows は Ctrl+M を配達しない（^M=CR 遺産）ため merge は A
- 1 窓 1 ペイン×タブ。pwsh.exe（無ければ powershell/cmd）を ConPTY で起動
- **TSF 常時 ON**: 本プロダクトが rikka-terminal-gpui-ime の soak 車両
  （shogun-desktop は SHOGUN_TSF ゲート維持）。タスクバー あ/A 追従・
  preedit インライン・候補ウィンドウはカーソル位置・確定→PTY。
  e2e=`rikka-tsf-typing.ps1`（preedit/候補リスト/確定の3段スクショ）
- エンジン直結: 描画・スクロールバック・選択+コピー（e2e=`rikka-select-copy.ps1`）・IME・
  キー（kitty keyboard 含む）・ホイール（レポーティング/alt-scroll/履歴）・
  OSC タイトル → 窓タイトル
- コピー: Ctrl+Shift+C / Ctrl+Insert、ペースト: Ctrl+Shift+V / Shift+Insert、
  右クリックメニュー（コピー/ペースト）— **非スクロールの pane に取り付け**
  （ContextMenu は開くと窓サイズ absolute 子を注入するため、スクロール
  コンテナに付けると grid が画面外へ吹き飛ぶ: shogun-desktop で実証済みの罠）
- Shift+PageUp/PageDown で履歴ページング
- フォントは Consolas（システム解決・CJK は DirectWrite fallback 任せ）
- **UI の方向性 = Files (files.community) 系のソフト Fluent**: 低コントラストの
  レイヤ面（白 8〜12% オーバーレイ）・角丸ピルタブ＋hover・アクセントは
  アクティブタブ下の 2px バーのみ・ヘアライン境界。
- **タブはタイトルバー統合済み**（appears_transparent でネイティブバーを消し、
  タブ列がタイトルバーを兼ねる）: 空き領域が `WindowControlArea::Drag`
  （HTCAPTION = ドラッグ・ダブルクリック最大化・スナップ・システムメニューが
  ネイティブ）、min/max/close は Segoe MDL2 Assets グリフの自前ボタン
  （`window_control_area` hitbox → gpui の NC ハンドラがネイティブ動作を実行、
  click リスナー無し）。Drag はタブの親でなく **兄弟**に張る — NC hit-test は
  点下の全 hitbox を見るため、親に張るとタブクリックが HTCAPTION に食われる。
  最大化時のフレーム食い込みは gpui の NCCALCSIZE 側で補正済み。
- **アクリル対応済み（opt-in）**: `RIKKA_ACRYLIC=1` で gpui の
  `WindowBackgroundAppearance::Blurred`（= SetWindowCompositionAttribute
  ACCENT_ENABLE_ACRYLICBLURBEHIND・Win10 1809+）を有効化し、chrome と
  ペイン面が 72〜78% ティントの半透明になる（Win10 ESU 実機で透過確認済み）。
  既定 OFF の理由 = この Win10 世代 API はウィンドウドラッグ時の遅延が既知
  （wt はドラッグ中だけアクリルを切る対策をしている）。採否と既定値は
  設定ファイル（P1）に吸収する。
- UI TODO: mica バックドロップ（Win11 専用 — 実機が来たら DWMWA_SYSTEMBACKDROP_TYPE で）・
  chrome 書体の Segoe UI Variable 化・システムアクセント色追従・
  タブ close の hover-reveal（WinUI CloseButtonOverlayMode=Auto 相当）

## wt 互換 CLI（`rt`）

`rt.exe` は**薄いランチャー**（隣の `rikka-terminal.exe` へ argv を横流しして
即終了・パースは本体側 `src/cli.rs`）。Windows Terminal の
`wt` コマンドライン文法のサブセットを実装する（parser: `src/cli.rs`・ユニット
テスト付き。エラーと `--help` は GUI サブシステムのためメッセージボックス表示）。

| wt 構文 | 状態 |
|---|---|
| `-M/--maximized`・`-F/--fullscreen`・`--pos x,y`・`--size c,r` | ✔ |
| `new-tab`/`nt`: `-d`・`-p`（シェル名）・`--title`・裸の command line | ✔ |
| `;` による複数コマンド連結（1 窓に複数タブ） | ✔ |
| `--tabColor`/`--colorScheme`/`--suppressApplicationTitle` | 受理して無視（設定基盤待ち） |
| `-w/--window`（既存窓へのルーティング） | **TODO**: 単一インスタンス IPC が必要 |
| `split-pane`/`sp`・`focus-tab`・`move-focus`・`move-pane` | **TODO**: ペイン分割の実装後 |

さらに **Linux 系ターミナルの共通引数**（P3 Linux 移植への布石）も本体が受ける:
`-e <cmd>…` / `-- <cmd>…`（以降全部コマンド）・`--working-directory`・`-t/-T/--title`・
`--geometry CxR[+X+Y]`・`--maximize`/`--full-screen`・`--hold`/`--class`/`--name`（受理）・
`-v/--version`。`rt <dir>` は code 流にそのディレクトリでシェルを開く（1個1タブ・rt 拡張）。

例: `rt --pos 150,150 --size 100,30 nt -d C:\work --title 作業 ; nt -p cmd ; ping localhost`
例: `rt -e ssh anchor` ・ `rt --geometry 120x40+100+100 -- btop` ・ `rt . C:\work`

## 非目標（プロトタイプでは持たない）

- 分割・設定ファイル・フォント同梱・検索・タブDnD（ロードマップ P1 残）
- SSH（それは shogun-desktop の仕事）
- シェル終了後の再起動 UI（グリッドが凍るだけ）

## ロードマップ

- P0: 本プロトタイプ（起動して普段使いの smoke が通る）✔
- P1: タブ ✔（分離結合込み）・残り=設定・フォント同梱・シェル選択・タブDnD
- P2: リポ切り出し（vendored gpui / alacritty_terminal パッチの扱いと同時に）
- P3: Linux (forkpty) / macOS
