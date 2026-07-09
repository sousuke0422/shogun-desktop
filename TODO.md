# TODO / 未対応案件

2026-07-03 時点。直近の実装状況は git log、経緯の詳細は multi-agent-shogun 側
`memory/project_shogun_desktop_terminal_2026_07_02.md` を参照。

## 1. 実機確認バックログ

ビルド: `cargo build --release`（Windows cargo.exe）→ 再起動して確認。

### 1.1 Windows

#### 1.1.1 ホイール方向の符号

- トリガー: shell window でマウスホイール上/下を送る
- 期待結果: 上=上方向スクロール、下=下方向スクロール（gpui wheel-up=正 前提の直結マッピング。逆なら `shell_window.rs` の `on_scroll_wheel` で `lines` を符号反転）
- 対象窓: shell window
- 対象OS: Windows

#### 1.1.2 shell window 履歴スクロール

- トリガー: shell window でマウスホイール上を送って履歴行を表示 → ステータスバーに「履歴 N行上」が出ることを確認 → 任意のキー入力
- 期待結果: (1)履歴行が正しく描画される（7bf34c4 で display_iter グリッド座標の usize キャストバグは根治済）(2)ステータスバーに「履歴 N行上」が表示 (3)キー入力で最下部に復帰
- 対象窓: shell window
- 対象OS: Windows
- 備考: ホイール符号・alacritty履歴側は計装実測で最初から正常だった

#### 1.1.3 shell window Shift+PageUp/PageDown ページング

- トリガー: shell window にフォーカス → Shift+PageUp / Shift+PageDown
- 期待結果: 1ページ分の上下スクロール
- 対象窓: shell window
- 対象OS: Windows

#### 1.1.4 Tailscale アイドル後の入力引っかかり

- トリガー: Tailscale 経由で SSH 接続 → 数分間_idle_放置 → typing 再開
- 期待結果: 入力が引っかからずスムーズに流れる（keepalive + 非同期書込で修正済み）
- 対象窓: shell window（Tailscale 経由 SSH）
- 対象OS: Windows

### 1.2 macOS

#### 1.2.1 デスクトップ通知配信（MBA 実機）

- トリガー: ターミナルで `printf '\e]9;test notification\a'`（アプリが非フォーカス状態）
- 期待結果: macOS の通知センターに「将軍デスクトップ」名義のトーストが表示
- 対象窓: 全ターミナルタブ
- 対象OS: macOS（MacBook Air）
- 備考: mac-notification-sys 0.6.15 で実装済み。`set_application("app.rikkalab.shogun-desktop")`。focus suppression（active タブでは出ない）も実装済み

### 1.3 検証済み疑い（要ダブルチェック）

git log（2026-07-03 以降）および TODO 記述を突合した結果、「検証済み疑い」に該当する項目は
無し。上記 5 件はいずれも git log に検証を示すコミット無く、純粋な「要検証」
（OSC 9;4 進捗表示は 2026-07-08 実機確認済みのため 1.9 へ移動）。

---

### 1.9 検証済み（完了記録）

- [x] **OSC 9;4 進捗表示 — 2026-07-08 実機確認済み（殿確認）**。
      `printf '\e]9;4;1;50\a'` でタブ下端 + shell window ステータスバー下端に 3px
      バーが表示。実装 2026-07-04（PTY 受動スキャナ→虹色スクロールバー）＋2026-07-08
      に parser の空 percent field 修正・tmux 内で OSC 9;4 を出さず title spinner
      （Braille）を出す agent 向けの title-spinner→不定バー fallback を追加
- [x] ssh 系ペインの微スクロール — **2026-07-04 根治・実機確認済み**。真因は
      IME/選択 overlay canvas (absolute+size_full) 自体が overflow 源（taffy は
      absolute 子もコンテンツサイズに算入→padding 8px 分が常時スクロール可能・
      上端欠け・下隙間も同根）。bce57db で overlay を content box に inset
- [x] アイドル時 CPU がほぼ 0%（16ms ポーリング全廃・イベント駆動化）
      — **2026-07-05 実機確認済み**（殿計測: アイドル時 ≤0.5%）
- [x] btop でホイールがアプリ側スクロールになるか（マウスレポーティング転送）
      — **2026-07-05 実機確認済み**（殿確認）
- [x] less / man でホイールが効くか（alternate scroll → 矢印変換）
      — **2026-07-05 実機確認済み**（殿確認）
- [x] 絵文字 Twemoji 統一 — **2026-07-04 実機確認済み**。gpui バグ 2 連: ①fallback を
      system collection でしか解決しない（44b69fd: custom 検索 + AddMapping へ collection
      明示）②raster_bounds がベースグリフのみ解析 → COLR ベース空の Twemoji が 0×0 で
      消滅（6f5f9ae: COLR レイヤー bounds の union）。vendored gpui (crates/gpui) に実装。
      gpui 上流 PR ネタ×3: 上記2件 + Linux FontFallbacks 無視。診断は src/bin/fontprobe.rs。
      **上流起票用の下書きは本 repo issue #2 / #3 / #4 に格納済み**（zed へは殿が自分の
      言葉で起票し、下書きは AI 生成と開示して引用添付 — Zed AI Policy 準拠の形式）
