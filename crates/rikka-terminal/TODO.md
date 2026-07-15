# RikkaTerminal 課題一覧

完成済みの設計・経緯は `IPC.md`（IPC/タブ移送）と `README.md`/`packaging/README.md`
（既定ターミナル化）を正とする。ここは**残っている課題だけ**を書く。
（最終更新: 2026-07-13・conhost Reflow 移植完遂 `dbc90c8` 時点）

## タブ移送（P2 残・優先度順）

- [ ] **画像 store の搬送** — 移送すると kitty/sixel 画像は消える
      （placeholder セルは空白化済みで tofu は出ない）。wire にバイナリ添付
      か再転送プロトコルが要る。
- [ ] **OSC 8 ハイパーリンクの搬送** — 移送でリンクが普通のテキストになる。
      `replay_bytes` に OSC 8 を織り込めば済む（中規模）。
- [ ] **monarch 再選出 v2** — monarch 窓が閉じると新規 spawn/移送の調整が
      止まる（既存窓は無傷 = 設計どおりの縮退）。次の cold start までの
      空白を自動再選出で埋める。
- [ ] **窓単位 addressing** — 現状 `window_id` = pid 粒度。in-process detach
      （Ctrl+Shift+D）で作った同プロセス複数窓は移送先として個別指定不可
      （in-process merge で代用可）。
- [ ] **`-w <id>` CLI** — `rt -w <id>` は「未対応」エラーのまま
      （cli.rs）。spawn の既存窓ルーティング＋窓 socket の spawn 受理が要る。
- [ ] **Unix の tab move** — SCM_RIGHTS 版の handle 移送。tear-off /
      drag-merge も Windows 専用のまま。
- [ ] elevated handoff 窓プロセス（wire に flag のみ・実装なし）。

## resize / reflow（残りは意図的妥協のみ）

- [x] ~~conhost Reflow parity~~ — `dbc90c8` で完遂（実機ゲート
      `width_shrink_grow_keeps_conhost_agreement` 緑・実窓 E2E も PASS）。
- [ ] **alt screen 中の幅リサイズは no-reflow 妥協** — conhost の altbuffer
      reflow は未実測。フルスクリーンアプリは SIGWINCH 相当で再描画するため
      実害はほぼ無いが、厳密 parity なら実測 probe から。
- [ ] 移送 replay の可視行 WRAPLINE 非再現 — 見た目は同一。選択コピーの
      行結合だけが次の再描画まで異なる。

## UI / UX

- [ ] **「window not found」ログノイズ** — 窓 close 後に gpui 内部が dead
      handle を叩く ERROR ログ（実害なし）。出所特定して黙らせる。
- [x] ~~タブ strip の空白部への drop = 末尾移動~~ — 実装済（タブ上 drop は
      子が consume するため空白部のみ末尾移動・誤爆なし）。
- [ ] drag-merge の挿入位置 — 現状は移送先の末尾 adopt 固定。drop 位置の
      タブ間に挿入できると自然（cross-process は wire に挿入 index 追加）。
- [x] ~~ghost の掴み位置~~ — 非問題と判明: gpui が cursor_offset を自動で
      保存・適用しており ghost は掴んだ位置に追従する。
- [x] ~~設定ファイル~~ — 初版済: `[appearance] font / font_size /
      line_height / acrylic`・`[terminal] scrollback`・`[logging]`
      （%APPDATA%/rikka-terminal/config.toml・実窓検証済）。
      残: キーバインド設定・テーマ/配色。
- [x] ~~セッションロギング（Tera Term 流）~~ — Ctrl+Shift+L トグル＋タブ●
      表示＋`[logging]` config（auto_start / directory / log_input
      オプトイン）。出力は PTY 生バイト tee（src/session_log.rs）。
      実窓スモーク済（auto_start でログ生成・●表示・エスケープ列込み記録）。
- [ ] キーバインド設定（config.toml で chord の再割当）。
- [ ] フォント同梱（Cascadia 等）。

## 保守メモ

- 診断 probe（`#[ignore]`・`--nocapture` で実行）は残置:
  `width_semantics_probe` / `vertical_grow_probe` / `width_reflow_probe` /
  `conpty_resize_probe`。conhost の挙動疑義はまずこれで実測せよ。
- conhost spawn 系テストは `conhost_serial()` mutex で直列化必須
  （並列 cold start は deadline をいくら延ばしても飢える）。
- 背面窓の実機 E2E: `PostMessage(WM_CHAR/WM_KEYDOWN)` + `SetWindowPos` +
  `PrintWindow(PW_RENDERFULLCONTENT)` で前面を奪わず完結できる。
  WM_CHAR は制御文字が gpui 側で弾かれる — Enter 等は WM_KEYDOWN で送る。
  判定は窓を縦に広げて全景スクショで（viewport 見切れの誤診に注意）。

## 隣接（shogun-desktop 側）

- [x] ~~shogun-desktop の再ビルド+配備~~ — `e88cd3c` で完了（conhost
      Reflow + font_size/line_height 設定込み・rename 退避配備）。
- [ ] セッションロギングの shogun-desktop 露出 — tee はエンジン側実装
      なので `set_logging` を呼ぶ UI（トグル＋設定）を足すだけで載る。
