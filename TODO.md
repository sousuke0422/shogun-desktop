# TODO / 未対応案件

2026-07-03 時点。直近の実装状況は git log、経緯の詳細は multi-agent-shogun 側
`memory/project_shogun_desktop_terminal_2026_07_02.md` を参照。

## 1. 実機確認待ち（コード済み・検証未了）

ビルド: `cargo build --release`（Windows cargo.exe）→ 再起動して確認。

- [ ] **ホイール方向の符号** — gpui wheel-up=正 前提で直結マッピング。逆なら
      `shell_window.rs` の `on_scroll_wheel` で `lines` を符号反転するだけ
- [x] ssh 系ペインの微スクロール — **2026-07-04 根治・実機確認済み**。真因は
      IME/選択 overlay canvas (absolute+size_full) 自体が overflow 源（taffy は
      absolute 子もコンテンツサイズに算入→padding 8px 分が常時スクロール可能・
      上端欠け・下隙間も同根）。bce57db で overlay を content box に inset
- [ ] アイドル時 CPU がほぼ 0%（16ms ポーリング全廃・イベント駆動化）
- [ ] shell window: 履歴スクロール／ステータスバー「履歴 N行上」／キー入力で最下部復帰
- [ ] shell window: Shift+PageUp/PageDown ページング
- [ ] btop でホイールがアプリ側スクロールになるか（マウスレポーティング転送）
- [ ] less / man でホイールが効くか（alternate scroll → 矢印変換）
- [ ] 絵文字が Twemoji 絵柄で出るか・セル幅 2 に収まるか（`echo 😀🍣🚀`）—
      2026-07-04 実機で Segoe 落ちを確認 → 真因 = gpui が fallback を system collection
      でしか解決しない → 0469cd8 で起動時 AddFontResourceExW 登録。要再検証（UI/グリッド両方）。
      gpui 上流 PR ネタ: generate_font_fallbacks は custom collection も探すべき
- [ ] Ctrl+Shift+V ペースト（claude code へ複数行貼り→ 1行ずつ実行されないこと）
- [ ] shell window の選択コピー・IME（前回実装分の目視）
- [ ] tailscale アイドル後の入力引っかかり解消（keepalive + 非同期書込）
- [x] e2e/ スクリプト再実行 — **2026-07-04 PASS**（drag-copy + scan-highlight とも）。
      `pwsh.exe -NoProfile -File` で呼ぶ（powershell.exe 5.1 は PSModulePath 汚染で不可）。
      注意: この機では常駐アプリが Ctrl+Shift+C をグローバルホットキー占有しており
      アプリに届かない → コピーは Ctrl+Insert（1790008 で正式バインド）。犯人特定は未了

## 2. 未実装 — ターミナル機能（優先順）

- [ ] **kitty keyboard protocol のエンコード** — vendored alacritty はモード追跡済み、
      `keys.rs` が非対応。opencode（bubbletea v2）等が要求した場合キー挙動に影響
- [ ] **ssh/tmux ペインの履歴スクロール** — tmux は alternate screen 故サーバ側履歴。
      tmux `mouse on` なら `wheel_pty_bytes` を本窓ペインに配線するだけで動く見込み。
      copy-mode 中継 / capture-pane 方式はその後
- [ ] マウス click/drag のレポーティング転送（現状ホイールのみ。btop のクリック操作等）
- [ ] OSC 8 ハイパーリンクのクリック（パース済み・snapshot が捨てている）
- [ ] OSC 0/2 ウィンドウタイトル反映（実害小）
- [ ] OSC 9 / 777 デスクトップ通知 — エージェント完了通知に転用できる可能性
- [ ] 選択中の自動スクロール抑止（出力が流れるとハイライトが内容とずれる）
- [ ] リサイズ時のリフロー
- [ ] 検索・設定ファイル・タブ/分割（「本物のターミナル」級の将来項目）
- [ ] terminal_tab.rs の旧 scroll-lock ロジック整理 — 符号が逆疑い＋padding 修正後は
      ほぼ死にコード

## 3. 高リフレッシュ・ヌルヌル構想（殿表明・順序が肝）

段階目標（殿裁定 2026-07-03: いきなり 200Hz 級は狙わない）:
**第1段 = 120fps（予算 8.3ms）で「ヌルヌル」を成立させる**。
200Hz 台への追従は第2段で、vsync 連動にしておけば自然に伸びる余地として残す。
Air は 60Hz（ProMotion なし）→ 省電力側の検証機。

1. [x] スクロールバック（shell window 分は済み）
2. [ ] ピクセル単位スクロール補間（現状セル単位ジャンプ）
3. [ ] 行 shape 結果のキャッシュ＋dirty row 差分描画
      （スクロール中は行内容不変＝全ヒットで最軽量、という好条件あり）
4. [ ] コアレス 16ms→8ms（第1段・最小変更）→ 余裕が出たら vsync 駆動（第2段）

## 4. リリース / インフラ（殿の作業を含む）

- [x] **push**: 2026-07-04 完了（Windows git で `--force-with-lease`、61c2b38）。
      匿名化履歴（`Users\aki`→`Users\dev`）が origin に反映済み。
      過去メモの旧ハッシュ参照は 2026-07-04 に新ハッシュへ付替済み
- [ ] **shogun-suite を GitHub に新規作成して push**（remote 未設定のローカル repo。
      public 推奨 = CI が素の github.token で checkout 可。private なら
      shogun-desktop 側に `SUITE_CHECKOUT_TOKEN` secret 登録）
- [ ] CI 緑化確認 → `v0.1.0` タグ → GitHub Release（macOS .app zip + Windows exe 恒久添付）
- [ ] **MacBook Air 展開**: Release から zip 取得・`xattr -dr com.apple.quarantine`・
      実機検証（IME/ことえり・cmd 系キー・絵文字 COLRv0・全機能）。参照第一候補は
      ghostty（MIT）。Zed/cosmic-term ソースは最終手段（GPL 回避）
- [ ] mac 版 E2E（osascript/CGEvent）— 必要になってから
- [ ] **Linux 対応時**: gpui 0.2.2 は Linux で FontFallbacks 無視＋emoji 判定が
      NotoColorEmoji 固定 → 上流 PR が本命。Noto に落ちる分には殿許容済み