- [x] Ctrl+Shift+V ペースト（claude code へ複数行貼り→ 1行ずつ実行されないこと）
      — **2026-07-05 実機確認済み**（殿確認）
- [x] shell window の選択コピー・IME（前回実装分の目視）
      — **2026-07-05 実機確認済み**（殿確認）
- [x] **リサイズでグリッドが追従しない（殿報告 2026-07-05）** — 根治済み。真因は
      bce57db の overlay content-box 化の副作用: スクロールコンテナ内の absolute 子は
      taffy が **コンテンツサイズ**に合わせるため、overlay 高さ = グリッド行数×セル高で
      固定 → 行数計算が overlay 高さ依存 → **循環参照で行数がspawn時から不変**
      （幅は追従、高さのみ死亡）。修正: overlay canvas をスクロールコンテナ外の
      relative ラッパー兄弟へ移設（**shell window のみ**）。offset は常時0のため選択/IME
      座標系は不変。計装並行インスタンス+MoveWindow 自動試験で 33→21→41行の追従を確認、
      shift-drag e2e で選択ハイライトも PASS。
      **本窓（将軍/家老陣タブ）への同修正は表示崩れのため revert**（殿裁定 2026-07-05。
      本窓は shell と違い外側に overflow_hidden ラッパーが無く、relative ラッパーが
      min-height:auto でコンテンツ高さに膨らんだ疑い）。
      **本窓は触る前からリサイズ正常（殿確認）** — 同じ pane_measured 構造なのに
      shell だけ壊れた理由は未解明（タブ chrome の flex 構成差か、grid content と
      viewport の大小関係か）。本窓構造には触れないこと。再発時のみ調査
- [x] e2e/ スクリプト再実行 — **2026-07-04 PASS**（drag-copy + scan-highlight とも）。
      **2026-07-05 注意**: マウスレポーティング転送実装後は素のドラッグはアプリ側へ
      転送され青ハイライトが出ない → `-ShiftDrag` オプション（ローカル選択バイパス）で
      実行すること。素のドラッグ+scan-highlight FAIL は仕様であり回帰ではない。
      `pwsh.exe -NoProfile -File` で呼ぶ（powershell.exe 5.1 は PSModulePath 汚染で不可）。
      注意: この機では常駐アプリが Ctrl+Shift+C をグローバルホットキー占有しており
      アプリに届かない → コピーは Ctrl+Insert（1790008 で正式バインド）。犯人特定は未了

## 2. 未実装 — ターミナル機能（優先順）

- [x] **kitty keyboard protocol のエンコード** — 2026-07-04 実装。Config::kitty_keyboard
      有効化（push/pop/CSI ? u 応答は alacritty、返信は PtyWrite 経由）＋ keys.rs が
      TermMode でエンコード切替。flag 1 (disambiguate)・8 (all keys as esc) 対応、
      flag 2 (event types) は key-up/repeat 未送出の縮退動作、4/16 未対応（省略合法）。
      ついでにレガシー側も拡充: F1-F12・Insert・修飾付き矢印/Home/End/PgUp/PgDn
      (CSI 1;m X / n;m ~)・Alt=ESC prefix。実機確認: `kitten show-key -m kitty` 相当か
      claude code / opencode で矢印・Esc・ctrl+英字の挙動
- [x] マウスレポーティングの残ギャップ — **2026-07-09 実装分**: 水平ホイール
      (btn 66/67、正=左=gpui 符号準拠、両窓に hwheel_accum 配線・alt-scroll
      相当なし=仕様) と **?1005 UTF-8**（座標上限 223→2015、SGR>UTF-8>X10 の
      優先順、release は非 SGR で btn3 のまま）。回帰テスト 3 本（DECSET を実
      parser 経由で立てて検証）。**意図的スキップ**: ?1015 urxvt・?1016
      SGR-pixels は vte 0.13.1 が NamedPrivateMode を持たず DECSET が TermMode
      に届かない（対応には vte の vendoring/バンプが必要 — レガシー/ニッチ
      encoding に見合わず棚上げ）。shift-capture (kitty 系) は shift=ローカル
      選択バイパスの UX・e2e 規約と衝突するため不採用
- [x] **ssh/tmux ペインの履歴スクロール** — **2026-07-06 実装（実機確認待ち）**。
      `ShogunWindow::wheel_to_pty_for_pane` が本窓ペインの wheel を shell window と
      同じ規則で PTY へ転送（tmux `mouse on`=mouse reporting / alternate scroll。
      tmux がサーバ側履歴＝copy-mode でスクロール）。座標は overlay canvas が書き戻す
      pane_origin で window座標→セル変換（tmux はカーソル下のペインを座標で選ぶ）。
      小数 wheel 断片は accum 保持しローカル/リモート二重スクロールを防止。
      PTY が取らない時のみ従来の autoscroll-lock ロジックへ落ちる
