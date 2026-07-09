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
- エンジン直結: 描画・スクロールバック・選択+コピー・IME・
  キー（kitty keyboard 含む）・ホイール（レポーティング/alt-scroll/履歴）・
  OSC タイトル → 窓タイトル
- コピー: Ctrl+Shift+C / Ctrl+Insert、ペースト: Ctrl+Shift+V / Shift+Insert
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

## 非目標（プロトタイプでは持たない）

- 分割・設定ファイル・フォント同梱・検索・タブDnD（ロードマップ P1 残）
- SSH（それは shogun-desktop の仕事）
- シェル終了後の再起動 UI（グリッドが凍るだけ）

## ロードマップ

- P0: 本プロトタイプ（起動して普段使いの smoke が通る）✔
- P1: タブ ✔（分離結合込み）・残り=設定・フォント同梱・シェル選択・タブDnD
- P2: リポ切り出し（vendored gpui / alacritty_terminal パッチの扱いと同時に）
- P3: Linux (forkpty) / macOS
