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
- [x] アイドル時 CPU がほぼ 0%（16ms ポーリング全廃・イベント駆動化）
      — **2026-07-05 実機確認済み**（殿計測: アイドル時 ≤0.5%）
- [ ] shell window: 履歴スクロール／ステータスバー「履歴 N行上」／キー入力で最下部復帰
      — **2026-07-05 描画バグ根治（7bf34c4）**: take_snapshot が display_iter の
      グリッド座標（履歴行=負のline）を生 usize キャストしており、履歴行が全部捨てられ
      残りが上にずれて描画＝「スクロールで先に進む」ように見えた。screen row =
      grid line + display_offset へ修正（cursor同様・回帰テスト付き）。
      ホイール符号・alacritty履歴側は計装実測で最初から正常だった。実機確認待ち
- [ ] shell window: Shift+PageUp/PageDown ページング
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
- [ ] tailscale アイドル後の入力引っかかり解消（keepalive + 非同期書込）
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
- [ ] マウスレポーティングの残ギャップ（2026-07-05 opus 監査で列挙。Ghostty 比で
      核心経路=SGR click/drag/hover は**バイト単位一致**と判定済み。ホイール修飾ビットと
      X10>223 drop は同日適用済み）: 水平ホイール (btn 66/67)・?1005 UTF-8・
      ?1015 urxvt・?1016 SGR-pixels・shift-capture (kitty 系)。いずれも低優先
- [ ] **ssh/tmux ペインの履歴スクロール** — tmux は alternate screen 故サーバ側履歴。
      tmux `mouse on` なら `wheel_pty_bytes` を本窓ペインに配線するだけで動く見込み。
      copy-mode 中継 / capture-pane 方式はその後
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
      実機確認待ち: `printf '\e]9;4;1;50\a'`
  - [ ] Phase 2: Windows タスクバー進捗 (ITaskbarList3::SetProgressValue)。
        HWND は vendored gpui に accessor を足すか EnumWindows+PID で取得
- [x] **OSC 8 ハイパーリンク（2026-07-06 実装・実機確認済み）**: 明示 OSC 8 ＋
      ベア http(s):// 自動検出（WRAPLINE で soft-wrap 跨ぎ結合・句読点trim・括弧
      バランス）。リンクセルは点線下線(SGR Dotted+58 注入で run 機構に相乗り)常時表示、
      Ctrl+ホバーで当該「出現」のみ実線（URI dedupe→出現単位 index に後段split、
      同一URL複数箇所の全点灯を修正済み）、Ctrl+クリックで開く（http/https/mailto
      限定 — 端末エスケープ由来 URI に任意ハンドラを起動させない）。
      素クリックは mouse reporting/選択のまま（wt/VSCode 流）
- [ ] **Synchronized output (DEC ?2026)** — 新規 (2026-07-04)。CC 2.1.191 は更新を 100ms に
      合体するなど高頻度書換方向。begin/end synchronized update の間は描画を保留して
      チラつき根絶。WT/Ghostty/kitty 対応済みの現代標準。alacritty_terminal がパース済みなら
      renderer 側でフラグを見るだけの可能性。高リフレッシュ構想 §3 の dirty batch 境界にも流用可
- [ ] **Focus reporting (?1004)** — 新規 (2026-07-04)。focus in/out で `CSI I`/`CSI O`。
      CC 2.1.181 の presence 検知（在席中は mobile push 抑止）の標準経路。vim/tmux も使う。数十行
- [ ] OSC 0/2 ウィンドウタイトル反映（実害小）
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