- [x] **マウス click/drag のレポーティング転送** — 2026-07-04 実装。動機: claude code
      2.1.187 で select menu がクリック対応・2.1.178 で statusline リンクもクリック化。
      `mouse_pty_bytes`（?1000 click / ?1002 drag / ?1003 hover、SGR/X10 両対応、
      release は SGR=ボタン保持+'m'・X10=btn3、mods は alt+8/ctrl+16）＋
      selection.rs のリスナーがレポーティング優先で分岐（shift＝ローカル選択バイパス、
      右クリックはローカルメニュー温存 = WT 同様、press/release は mode 途中切替でも対で送る）。
      **2026-07-05 実機確認済み**（tmux ペイン選択・ドラッグ転送とも動作。合成入力で
      座標の完全一致を検証）。付随事故: 「横ドラッグで選択が縦にずれる」の真因は
      本アプリではなく tmux — マウス掴みアプリ(opencode ?1003)のペイン内ドラッグが
      境界セルを踏むと既定 MouseDrag1Border → resize-pane -M が発火し境界が追随、
      リフローで選択が縦に動く。対処は ~/.tmux.conf の `unbind -n MouseDrag1Border`
      （WSL 側・注入実験で再現と修正効果を確認済み）
- [x] **OSC 9;4 進捗表示** — 2026-07-04 実装。PTY reader の受動スキャナ
      (`terminal/progress.rs`、vte は 9;4 を捨てるため素通し監視) → タブ下端 +
      shell window ステータスバー下端に 3px バー（通常=虹色スクロール・ゲーミング仕様/
      エラー=紅/警告=金箔/不定=全幅虹色。with_animation なので表示中のみフレーム駆動）。
      **2026-07-08 実機確認済み（殿確認）**。tmux 内で OSC 9;4 を出さず title spinner
      を出す agent 向けに、title-spinner→不定バー fallback も追加
  - [x] **Phase 2: Windows タスクバー進捗 — 2026-07-09 実装＋自律 e2e 検証済**
        （`src/taskbar_progress.rs`）。ITaskbarList3::SetProgressState/Value、
        HWND は EnumWindows＋PID＋タイトル完全一致（gpui 無改変）・title 毎
        キャッシュ＋dedup。本窓＝両セッションの aggregate（Error>Normal(max%)>
        Warning>Indeterminate）・シェル窓＝自セッション。
        `e2e/taskbar-progress-test.ps1`: 50%＝ボタン半分緑・不定＝marquee・
        clear 復帰をスクショ確認。**エージェント稼働中（title spinner）に本窓
        ボタンが常時 marquee になるのも実機で確認**＝本旨成立。
        既知: Win10 のボタン結合で 2 窓が 1 ボタンに畳まれると表示は混合される
- [x] **OSC 8 ハイパーリンク（2026-07-06 実装・実機確認済み）**: 明示 OSC 8 ＋
      ベア http(s):// 自動検出（WRAPLINE で soft-wrap 跨ぎ結合・句読点trim・括弧
      バランス）。リンクセルは点線下線(SGR Dotted+58 注入で run 機構に相乗り)常時表示、
      Ctrl+ホバーで当該「出現」のみ実線（URI dedupe→出現単位 index に後段split、
      同一URL複数箇所の全点灯を修正済み）、Ctrl+クリックで開く（http/https/mailto
      限定 — 端末エスケープ由来 URI に任意ハンドラを起動させない）。
      素クリックは mouse reporting/選択のまま（wt/VSCode 流）
- [x] **Kitty graphics protocol（画像表示・yazi 等）** — 2026-07-06 実装・**実機確認済み**
      （yazi 26.5.6 / SSH native / `TERM=xterm-kitty`、7680×4320 PNG プレビュー成功）。
      実測知見: yazi は `q=2`（完全沈黙）・`f=24` 生RGB・`c=/r=` 省略で送る —
      placement 寸法なし時は「画像の実ピクセル寸法（÷scale factor）で描く」が
      kitty の既定。デバッグは `SHOGUN_KITTY_LOG=<path>` で APC 往復をファイル採取。
      Unicode placeholder 方式限定（U+10EEEE＋合字 row/col・fg 色=画像ID）: placeholder は
      通常セルとして grid を流れるため scrollback/alt-screen/リサイズ追従がタダ。
      `terminal/kitty_graphics.rs` = APC 受動スキャナ（progress.rs と同型・vte は APC を
      握り潰すため手前観測）＋ t=d/f=24/32/100/o=z/チャンク/a=q 応答/a=d。画像 store は
      256MiB 上限 LRU。renderer は placement 全体を行 mask で clip して paint_image。
      カーソル固定の古典 placement・Sixel・iTerm2 は未対応（需要が出たら）。
      TIOCGWINSZ ピクセル寸法も resize 経路に配管済み（yazi の解像度計算用）。
      残課題: `TERM=xterm-kitty` 誘導なしで検出させる恒久策 — **XTVERSION 応答は
      2026-07-06 実装済み**（`terminal/xtversion.rs` 受動スキャナ、
      `DCS >|shogun-desktop x.y ST` を返す。ただし kgp 検出側が既知端末名しか
      見ない場合は効かず、独自 terminfo が残り弾）。ローカル WSL シェル
      （ConPTY 経由）は conhost が APC を剥がす疑いが濃く、native SSH 経路のみ対応
