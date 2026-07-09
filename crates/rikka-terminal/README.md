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

- 1 窓 1 ペイン。pwsh.exe（無ければ cmd.exe）を ConPTY で起動
- エンジン直結: 描画・スクロールバック・選択+コピー・IME・
  キー（kitty keyboard 含む）・ホイール（レポーティング/alt-scroll/履歴）・
  OSC タイトル → 窓タイトル
- コピー: Ctrl+Shift+C / Ctrl+Insert、ペースト: Ctrl+Shift+V / Shift+Insert
- Shift+PageUp/PageDown で履歴ページング
- フォントは Consolas（システム解決・CJK は DirectWrite fallback 任せ）

## 非目標（プロトタイプでは持たない）

- タブ・分割・設定ファイル・フォント同梱・検索（ロードマップ P1）
- SSH（それは shogun-desktop の仕事）
- シェル終了後の再起動 UI（グリッドが凍るだけ）

## ロードマップ

- P0: 本プロトタイプ（起動して普段使いの smoke が通る）✔
- P1: タブ（旧 aki-term 構想の吸収）・設定・フォント同梱・シェル選択
- P2: リポ切り出し（vendored gpui / alacritty_terminal パッチの扱いと同時に）
- P3: Linux (forkpty) / macOS
