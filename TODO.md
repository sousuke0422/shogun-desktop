# TODO / 未対応案件

2026-07-03 時点。直近の実装状況は git log、経緯の詳細は multi-agent-shogun 側
`memory/project_shogun_desktop_terminal_2026_07_02.md` を参照。

## 1. 実機確認待ち（コード済み・検証未了）

ビルド: `cargo build --release`（Windows cargo.exe）→ 再起動して確認。

- [ ] **ホイール方向の符号** — gpui wheel-up=正 前提で直結マッピング。逆なら
      `shell_window.rs` の `on_scroll_wheel` で `lines` を符号反転するだけ
- [ ] ssh 系ペインの微スクロールが消えたか（pane padding 勘定漏れ修正）
- [ ] アイドル時 CPU がほぼ 0%（16ms ポーリング全廃・イベント駆動化）
- [ ] shell window: 履歴スクロール／ステータスバー「履歴 N行上」／キー入力で最下部復帰
- [ ] shell window: Shift+PageUp/PageDown ページング
- [ ] btop でホイールがアプリ側スクロールになるか（マウスレポーティング転送）
- [ ] less / man でホイールが効くか（alternate scroll → 矢印変換）
- [ ] 絵文字が Twemoji 絵柄で出るか・セル幅 2 に収まるか（`echo 😀🍣🚀`）
- [ ] Ctrl+Shift+V ペースト（claude code へ複数行貼り→ 1行ずつ実行されないこと）
- [ ] shell window の選択コピー・IME（前回実装分の目視）
- [ ] tailscale アイドル後の入力引っかかり解消（keepalive + 非同期書込）
- [ ] e2e/ スクリプト再実行（PowerShell 実行ポリシーの都合で自動実行不可、手動で）

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

目標はモニタ追従（vsync 連動）。殿の Windows 実機は 200Hz 台 → フレーム予算
~4-5ms、Air は 60Hz（ProMotion なし）→ 省電力側の検証機。固定 8ms タイマー案は
240Hz を半分捨てるので不採用、vsync 連動一択。

1. [x] スクロールバック（shell window 分は済み）
2. [ ] ピクセル単位スクロール補間（現状セル単位ジャンプ）
3. [ ] 行 shape 結果のキャッシュ＋dirty row 差分描画 — 4-5ms 予算では**必須**
      （スクロール中は行内容不変＝全ヒットで最軽量、という好条件あり）
4. [ ] コアレスタイマー廃止 → vsync 駆動（`start_terminal_refresh` の置換）

## 4. リリース / インフラ（殿の作業を含む）

- [ ] **push**: master が origin より 16 コミット先行（殿実行、workflow scope 必要 —
      ci.yml 変更を含む）
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