- [x] **Sixel 対応**（2026-07-06 殿下知）— **2026-07-06 実装・実機確認済み**
      （e2e自動: shell window へ合成打鍵で printf sixel → 赤ブロック描画＋カーソルが
      画像下行へ降りるスクロール動作をスクショで確認）。
      ローカル WSL シェル（ConPTY）で画像を出す道: ConPTY は Sixel(DCS) を通す
      （WT 1.22+ 実証済み）。kitty(APC) は ConPTY に剥がされるため相補関係。
      設計は **kitty 基盤への合流**: `terminal/sixel.rs` の受動 DCS スキャナ
      （中間バイト検査で XTGETTCAP/DECRQSS 除外）→ 自前デコーダ（VT340 パレット・
      DEC HLS・raster/repeat/CR/LF・4096²上限）→ `KittyImageStore` に合成 id
      （0xE00000+）で格納 → **placeholder セルを parser に注入**。grid 上は通常
      セル故 scrollback/alt-screen/resize/描画がすべて kitty 経路の再利用となり、
      カーソル固定 placement 追跡を丸ごと回避。注入前後で SGR fg を退避復元。
      cell 寸法は resize が session 共有 atomics へ書き reader が参照。
      DA1 は vendored patch で `?62;4;22c`（sixel 能力 4 を広告 — lsix 等の検出）。
      デコーダ 9 + e2e 1 の回帰テスト。旧メモ:
      カーソル固定 placement（placeholder 方式が使えないため、grid 行に紐づく
      配置追跡が必要 — kitty で回避した宿題がここで来る）。
      検出はアプリが DA1 応答の `4` を見る — vendored alacritty の DA1 応答への
      capability 追加パッチも必要
- [x] **Synchronized output (DEC ?2026)** — **2026-07-06 実装**。vte 0.13 の Processor が
      BSU/ESU バッファリングを内蔵（BSU 後のバイトは grid に触れず buffer、ESU/満杯で
      一括適用）＋ DECRQM 2026 応答も vendored term が既対応 — 残っていた宿題は
      **タイムアウト駆動**のみ。ブロッキング read のままでは ESU 不達＋PTY 沈黙で
      画面が凍るため、reader を IO スレッド（read→チャネル送出）と parse スレッド
      （sync 保留中は `recv_timeout(deadline)`、期限切れで `stop_sync` flush）に分割。
      EOF 時も開きっぱなしの sync を flush。回帰テスト 3 本（ESU 適用・EOF flush・
      timeout flush、StallingReader で沈黙 PTY を再現）
- [x] **Focus reporting (?1004)** — **2026-07-06 実装**。`TerminalSession::report_focus`
      （mode gate＝FOCUS_IN_OUT・dedup 内蔵）＋ shell window は activation 直結、本窓は
      「window active かつ当該タブ選択中」（OSC 9 抑止と同じ規則、将軍 tab0 / 家老陣 tab5）。
      spawn 完了時にも初期整合。回帰テスト 2 本（gate+dedup / mode off 無音）
- [x] OSC 0/2 ウィンドウタイトル反映 — **2026-07-06 実装・実機確認済み**
      （bash PROMPT_COMMAND の自動タイトル＋明示 OSC 2 上書き→プロンプトで復帰、
      GetWindowText で検証）。Event::Title/ResetTitle を
      listener が session 共有スロットへ、shell window が render で OS タイトルへ
      dedup 反映（既定「シェル」）。本窓タブは対象外（tmux セッション名表示を優先）
- [x] **OSC 9 / 777 デスクトップ通知** — 2026-07-05 実装（Ghostty 準拠挙動）。
      OSC 9;4 スキャナを OSC 9 / 777 汎用観測器に拡張（`terminal/notify.rs`）。
      Ghostty parity: ConEmu サブコマンド (9;1〜10) は通知にしない・title 63 /
      body 255 バイト切詰・**フォーカス抑止**（window active かつ当該タブ選択中は
      出さない = requireFocus 挙動。本窓は将軍 tab0 / 家老陣 tab5 を個別判定）。
      配送は tauri-winrt-notification (MIT/Apache-2.0)、HKCU AppUserModelId 登録で
      「将軍デスクトップ」名義・失敗時 PowerShell AUMID フォールバック。
      設定 `terminal.desktop_notifications`（既定 on）＋
      `terminal.desktop_notifications_multiagent`（**家老陣 tab は既定で握りつぶし**。
      多エージェント常時発報のため。マスターと AND）。設定タブ「ターミナル」節に
      Switch UI あり（切替は即時反映・保存で永続化）。
      CC 側は `preferredNotifChannel` で「足軽完了→トースト」成立。
      **2026-07-05 実機確認済み**（殿確認）。
      mac 配送も実装済み: mac-notification-sys 0.6.15（objc2系・notify-rust の中身と同じ。
      notify-rust 本体は非optional async-std（開発終了ランタイム）を担ぐため不採用）。
      set_application("app.rikkalab.shogun-desktop") でバンドル名義。**MBA実機確認待ち**。
      Linux も notify-send spawn（依存ゼロ・ゾンビ回収付き）実装済み — port 時に実機確認。
      アクション/アイコンが要る日が来たら zbus (blocking) へ昇格
- [x] **box-drawing グリフの procedural 描画（角丸・斜線・混在 junction・線幅）**
      — 2026-07-08 実装・実機確認済み（`/config`・codex 起動枠・btop で崩れず描画）。
      geometry 優先＋font-fallback 方針: 角丸 ╭╮╯╰ = 制御点をセル中心に置く二次ベジェ
      （中心線が角を内側にカット＝rounded ┌）、斜線 ╱╲╳ = `PathBuilder::stroke` 直線、
      混在 heavy/light junction ┝〜╊（38 字）を `junction!(u,d,l,r)` マクロで腕別太さ
      厳密化（従来は範囲 arm で一律 light 近似だった／Unicode 正式名と 1 字ずつ照合）。
      線幅はセル幅基準（lw=cw/8, hw=cw/4）— 旧来の高さ基準は monospace で light 線が
      約 2 倍太く見えた。geometry で拾えない字のみ `shape_line` でフォント描画。
      `crates/rikka-terminal-core/src/renderer.rs`（paint_box_char / is_geom_box_char）
- [ ] **タスクバー IME インジケータ（あ/A）追従 — TSF text store 実装中**
      （2026-07-09 着手・M1a 実機検証待ち）。真因は実機トレースで確定: Win11 新
      MS-IME は IMN_SETOPENSTATUS/SETCONVERSIONMODE を送らず（candidate/
      composition のみ着信）、モード状態は TSF 側 — IMM32-only の gpui では
      インジケータが追従しない。失敗2件は記録済み: ①空 doc AssociateFocus =
      入力を強奪して日本語入力全壊（即 revert）②winit 流 ImmAssociateContextEx
      (IACE_DEFAULT) = 入力無事だがインジケータ不動。正攻法 =
      `crates/rikka-terminal-gpui-ime` の ITextStoreACP text store（arcweft
      MIT adapt・CREDITS 帰属・26 メソッド + lock model・Windows compile 済
      400586b）。M1a (4d0ef14) = SHOGUN_TSF env ゲートで shell window の
      focus_in/out から focus/blur のみ配線（gpui 無改変・既定挙動不変）。
      **COM 配管は headless smoke で実証済**（`cargo run --example tsf_smoke`
      → 全段 OK・sink advised: yes = TSF が store に実際に食いつく）。
      **M1a 実機検証済（2026-07-09・自律 e2e）**: `e2e/tsf-indicator-test.ps1`
      （合成クリック＋半角/全角＋トレイ物理座標スクショ。4K@200% は
      SetProcessDPIAware 必須 — 非 DPI-aware pwsh の座標仮想化でクロップが
      ズレる罠を踏んだ）→ **タスクバー表示が A→あ→A とトグルに完全追従**。
      TSF ログも focus→AdviseSink→RequestLock(0x6)→blur の全経路を確認。
      仮説確定: 実 text store 付き文書を SetFocus すればインジケータは追従する。
      **M1b 実装＋実機検証済（2026-07-09・自律 e2e）**: composition sink
      （store と同一 COM object に多重 implement）で preedit/commit を判別、
      Preedit→ime.marked（inline 描画）・Commit→focused pane の PTY・commit 後
      文書リセット（drain 時 = lock 外で OnTextChange）・focus 時 waker（TSF
      callback 内から foreground executor 経由で notify 予約）→render で drain
      （blur で未 drain 破棄 = 誤 PTY 着弾防止）・RequestLock 再入 upgrade
      (TS_E_SYNCHRONOUS/TS_S_ASYNC)。`e2e/tsf-typing-test.ps1`（段階式・vision
      で座標決定）で シェル窓に「あいうえお」を compose→preedit inline 表示→
      Enter 確定→bash プロンプト着弾 まで全経路スクショ＋ログ確認。
      残: 実運用 soak（殿が SHOGUN_TSF=1 で常用して様子見）→問題なければ既定 on 判断
      （殿裁可事項）・GetTextExt へ実 caret 供給（候補窓が既定位置に出る、M2）・
      Linux(IBus)/mac は同 trait の別 backend として後日ラウンド可能
- [x] **DECSCUSR カーソル形状 — 2026-07-09 実装＋e2e 目視済**（beam `│`・underline
      `_`・block・**?25l 非表示もこれで初対応** — 従来は隠しても block が出続けて
      いた）。RenderableCursor.shape を snapshot に載せ、Block=reverse-video /
      Beam・Underline=既定 fg の細 quad（太さ cw/8）/ Hidden=無描画。
      HollowBlock は engine が focus を持たないため Block 扱い。blink 位相は
      未対応（形状のみ・steady 描画）。`e2e/cursor-shape-test.ps1`
- [x] **OSC 10/11/4 色クエリ応答＋XTWINOPS CSI 14 t — 2026-07-09 実装**。
      vim の背景自動判別（OSC 11 ?）と imgcat 系のピクセル寸法取得が成立。
      解決順 = OSC-set された palette entry > 標準 256 palette > renderer 既定
      （fg #E8DCC8 / bg #1A1A1A・cursor は fg）。CSI 14 t はセル数×renderer
      セル寸（TIOCGWINSZ と同じ数字）。handler スレッドへ term を OnceLock/Weak
      遅延バインド（thread が term より先に要るため）。CSI 18 t は既存 PtyWrite
      経由で動作済みだった。回帰テスト 2 本
- [x] **カーソル blink 位相 — 2026-07-09 実装**（DECSCUSR 1/3/5・DECSET ?12 →
      cursor_style.blinking 単一ソース）。SGR blink と同じ 600ms 位相・300ms
      refresh timer に相乗り。?25l 中は flag を落として timer 空回りを防止。
      既定（DECSCUSR 0/2）は従来どおり steady。回帰テスト 1 本
- [x] **選択のグリッド追従 — 2026-07-09 根治＋e2e 実証**（旧題「選択中の自動
      スクロール抑止」）。真因は選択が app 側の画面行座標で、スクロール／出力で
      内容から滑っていた。**alacritty `Selection` へ全面委譲**: グリッド座標で
      保持され scroll・出力回転に自動追従、snapshot が可視範囲を毎回算出
      （選択変更時は session 側で snapshot を即時再構築 — parse スレッドは
      PTY 出力時しか更新しないため）。コピーも `selection_to_string` へ移行=
      **scrollback 跨ぎ・wide/wrap が正確に**。副次挙動: クリック単発は
      ハイライトなし（empty until drag、標準挙動）／TUI が選択下の文字を
      書き換えたら選択はクリア（旧実装は誤テキストを黙ってコピーしていた）。
      `e2e/shell-drag-copy.ps1`: drag→highlight→9行スクロールで**同じ数字に
      張り付く**before/after スクショ＋コピー内容一致で実証
- [ ] リサイズ時のリフロー
- [ ] 検索・設定ファイル・タブ/分割（「本物のターミナル」級の将来項目）
- [ ] **RikkaTerminal 構想**（2026-07-06 殿表明）— 本格ターミナルとして独立プロダクト化。
      **第一段完了 (2026-07-06 殿下知「モノレポ構造で外に出しやすく分割」)**:
      workspace 化し `crates/rikka-terminal` へ terminal/ 一式を抽出
      （lib = 旧 mod.rs。SSH/ConPTY spawn は app 側 `src/pty_spawn.rs` に分離、
      theme::Colors は engine 既定色 default_bg/fg に置換、measure_cell_metrics
      は renderer へ移動）。app は root の `pub use rikka_terminal as terminal;`
      で旧パス互換。レイヤ規約は crates/rikka-terminal-core/Cargo.toml 冒頭に明記
      （engine は SSH・settings・窓を知らない）。CI は --workspace 化。
      残: リポ切り・クレート名/ライセンス確定・vendored gpui/alacritty の扱い。
      shogun-desktop は抽出クレートの利用者となり二重メンテを避ける。
      **2026-07-08 追補**: engine 純化を further — bashrc/shell 注入系（ZDOTDIR
      ラッパー・remote_env_prefix・tmux title 転送）を `rikka-terminal-ssh-integration`、
      エージェント進捗検出（Braille スピナー→AgentProgress）を
      `rikka-terminal-agent-integration` へ分離。engine は両クレートを知らず、SSH/
      settings/窓非依存の規約がより厳密になった（codex 対応の布石）。
      **2026-07-09 追補**: クレート名確定 — engine を `rikka-terminal-core` に
      リネームし衛星（-ssh-integration / -agent-integration / -gpui-ime）と
      ファミリー統一。**プロダクト名/実行時 identity（TERM_PROGRAM・XTVERSION
      の "rikka-terminal"）は据え置き** — パッケージ名のみの変更。app は
      `pub use rikka_terminal_core as terminal;` で旧パス互換のまま。
      **同日 P0 プロトタイプ完成**: `crates/rikka-terminal`（bin・13.4MB）=
      ローカル ConPTY(pwsh→cmd fallback) + gpui 1窓を core に直結した薄い殻
      （~400行）。設計は crates/rikka-terminal/README.md。smoke e2e
      （`e2e/rikka-terminal-smoke.ps1`）で pwsh 起動・SGR色・OSC タイトル・
      URL下線・ペースト実行をスクショ実証。engine の transport 非依存が確定。
      残ロードマップ: P1 タブ(aki-term吸収)/設定/フォント同梱・P2 リポ切り・
      P3 Linux/mac。
      旧 aki-term 構想（wt 代替タブ付きターミナル・設計書あり実装未着手）は
      本構想に吸収候補。検討事項: リポ切りとクレート側ライセンス選定
      （shogun-desktop に GPL 化予定は無い — gpui が Apache-2.0 になり
      旧 Zed-GPL 前提は消滅）、vendored gpui/alacritty パッチの扱い
- [x] terminal_tab.rs の旧 scroll-lock ロジック整理 — **2026-07-09 除去**。読解で確定:
      ①符号は実際に逆だった（gpui wheel-up=正は btop 実機で確証済み、なのに
      wheel-down で lock）②ただし tab pane は display_iter=可視グリッドのみ＋
      PTY-fit サイズでローカル overflow が発生せず、lock/prev_offset/last_gen は
      全て inert（last_gen は書込専用＋render の `let _` 警告封じ）。
      wheel は PTY 転送のみ残し、フィールド 6 個と End キー握り潰しを削除 —
      **End は今後 `\x1b[F` として PTY に届く**（従来はタブ pane で端末に届かない
      隠れバグだった）。shell window の scroll-lock は別系統・現役のまま

## 3. 高リフレッシュ・ヌルヌル構想（殿表明・順序が肝）

**アンチ硬直目標（2026-07-06 殿表明）**: wt は tmux セッションが長くなると
**数秒**固まることがある — あれを構造的に潰す。数秒級はフレーム尻尾でなく
**ブロッキング（同期IO・ロック待ち）監査**の類。UIスレッド上の同期IOを狩る:
- [x] render が毎フレーム settings.toml を読む DrvFs 同期IO（両ターミナルタブの
      session_name）— **2026-07-06 退治**（view にキャッシュ・保存時のみ更新）現状の防御: PTY 処理は IO/parse
別スレッド（UI は Notify 待機＋60fps 合流）・?2026 で tmux 再描画嵐を一括反映・
scrollback/画像 store は上限固定でセッション寿命による肥大なし。残る硬直候補
（見つけ次第ここで消し込む）:
- [ ] 洪水時に parse スレッドが chunk 毎に take_snapshot する再計算コスト
      （generation を UI が消費するまで snapshot を skip する合流で削れる見込み）
- [ ] 行 run 再構築の per-frame CPU（下記 dirty-row cache 案と同件）
- [ ] FairMutex（term/snapshot）の洪水時コンボイ
- [x] **計測ハーネス — 2026-07-09 実装**（`crates/rikka-terminal-core/src/frametime.rs`）。
      `SHOGUN_FRAMETIME=<path>` で起動→負荷（`cat` 大容量 / tmux 全画面再描画連打 /
      `yes` 洪水）→300 build 毎に stats 行が追記される:
      `[ft] frames=300 rows_avg=41 build_ms p50/p95/p99/max | paint_ms … | gap_ms … stalls>50ms=N`。
      build=render_grid の要素構築（coalesce_runs はここ）・paint=行 canvas 描画合計・
      gap=build 間隔（500ms 超はアイドル境界として除外、50ms 超 stall を別カウント —
      **数秒級ブロッキング硬直は gap に出る**）。Drop ガード方式で gpui 無改変。
      v1 注意: 全可視グリッドが単一系列に混ざるため負荷測定は 1 窓ずつ。
      実測ラン（8.3ms 予算判定）はこれから
- [x] **計測ラン — 2026-07-09 自律 e2e で実施**（`e2e/frametime-run.ps1`: 負荷 3 種 =
      yes 洪水 12s / cat 40MB / clear+seq 連打 12s、shell window・Tailscale SSH 経由）。
      **実測**: build_ms p99=0.1〜0.2 / paint_ms p99≤1.6 (max 21ms は初回 shaping の
      1 発のみ) / gap_ms p50=16.7（60fps 刻み）・洪水中 p95≈30〜33ms・stalls>50ms=0
      （洪水遷移窓で 4 回・max 266ms のみ）。**結論**: ①render CPU (build+paint<1ms)
      は 8.3ms 予算に対し余裕 20 倍超 → dirty-row cache (下記③) は現サイズでは
      効果薄・棚上げ ②実効レートの律速は 16ms coalesce (下記④が本命) ③数秒級
      freeze は洪水下でも皆無 = 既存 IO/parse 分離+?2026 が効いている。
      **スループット実測（`e2e/cat-timing.ps1`・疑義検証）**: `time cat` 40MB 描画込みで
      real 0.686s ≒ **58MB/s 全パイプライン**（sys≈real = cat 側はほぼ非待機）→
      「SSH スロットルで楽をしていた」説は棄却。速さの構造理由 = サンプリング型
      （parse スレッドが全速で飲み、UI は 16ms 毎に snapshot を描くだけ — 描画コストが
      バイト量と結合しない。wt の freeze はこの結合が原因側）。副記: cat は 0.7s で
      完了していたため前回計測の cat 窓は大半プロンプト待ち（yes/clear+seq の 12s
      持続洪水データは有効）。残る観察点は洪水中 gap p95≈30ms（2vsync 落ち）のみ

段階目標（殿裁定 2026-07-03: いきなり 200Hz 級は狙わない）:
**第1段 = 120fps（予算 8.3ms）で「ヌルヌル」を成立させる**。
200Hz 台への追従は第2段で、vsync 連動にしておけば自然に伸びる余地として残す。
Air は 60Hz（ProMotion なし）→ 省電力側の検証機。

1. [x] スクロールバック（shell window 分は済み）
2. [ ] ピクセル単位スクロール補間（現状セル単位ジャンプ）
3. [ ] 行 shape 結果のキャッシュ＋dirty row 差分描画
      （スクロール中は行内容不変＝全ヒットで最軽量、という好条件あり）
      2026-07-06 検分（gpt-5.5 献策の突合）: shape_line は gpui 内部の
      LineLayoutCache が前フレーム同一 (text,font,runs) を再利用済み → 残る
      CPU コストは毎フレーム全行の coalesce_runs による Run 列組立
      （renderer.rs render_grid）。実装は「snapshot に行 hash → 不変行は
      前回 Vec<Run> を使い回す」一本で box drawing ループも巻き込んで消える。
      背景 quad コアレスと空白 run スキップは実装済み・カーソル/blink の
      overlay 化は不可（絶対配置 overlay canvas の paint は画面に届かない
      実測 2026-07-03）。着手前にフレーム時間の実測必須（btop 全画面等）。
4. [x] **コアレス 16ms→8ms — 2026-07-09 実施**（`FRAME_COALESCE`、両窓共通定数）。
      60Hz 環境での after 計測: gap p50=16.7ms（アクティブディスプレイの vsync 壁
      — gpui VSyncProvider は DwmFlush 追従で 60fps 固定ではない）、p95 は 30〜33
      → 18〜20ms に改善（vsync 2 枚落ちがほぼ消滅）。
      **200Hz モニタ復帰後の最終計測（2026-07-09）: 第1段成立** — yes/cat 洪水中
      gap p50=5.0ms（≒毎 vsync・200fps）p99≤8.6ms、clear+seq 連打中 p50=9.1ms
      （≈110fps、8ms coalesce 経路の素の値）。stalls>50ms は持続洪水中ゼロ。
      render コストは 200fps 駆動でも build 0.1ms / paint p99≤1.0ms と余裕。
      副観測: 洪水中 5ms は coalesce より速い = notify 以外の dirty 源
      （スクロールバーのフェード等）が gpui を vsync 全速で回している。
      アイドル挙動不変（notify 待機のまま）。
      第2段 = タイマ撤去・vsync 直駆動（全経路 5ms 化の余地）は据え置き

## 4. リリース / インフラ（殿の作業を含む）

- [x] **push**: 2026-07-04 完了（Windows git で `--force-with-lease`、61c2b38）。
      匿名化履歴（`Users\aki`→`Users\dev`）が origin に反映済み。
      過去メモの旧ハッシュ参照は 2026-07-04 に新ハッシュへ付替済み
- [ ] shogun-suite を GitHub に新規作成して push — **当面棚上げ**（shogun-core は
      crates/shogun-core に暫定同梱済み・CI 単一 checkout。suite 復活時に two-repo 構成へ戻す）
- [x] CI 緑化 → Release: **v0.1.0（2026-07-04 初回）→ v0.2.0 → v0.2.1 発行済み**。
      v0.2.1 = mac アイコン角丸プレート化・正規 iconset・zip 新構造・plist バージョン注入込み
- [ ] **MacBook Air 展開**: v0.2.1 の zip を取得 → 展開 → `setup.command` を
      右クリック→開く（quarantine 解除＋起動まで自動）。
      実機検証: IME/ことえり・cmd 系キー・絵文字 COLRv0（CoreText 側は未検証！）・
      アイコン見た目・全機能。参照第一候補は ghostty（MIT）、Zed/cosmic-term は最終手段
- [ ] mac 版 E2E（osascript/CGEvent）— 必要になってから
- [ ] **Linux 対応時**: gpui 0.2.2 は Linux で FontFallbacks 無視＋emoji 判定が
      NotoColorEmoji 固定 → 上流 PR が本命。Noto に落ちる分には殿許容済み
- [x] **WSLg 実機スモーク（2026-07-05・CI artifact 直行）**: Wayland は即死
      — gpui は `xdg_wm_base` v2+ 要求、WSLg Weston は v1 のみ
      （wayland/client.rs:151 `wm_base` bind unwrap panic）。
      回避 = `WAYLAND_DISPLAY= ./shogun-desktop` で X11(Xwayland) に落とす。
      結果: 起動 OK・日本語含め表示 OK・**ウィンドウタイトルのみ豆腐**（別要因、
      X11 タイトルはコンポジタ側フォント描画 — アプリ側では直せない可能性大）。
      絵文字は未テスト。予想（FontFallbacks 全滅で本文豆腐）より大幅に良好
