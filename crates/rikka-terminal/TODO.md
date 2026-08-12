# RikkaTerminal 課題一覧

完成済みの設計・経緯は `IPC.md`（IPC/タブ移送）と `README.md`/`packaging/README.md`
（既定ターミナル化）を正とする。ここは**残っている課題だけ**を書く。
（最終更新: 2026-07-13・conhost Reflow 移植完遂 `dbc90c8` 時点）

## タブ移送（P2 残・優先度順）

- [x] ~~画像 store の搬送~~ — store を PNG 化して `state.images` で搬送
      （新しい順に 1.5MiB 予算・溢れは従来どおり blank）。予算に乗った id の
      placeholder セルは replay で素通し（fg=id を厳密保存する専用 SGR）、
      受信側 `into_session` が store 復元。E2E で pixel/placement/placeholder
      復元を実証。予算外・旧送信元は従来挙動に fail open。
- [x] ~~OSC 8 ハイパーリンクの搬送~~ — `replay_bytes` がリンク run を
      OSC 8 で再発行（history は行単位で self-contained・id/uri 一致で
      受信側 parser が結合）。roundtrip テストで cell.hyperlink 復元を実証。
- [x] ~~monarch 再選出 v2~~ — 各窓プロセスが 5 秒毎に heartbeat
      （idempotent な RegisterWindow upsert = 生存確認と directory 更新を
      兼ねる）。不達なら bind 競争: 勝者は `monarch_accept_loop` でその場から
      monarch 業務を開始（空 directory は他窓の次 heartbeat で再充填）、敗者は
      次 heartbeat で新 monarch に自動再登録。Standalone 窓も watcher 経由で
      調整系に復帰する。再選出→登録→resolve をユニットテストで実証。
- [x] ~~窓単位 addressing~~ — window_id を per-window 化
      （`pid<<20|seq` 採番・`hub::window_pid` で pid 逆算）。全窓が heartbeat
      （`RegisterWindows` = pid 単位の全置換）で directory に個別登録され、
      閉窓は次 beat で自動消滅。移送は `Target::Window(id)` を受信 pump が
      `window_by_id` で着地（fallback: drop 座標→bounds 照合→any_window）。
      Ctrl+Shift+X は具体窓 entry へ、drag-merge の pid 形式 query は
      resolve の pid フォールバックで解決。旧 monarch 混在は id==pid 判定で
      互換。directory の upsert/置換/フォールバックはユニットテスト済。
- [x] ~~`-w <id>` CLI~~ — `rt -w new|last|0|<id>` 実装。monarch が
      `resolve_target`（0=any・実 id・pid フォールバック）で具体窓へ書き換え、
      その窓 socket に Spawn を転送（窓 socket が Spawn 受理）。受信 pump が
      指名窓にタブを展開（相対 dir は送信元 cwd に anchor）。解決不能・窓消滅は
      新窓に fail open。実機で `-w 0` の既存窓着地（新プロセス0）を確認。
- [ ] **Unix の tab move** — SCM_RIGHTS 版の handle 移送。tear-off /
      drag-merge も Windows 専用のまま。
- [ ] elevated handoff 窓プロセス（wire に flag のみ・実装なし）。
      **方針 (2026-07-20 殿発案)**: 昇格執行の実体は **gsudo
      (gerardog/gsudo・MIT・署名済みバイナリ) をバンドル**して使う —
      UAC ダイアログが署名者名義になり、credentials cache で 2 回目以降の
      UAC を省略できる（cache は UAC 削減のトレードオフゆえ**既定 off・
      opt-in**）。ただし工数の本丸は rikka 側の「昇格 PTY ホスト＋IPC」
      （非昇格 UI 窓のまま昇格 ConPTY を別プロセスに握らせる・conhost
      handoff と同型）。昇格実体は抽象化し、gsudo（同梱）> inbox sudo
      (Win 24H2+) > ShellExecuteEx(runas) のフォールバック連鎖に。
      同梱はビルド時に版数＋SHA256 ピンで取得・CREDITS 追記・AV の
      PUA 誤検知歴（署名で概ね解消）に留意。
      **抽象はクロスプラットフォーム前提 (2026-07-20 殿方針)**: 契約を
      「argv を管理者権限で起動するだけ」の薄い ElevationExecutor に絞り、
      P3 Unix 移植時に **pkexec (PolicyKit) / lxqt-sudo / kdesu / doas /
      sudo -A**、macOS の osascript administrator 等を実装として
      差し込めるようにする。cache・GUI プロンプト種別・環境変数の癖
      （pkexec は env をサニタイズ＝DISPLAY/XAUTHORITY 例外要）は各実装に
      閉じ込め、選択は自動検出＋config 明示指定の二段。
      **勝ち筋 (2026-07-20)**: wt ですら昇格タブは**別の管理者ウィンドウに
      集約**（境界設計）で同一窓混在は不可。handoff 方式なら**同一タブ列に
      盾アイコン付き admin タブが混ざる wt 超え UX** になり、Unix でも
      pkexec ベースの root タブを一級市民にする端末はほぼ無い＝差別化実在。
      トレードオフ: 非昇格 UI が昇格シェルへの入力チャネルを持つ＝UI 侵害
      で昇格注入の面（wt が別窓にした理由）。混在既定＋盾の視覚区別＋
      config で「別窓集約モード」も選べる二本立てが固い。プロファイル統合
      （`[[profiles.list]] elevate = true` → メニューに盾）まで含めて
      wt 流の「作りやすさ」。

## 文字レンダリング（adversarial review 2026-07-24 対応済）

- [x] ~~結合文字の喪失~~ — codex/gpt-5.6-sol 指摘(high・実機確定)。snapshot が
      kitty placeholder 以外で `zerowidth()` を捨てており NFD アクセント・
      結合スタック・VS16 が素の base に化けた。**SnapshotCell.zerowidth 追加＋
      coalesce_runs が該当セルを単セル cluster run に隔離**(text=base+trailers・
      char_widths=[w] 維持で下流の桁揃え無傷)＋paint に cluster 分岐(1クラスタ
      一括 shape・リガチャ OFF でも per-char 分解しない)。実機で NFD é・
      結合スタック合成を確認。ZWJ 家族絵文字はグリッドモデル上セル泣き別れ
      (wt 同様)で対象外。
- [x] ~~RTL cluster map の unchecked 減算~~ — 同レビュー指摘(将軍検証で
      medium 格下げ・単純 Hebrew では未発火=gpui は論理順描画)。防御として
      ClusterAnalyzer を saturating_sub 化＋paint 側スライスを境界 clamp。
      敵対的 bidi 出力でも「そのクラスタのグリフ欠落」止まり・クラッシュ不能。
- 再レビュー(89e241c 対象)の残余・いずれも意図的妥協 or 縁:
  - [ ] **検索の zerowidth 非対称** — vendored alacritty の regex_search が
        cell.c しか流さず、分解済みアクセント入りクエリは表示中テキストを
        発見できない(medium・実用頻度低=検索窓入力は IME 経由で NFC)。
        対処は DFA へ base+trailers 供給＋セル逆写像で中コスト。
  - geom 罫線セル(U+2500-259F)+結合文字は quad 描画優先で trailer 無視
    (罫線に結合文字を重ねる出力は現実に無く、base は正しく描画される)。
  - RTL 降順 cluster map はクラッシュ不能化のみ(全グリフが最終クラスタに
    寄り 1 セルに潰れ得る)。完全 BiDi 描画は gpui が論理順描画をやめる
    大改修とセット・現経路では降順 map 自体が未観測。

## resize / reflow（残りは意図的妥協のみ）

- [x] ~~conhost Reflow parity~~ — `dbc90c8` で完遂（実機ゲート
      `width_shrink_grow_keeps_conhost_agreement` 緑・実窓 E2E も PASS）。
- [ ] **alt screen 中の幅リサイズは no-reflow 妥協** — conhost の altbuffer
      reflow は未実測。フルスクリーンアプリは SIGWINCH 相当で再描画するため
      実害はほぼ無いが、厳密 parity なら実測 probe から。
- [ ] 移送 replay の可視行 WRAPLINE 非再現 — 見た目は同一。選択コピーの
      行結合だけが次の再描画まで異なる。

## UI / UX

- [x] ~~「window not found」ログノイズ~~ — 出所特定済: gpui の platform
      callback（frame/activation/hover/input）が窓 close と競走し、in-flight
      分が毎回 `log_err()` で ERROR を吐く（1日で72行）。vendored gpui は
      触らず、自前 FileLogger 側で「target=gpui かつ message 完全一致」だけ
      落とす（`is_benign_log_noise`・ユニットテスト付き・実窓で増加停止確認）。
- [x] ~~タブ strip の空白部への drop = 末尾移動~~ — 実装済（タブ上 drop は
      子が consume するため空白部のみ末尾移動・誤爆なし）。
- [x] ~~drag-merge の挿入位置~~ — drop 点のスクリーン座標を wire
      (`AttachArgs.drop_at`) で運び、受け側が Win32 実寸（client rect＋DPI＋
      strip レイアウト再計算＋scroll offset）から最寄りのタブ間に挿入。
      末尾 append は Ctrl+Shift+X 等 drop_at 無しの経路として温存。
      注: 受け側プロセスが in-process 複数窓を持つ場合の窓照合は
      「窓単位 addressing」解決待ち（現状は clamp で破綻はしない）。
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
- [x] ~~キーバインド設定~~ — `[keys]` で Ctrl+Shift 系 10 アクションを
      再割当可能（keymap.rs・"mod+mod+key" 形式・typo は既定維持＋warn）。
      固定シノニム（Ctrl+M merge・Ctrl+Tab/PageUp/Down・Insert 系・
      Shift+PageUp/Down）は対象外のまま。残: テーマ/配色設定。
- [x] ~~テーマ/配色設定~~ — `[theme]` で 16色＋bg/fg/selection 差し替え、
      **wt 互換モード** `wt_scheme = "Ubuntu"`（wt settings.json ＋ Fragments
      dir を名前解決・engine `theme` を process-global 化・パネル背景も追従）。
      実機で Ubuntu スキーム適用を確認。残: 内蔵スキーム（Campbell 等・wt に
      ファイルが無い分）・OSC 4/10/11 クエリへのテーマ値応答・ライブリロード。
- [x] ~~プロファイルごとのテーマ~~ — `[[profiles.list]].theme` と wt profile の
      `colorScheme`/`profiles.defaults` 継承を honor。タブ生成時に scheme を
      解決して TabSession に保持、`after_tab_change` で engine global へ
      載せ替え（アクティブタブのみ描画ゆえ global 1枚で成立）。同一窓の2タブで
      Ubuntu/Plain 切替=配色入替を実機実証。移送も対応済み: palette は
      `AttachArgs.palette`（19×0xRRGGBB・serde default で後方互換）で wire を
      渡り、relay 経路は `--attach-palette` CSV。tear-off 実機で Ubuntu 維持を
      確認（v1 制限解消）。
- [x] ~~TERM/COLORTERM 未設定~~ — spawn 時に `TERM=xterm-256color`＋
      `COLORTERM=truecolor` を注入（konsole/alacritty 同様・btop 等が色検出）。
      config `[terminal] term`/`identity` で変更可。`identity="ghostty"` で
      XTVERSION/TERM_PROGRAM 詐称（既定 honest・ConPTY 越しは封印）。実機で
      子に `TERM`/`COLORTERM` が届くのを確認。**マルチプラットフォーム化の
      布石** — SD の TermName/TerminalIdentity 相当を rikka にも移植済み。
      残: rikka 独自 terminfo（xterm-rikka）配布と SSH 経路の詐称活用。
- [x] ~~タブの右クリックメニュー~~ — 初版済 (2026-07-23)。profile-menu
      流の自前実装（scrim＋絶対配置・gpui-component 不使用）。項目は
      「ログ開始/停止」（状態追従ラベル・タブ●と連動）と「閉じる」。実機で
      3 段（表示/ログ●/停止ラベル/閉じ）確認。残: 項目追加（複製・
      色変更・移送系）はタブ機能の育ちに合わせて。
- [x] ~~ポータブル配布~~ — 済 (2026-07-23)。wt 流 **`.portable` マーカー**
      （exe 隣にあれば config.toml・既定ログ dir とも exe 隣・%APPDATA%
      完全不可侵）。`packaging/build-portable.ps1` が zip 生成（exe＋
      ConPTY ペア＋rt/handoff＋config.example＋マーカー＋README）。
      hot-reload watcher も portable パス追従。defterm 統合は意図的に
      非対応（要レジストリ＝MSI の領分）。実機 3 点確認（exe 隣 config
      起動読み・実行中書き換え反映・APPDATA before/after 不変）。
- [ ] フォント同梱（Cascadia 等）。
- [ ] **dingbat スピナー（✢✳✶✻✽）の幾何化** — Claude Code のメインスピナー。
      compaction バー（▰▱）と braille 全域は幾何化済（engine 共有ゆえ
      shogun-desktop にも波及・同梱フォント差し替えに免疫）。dingbat は
      意匠再現（スポーク本数・teardrop 形状）に主観が入るため、見た目の
      方向性を決めてから。放射スポークは `diag!` 基盤で描ける。
      調査メモ: Claude Code の UI 文字はネイティブ配布(bun)だと JSC
      バイトコード化で読めない — npm 旧版 tarball（2.1.100 以前は
      cli.js 同梱）を落として grep するのが確実。バー実装は
      `[" ","▏".."█"]`（旧型・幾何化済）と ▰▱（現行 material 型）の二系統。
      副作用の解も兼ねる: フレーム中 ✳ U+2733 だけ Emoji プロパティ持ちで、
      Twemoji 同梱の両アプリ（shogun-desktop・rikka とも同梱済 2026-07-18）
      では fallback が裸の U+2733 を食ってカラー絵文字化する（VS16 なしの
      既定は text presentation が正）。
      ただし CC 側が回避済みで実害は稀: フレーム列は TERM/platform で3変種
      — `TERM=xterm-ghostty`→✳あり(✽→*)・darwin→✳✽両方・その他(WSL含む)
      →✳を`*`に置換。つまり通常の WSL 運用では ✳ 自体が出ない。出るのは
      ghostty 偽装で TERM まで xterm-ghostty にした時だけ（「一度だけ見た」
      の正体）。幾何化すれば偽装時も含め決定論的に消える。被覆実測
      (2026-07-18): Moralerspace=✳✻⠋なし/▰あり・Twemoji=✳のみ・
      Segoe UI Symbol=全部。

## ghostty 比較ギャップ（2026-07-19 調査・優先度順）

- [x] ~~ダブル/トリプルクリック選択~~ — click_count で
      Simple/Word(semantic)/Line を選択、`SelectionKind` が drag と
      streaming 再ピンを通して保存される（ダブルクリックドラッグは単語単位で
      伸びる）。単語/行クリックは無移動 release でも選択維持。共有エンジン
      実装ゆえ SD にもリビルドだけで搭載済み。実機でダブル=単語・
      トリプル=行を確認。
- [x] ~~シェル統合~~ — OSC 133;A マーク（絶対行記録・scrollback cap 超で
      最古から失効）＋ Ctrl+Shift+↑/↓ jump-to-prompt（[keys] 再割当可）、
      OSC 9;9（wt/ConEmu・通知と衝突しない順で判定）＋ OSC 7（localhost の
      file:// URL・%decode・/C:/ 正規化）の cwd 追跡→**新規タブが cwd 継承**
      （明示 dir > cwd(実在チェック) > home）。自動注入は v1 見送り・
      config.example.toml に pwsh/bash スニペット。実機で jump 2 段・
      C:\Windows 継承を確認。残: 自動注入・OSC 133;C/D 活用（出力範囲選択等）。
- [x] **スクロールバック検索** — 済 (2026-07-19)。vendored alacritty の
      `RegexSearch`/`search_next`（regex・smart-case・履歴込み wrap 検索）を
      エンジン直結（`search_set`/`search_step`/`search_match_for_render`）。
      Ctrl+Shift+F（[keys] `search` 再割当可）で共有ウィジェット
      `search_bar.rs` の VSCode/wt 風バーが右上に出る: 入力欄（caret・
      不一致で赤枠）＋ Aa トグル（Alt+C・強制 case-sensitive、オフ=
      smart-case）＋件数 3/12（cap 1000 で 999+・RegexIter 全収集）＋
      ↑/↓/✕ ボタン（SearchHandlers 経由 host listener）＋ Ctrl+V ペースト。
      incremental 検索、Enter/Shift+Enter で次/前（マッチは上 1/3 へ
      スクロール）、Esc/同チョードで閉じ。全マッチ薄 gold＋current 濃 gold
      の 2 段ハイライト。SD（shell 窓＋将軍/家老陣ペイン）にも同配線。
      実機検証: 3 段（開閉/incremental/step）＋件数/Aa/×クリック全 PASS。
- [x] **リガチャ（ghostty 同等）** — 済 (2026-07-20)。真因は二重: (1)
      gpui が features 未指定でも**空 IDWriteTypography を SetTypography**
      し、DirectWrite の既定 OpenType features（calt/liga/clig = フォントの
      合字）を全滅させていた（空 typography = 既定の置換）。空なら Set しない
      修正で**フォントの合字が既定 ON**（ghostty の既定と同じ）。(2)
      force_width の**グリフ連番ピン**が合字グリフ（n 文字 1 グリフ）で
      後続を左に詰めるため、`glyph.index`（クラスタ開始 byte）から数えた
      **セル番号ピン**に一般化（kitty/ghostty のモデル・合字なしでは旧挙動と
      同一・BiDi 逆行は再カウント）。さらに DWrite TextLayout は
      **default-on feature を 0 指定で切れない**（実測）ため、OFF は
      renderer の **per-char shaping**（合字は shape 境界を跨げない）で実現。
      config は `[appearance] font_features = ["-calt"]`（ghostty 書式
      ±tag / tag=N・BOM 付き config も救済）。実機マトリクス 3 態 PASS:
      既定=合字 ON（==> ≠ ≥ ≤ → ≡・4 連矢印でも列整合維持）/
      -calt=全離散/ ss19=slashed zero ＋合字維持。SD は既存 settings 配線
      がそのまま生きる（bundled Moralerspace の合字も既定 ON 化）。
      注: カーソル通過セルは run 分割で合字が一時解除（kitty 同等・利点）。
- [x] **ペイン分割 v1** — 済 (2026-07-24)。タブ= PaneNode ツリー
      （Leaf{TabEntry+measured}/Split{ratio}・Empty 墓標で in-place 手術）。
      Ctrl+Shift+O/U（[keys] split_right/split_down）で右/下分割（新ペインは
      cwd 継承＋フォーカス）、Alt+矢印=幾何ナビ（正規化 rect 最近傍）、
      Ctrl+Shift+W=分割中はペイン閉じ（sibling 昇格）。描画=再帰 flex
      （relative 比率＋1px divider＋非フォーカス wash）。**PTY fit の所有権
      分離**: 非分割=窓由来一括／分割=overlay measured 経由 per-pane
      （振動防止）。selection は pane id で leaf 直接解決。移送系は
      is_split ガード（分割タブは移送不可=Phase C）。実機: 分割/両ペイン
      入力/Alt+←/W 閉じ+再フィット全 PASS。v1.1
      (2026-07-24 殿指摘「掴んでる時の判定が視覚的に分からない」):
      **ドラッグ視覚+ドロップゾーン** — タブ帯=挿入位置の左エッジ鋼青バー
      ＋空白部 wash、ペイン=ドラッグ中のみ 5 ゾーンをマウント（縁 4=**その
      方向に分割合流**(wt/VSCode 流タブ→ペイン)・鋼青 wash プレビュー、
      中央=従来 detach 点線）。合流不可（アクティブ自身/分割タブ）は縁
      ゾーン自体を出さない=誤誘導なし。通常時はヒットテスト完全無縁
      （dragging_tab 条件マウント）。実機: 左ゾーン wash→ドロップで左
      ペイン合流を確認。v1.2
      (2026-07-24 殿裁可 A+B): **ペイン切り離しのマウス UI** — A=ペイン
      右クリックに「タブへ分離/新しい窓へ分離」（分割中のみ表示）、B=
      **ペイン上端中央のホバーハンドル**（鋼青・⋯）を掴んで PaneDrag:
      タブ帯→タブ化（挿入バー共用）・他ペイン縁→組み替え（ゾーン共用・
      remove→split_at）・窓外→新窓。元ペインは opacity 0.5。TabDrag と
      対称の per-型リスナで既存視覚基盤を全再利用。実機: ハンドル出現→
      タブ帯ドロップ→タブ化を確認。v1.3 (2026-07-24): **分割線ドラッグ
      リサイズ＋ペインズーム**。リサイズ=Split ごとに 7px 透明グラブ帯を
      absolute 重畳(1px 線は不変・paint 最後=ヒット最優先・ResizeLeftRight/
      UpDown カーソル)、掴むと root の on_mouse_move が (path,横縦) から
      split_rect(正規化)×ペイン領域で ratio 直算(clamp 0.1..0.9)。実機で
      中央→250px 左が px 精度で追従。ズーム=Ctrl+Shift+Z([keys] zoom・
      RO ページ掲載)、Tab.zoomed でツリー不変のまま描画だけ折り畳み
      (くすみ金「ズーム中」バッジ・グリップ非表示・分割/ペイン閉じで自動
      解除)。解除で非対称比率ごと復帰を実機確認。v1.4 (2026-07-24 殿指示
      「タブ右クリックあたりから」): **broadcast input** — タブ右クリック
      メニューに「ブロードキャスト入力を開始/停止」（分割タブのみ表示・
      状態追従ラベル）。ON でキーストローク（key_to_pty_bytes）・打鍵文字
      （WM_CHAR→core ImeHost::ime_commit 新フック・rikka が send_input へ
      override・SD はデフォルト実装で挙動不変）・IME 確定・ペースト 3 経路
      が active タブ全ペインへ複製。encode は active ペインの mode 流用
      （per-pane DECCKM 差は妥協）。**視覚=各ペイン錆朱 2px 枠＋タブ » 
      マーク**（誤爆防止最優先）。単ペイン化で自動 OFF。実機で echo hi が
      両ペインで実行されるのを確認。v1.4.1 (2026-07-24 殿「タブまたいでたら
      ダメ？」): **全タブブロードキャスト** — メニュー第2項目「全タブへ
      ブロードキャスト」（タブ2個以上で表示・window 単位 `broadcast_all`・
      per-tab トグルと OR 合成）。この窓の全タブ全ペインへ複製（iTerm2
      "all panes in all tabs" 相当・別窓プロセスは対象外）。ON 中は全タブに
      » マーク。実機で裏タブにも echo hi 実行を確認。v1.4.2 (2026-07-24 殿
      「選択できるとなおよい」): **選択ブロードキャスト** — ペイン右クリック
      「ブロードキャスト対象を切替」(常時表示・ラベルは無状態=render 時
      capture の stale 回避)。TabSession に broadcast_target AtomicBool
      （session 付帯ゆえ移送・組替えでも保持）。マークされたペインは
      **どのタブ/ペインにフォーカスがあっても**入力複製を受ける（タブ内/
      全タブトグルと和集合・`input_sessions()` が Arc ptr dedup で収集）。
      右クリックでもペインフォーカスが移るよう修正（メニューの pane 系
      action が「クリックしたペイン」に効く前提の是正）。視覚=マーク
      ペインに錆朱枠(常時)＋所属タブに »。実機3ペイン: マーク+active
      のみ受信・非対象ペイン無傷を確認。**副産物: 入れ子分割の縦崩壊
      バグ根治** — percent height/flex-grow は入れ子 Split の indefinite
      親で content size に負ける(taffy)→ **percent-inset 絶対配置**
      （containing block 基準=常に definite）へ全面変更。50/50 復元・
      divider ドラッグ/ズーム回帰 PASS。v1.4.3 (2026-07-24 adversarial
      review 対応・codex/gpt-5.6-sol 指摘 high): **per-recipient キー
      エンコード** — 従来は active ペインの TermMode で1回 encode した
      バイト列を全対象へ複製しており、kitty keyboard protocol 混在
      (SSH 経路で広告継続)だと誤制御列配送・printable 取り残しが起きた。
      `keys::key_delivery_plans`(対象ごとの mode で個別 encode・全 None
      なら非消費=WM_CHAR 経路へ)＋`printable_text`(printable 判定の
      単一ソース化)＋main の `send_key_input`(None 対象へ text 補填・
      どれか Some で stop_propagation=WM_CHAR 二重配送を構造排除)。
      ユニット2件(混在 Enter=CSI 13u vs CR・printable 混在/非消費)＋
      回帰 E2E PASS。残: 分割タブの移送/複製・クロス窓のゾーン表示・
      broadcast のキー割当([keys])。
- [ ] **Quick terminal** — グローバルホットキーで出すドロップダウン窓。
- [ ] 細目: grapheme clustering（mode 2027）・minimum-contrast・正規表現
      URL 検出の網羅（OSC 8＋ベア URL 検出は実装済みの範囲）。内蔵テーマ集・
      ライブリロード・terminfo 配布は既存項目の「残:」を参照。

参考・rikka 側の優位（比較の公平のため記録）: Windows 対応そのもの＋既定
ターミナル handoff・wt CLI/プロファイル/スキーム互換・sixel・ライブセッション
のタブ移送一式（tear-off/drag-merge・画面/画像/テーマ搬送・クラッシュ隔離・
monarch 再選出）・OSC 9;4 プログレス UI。ghostty は現時点 Windows 未対応。

## wt 比較ギャップ（2026-07-20 調査・実用度順）

体感差はほぼ「ペイン分割＋設定まわり」に集約（プロファイル互換・defterm・
進捗 UI・検索・リガチャは解消済み）。

実用ギャップ大:

- [ ] **ペイン分割** — ghostty ギャップと共通の最大物（broadcast input も
      この上に乗る）。cli.rs の TODO 宣言参照。
- [ ] **設定 UI＋保存即反映** — **第一段（ライブリロード）済 2026-07-23**:
      `notify`(ReadDirectoryChangesW・イベント駆動・120ms デバウンス・親 dir
      監視で atomic save 耐性・render パス無変更) で config.toml 保存が即
      反映（フォント/テーマ/keys/プロファイルメニュー/検索スタイル/
      logging。acrylic は再起動・term/identity と profile command は新規
      タブから）。keymap/session_log は OnceLock→RwLock 化。実機で起動中
      書き換え→背景紫+font_size 18 追従を確認。**第二段（設定ウィンドウ v1）済
      2026-07-23**: Ctrl+Shift+,（[keys] settings）/⌄メニュー「設定...」で
      別窓（singleton・OS タイトルバー・検索バーシート統一）。項目=外観
      (フォント/サイズ/行高/検索意匠)・ターミナル(スクロールバック/TERM/
      識別)・ログ(保存先/入力記録/自動開始)。テキスト欄は search-bar 流
      自前入力(クリックfocus・Ctrl+Vペースト・日本語はペーストで)。**保存
      は toml_edit でコメント保持書き出しのみ**（decor 移植で行末コメント
      生存・新セクションは明示 [table]・壊れた config は絶対 clobber しない
      — 全てユニットテスト済）・適用は hot-reload watcher に一本化。dirty
      項目だけ書く。v1.5 (2026-07-23):
      **カード化＋スクロール本体＋フッタ固定**（クランプされても保存が
      見える）・status 色分け（エラー=錆朱）・**テーマカード**（wt_scheme/
      背景色/文字色・#RRGGBB 入力に生きた色スウォッチ）・acrylic チェック
      （要再起動注記）・**空にしたらキー削除**（toml_edit remove・テスト
      済）。v1.6 (2026-07-23): **wt スキーム
      ピッカー**（wt 操作感・殿要望）= `wt_schemes::catalog`（settings.json
      ＋Fragments を 1 回走査・名前＋解決済み Palette・窓 open 時のみ IO）
      → ドロップダウン（各行に bg/fg スウォッチ・「(なし)」で解除）＋
      選択スキームの **19 色プレビュー帯**。オーバーレイはルート配置
      （スクロールコンテナ内 absolute の罠を回避）。実機で列挙→選択→
      プレビュー→保存(`wt_scheme = "Ubuntu"` のみ書出)を確認。v1.7
      (2026-07-23 殿指示): **wt 流ページ分け** — 左ナビレール（外観/テーマ/
      ターミナル/セッションログ・選択に accent バー）＋右ページ（大見出し・
      スクロール維持=無茶なサイズ対策）＋フッタ固定。窓 580x480 に縮小。
      実機でナビ切替確認。v1.8 (2026-07-24 殿指示「確認がメイン・RO で
      いい」): **キー操作ページ（読み取り専用）** — keymap に共有解決
      テーブル `resolve_all`（live keymap と同一経路）＋ `display_rows`
      （"Ctrl+Shift+T" 整形・矢印はグリフ・customized 判定=既定と差）。
      設定窓 5 ページ目に全 16 アクション（和名ラベル・キーキャップ風
      チップ）＋ **[keys] 上書きは鋼青ドット＋凡例**（上書き有り時のみ）
      ＋固定ショートカット 7 種（Ctrl+Tab 系/Insert 系/Shift+PageUp/
      Alt+矢印/Ctrl+Shift+M）。実機で既定・alt+n 上書きの両ケース確認。
      残: キーバインド/テーマの本格エディタ (v2)・フォント候補リスト。
- [ ] **コマンドパレット**（Ctrl+Shift+P） — アクションの発見性で wt が上。
- Quake モード / グローバルホットキー — ghostty ギャップの
  **Quick terminal と同一物**（そちらを参照）。
- elevated（管理者）タブ — 既出「elevated handoff 窓プロセス」参照
  （wire に flag のみ）。

実用ギャップ中:

- [ ] キーバインドの網羅性 — wt は全アクション自由割当。rikka は主要
      チョード 13 個の再割当のみ。
- [ ] コピーの書式 — HTML/RTF コピー・Export text（バッファ書き出し）
      が無い（プレーンのみ）。
- [ ] アクセシビリティ（UIA/Narrator） — gpui の Windows UIA が弱く
      構造的に重い。長期課題として認識のみ。
- [ ] New Tab メニューの階層化（wt の folders） — 現状フラット。

飾り級（小・要望が出たら）: 背景画像・ペイン透過度スライダー・
unfocused appearance・visual bell・Terminal Chat（Copilot 統合）。

参考・rikka 側の優位（wt 比）: kitty graphics（wt 未対応）・kitty
keyboard protocol・Tera Term 風セッションログ・検索の regex/全マッチ/
件数（wt はリテラル中心）・タブ移送の搬送力（drag-merge でテーマ/画像/
リンクごと）・Twemoji 同梱の環境非依存絵文字・conhost 完全互換 reflow・
リガチャの per-char off 制御。

## セキュリティ

- [x] ~~IPC の権限境界~~ — 名前空間は rendezvous であって境界でないと判明
      （USERNAME 偽装で別 monarch に接続できた実証が発端）。二層で恒久対処:
      (1) listener を現ユーザー SID のみの DACL に制限
      （`ipc::security::owner_only`・Windows 固有を seam に隔離）、
      (2) `pull_attach` を OS 認証済み peer PID に束縛（`attach.pid ==
      Conn::peer_pid()`・不明なら fail-closed）。`DUPLICATE_CLOSE_SOURCE`
      故に偽 pid で第三者ハンドル窃取＋破壊ができた穴を塞ぐ。実窓で正規移送
      不変を確認。**Unix の listener ACL は未対応**（owner_only に一手）。
- [ ] **Unix listener の権限境界** — 現状 abstract namespace は netns 内から
      到達可能かつ ACL/mode が一切効かない。P3 Unix 移植時の方針（2026-07-16
      決定）:
      1. **アクセス層** = filesystem socket を **0700 の per-user ランタイム
         dir**（`$XDG_RUNTIME_DIR`／macOS `$TMPDIR`・既定で 0700）に置く。親
         dir の traversal 拒否で「現ユーザーのみ」を表現でき、socket mode を
         OS が尊重するかに依存しない。`$XDG_RUNTIME_DIR` 未設定時の
         `/tmp/...-$UID.sock` fallback は 0700 親 dir を自前生成・検証して
         symlink/再bind レースを防ぐこと（ここが Unix 実装の肝で、ACL より
         面倒）。
      2. **能力層** = accept で `Conn::peer_creds().euid() == 自分の euid` を
         強制（`SO_PEERCRED`/`getpeereid`・interprocess が抽象化済）。Windows
         の peer-pid gate の双子で、これが本命。
      - **拡張 ACL は既定では使わない**: 「自分だけ」なら 0700 dir で足り
        ACL は冗長。跨アカウント共有（艦隊で特定サービスアカウント許可等）が
        要件化したときだけ導入 — その際 Linux=POSIX.1e(libacl)、macOS=NFSv4
        系 ACL で API が分岐する可搬性コストを織り込む。owner_only の一手として
        seam 内に閉じる。

## 将来構想

- [ ] **スクロールバック永続化（VSCode 型の再起動復元）＋保存時暗号化** —
      需要が立ったら着手する条件付き ToDo（殿裁定 2026-08-12）。現状の
      スクロールバックは純粋に RAM のみでディスクに触れず、Ptyxis/VTE の
      「Encrypted Scrollback Buffers」が守る脅威（プレーンテキストの
      ディスク退避を鑑識回収される）は**そもそも存在しない**。永続化を
      入れる日が来たら、その瞬間から暗号化保存（Windows は DPAPI 包み）が
      **前提条件**であって後付けオプションではない — プレーンテキストで
      一度でも書いたら負けの類。pagefile/休止ファイル経由の漏れは OS の
      領分（BitLocker・pagefile 暗号化）としてドキュメントで誘導する。

> **マイルストーン（殿見立て 2026-07-28）**: このペースなら遅くとも **2026年9月**
> に「ConPTY ボトルネック解消」へ着手する。下の 2 案は排他ではない。**忠実度が
> 要るのは WSL 側（tmux・エージェント群）だけ**なので、まず経路を二分して
> WSL 側だけ迂回し、Windows native は ConPTY のまま置くのが現実解。

- [ ] **ConPTY を通らない WSL 経路（本命）** — 2026-07-28 の実測で下地が揃った。
      WSL 内の中継エージェントへ socket で繋げば **Linux の本物の pty** に届く
      ので、conhost 起因の損失（APC 剥がし・kitty keyboard 不通・teardown 丸呑み・
      DECSLRM 剥がし）が **一括で消える**。sd の家老陣タブが ssh 経由で同じ
      忠実度を出している＝**達成可能性は実証済み**。常駐プロセスなので
      rikka daemon manager の最初の実用ユニットにもなる（命名・実装は保留中）。
      - **ssh → Windows sshd は逆効果**（同日調査）。`sshd.exe` 内に
        `CreatePseudoConsole` / `is_conpty_supported` があり、対話セッションでは
        sshd 自身が疑似コンソールを張る。しかも張るのが sshd 側なので**同梱
        OpenConsole を使わせられず、システム conhost に固定**される。加えて
        LogonUser トークン由来の権限差（ネットワークドライブ・DPAPI・
        プロファイル初期化）。常用経路にする理由なし。
      - fallback 実装 `ssh-shellhost.exe` も `ReadConsoleOutputW` /
        `WriteConsoleInputW` で**画面バッファを読んで再構成する**方式＝
        どちらの道もコンソールを経由する。

- [ ] **own OpenConsole（fork 保有）** — 殿意向 2026-07-16。conhost 起因の実害が
      累積しており（①APC 剥がし=ローカル kitty 画像不可・sixel 迂回中
      ②OSC 0 タブタイトル不達疑い ③kitty keyboard pop で終了 burst 丸呑み
      =今回の yazi 事件）、全て「端末側での回避」しかできていない。
      microsoft/terminal は MIT でフォーク可能・vendored ABI（HPCON 手挙げ）を
      自前ビルドで釘付けにできる利点もある。
      - **やるなら patch-set 方式**: upstream 素のソース＋小さなパッチ列＋CI
        ビルド（C++/MSVC toolchain が増えるのが最大コスト。upstream は高churn
        なので直 fork は追従地獄）。
      - **先にやる軽い手**: 今回の teardown 丸呑みは upstream へ issue 報告
        （gh トークン RO のため起票は殿。**英語下書き＋証跡は
        `UPSTREAM-BUGS.md` に保全済み**・APC 剥がしの stub も同居）。
        修正が取り込まれれば vendored 更新だけで済む。
      - **着手トリガー**: 端末側回避が不可能な要求が出た時。筆頭candidate=
        ローカル kitty graphics passthrough（conhost が APC を落とす限り
        こちら側では原理的に直せない）。
      - **上の WSL 経路が入ると射程が狭まる**: kitty graphics も含め、WSL 側の
        要求は迂回で片付く。fork の残る正当性は **Windows native シェル
        （pwsh/cmd）でも同じ忠実度が要る場合**に限られる。C++/MSVC toolchain
        と upstream 追従のコストを踏まえ、**WSL 経路を先に打つ**のが安い。

## 保守メモ

- **既定ターミナルが壊れたら、まず配備済み OpenConsole の CLSID を疑え**
  （2026-08-03 事件の本命）: ConPTY ペアを更新した際、配備先の
  `OpenConsole.exe` を NuGet 原本で上書きし、`install-default-terminal.ps1`
  手順 1b のブランド書き換え（WT の `{2EACA947-…}` → 我々の
  `{77F531BA-…}`）を消してしまった。委譲は解決せず Windows は**無言で
  conhost に戻る** — `rikka-handoff.exe` すら起動しないので handoff 側を
  いくら調べても出てこない。確認は
  `xxd -p …/OpenConsole.exe | tr -d '\n' | grep -c ba31f577bd46804eb0df8e45e1f7183b`
  （0 なら消えている）。復旧は手順 1b の再適用のみで足り、証明書も
  再登録も要らない。詳細は `assets/conpty/README.md` の更新手順に記載。
- **配備物は 4 つある。`rikka-handoff.exe` を置き去りにするな**（2026-08-03
  事件）: 既定ターミナルの handoff が壊れた。原因は MSIX ではない
  （登録済みマニフェストと `packaging/AppxManifest.xml` は 7/12 以降不変で
  完全一致）。`rikka-handoff.exe` だけが **7/22 のビルドのまま**で、IPC に
  認証を足した `a63a073`（7/24）より**古かった** — `auth` を持たないフレームを
  送り、端末が `PermissionDenied` で撥ねていた。
  - ビルドは `-p rikka-terminal` だけでは足りない。**必ず
    `-p rikka-terminal-windows-integration` も**（handoff の bin 名は
    crate 名と違うので見落としやすい）。
  - 配備スクリプトのハッシュ比較は「ビルドし直していない」を
    「最新」と誤読する。**古い出力と古い配備物は当然一致する。**
  - 疑うべき順序: ①handoff バイナリの日付 vs IPC 契約変更の日付
    ②`HKCU:\Console\%%Startup` の Delegation* が CLSID を指しているか
    ③マニフェスト差分。①で即決した。
- **ConPTY 越しに kitty keyboard を広告するな**（2026-07-16 yazi 事件）:
  `CSI ? u` に `?0u` を返すと TUI が push/pop を使い、OpenConsole 1.24 が
  終了 restore burst の途中から丸呑み → `?1049l` が届かず alt screen 残留。
  ~~そもそも client の push は conhost に食われて端末へ届かない（プロトコルは
  ConPTY 経由では機能しない）~~ ← **2026-07-28 に再現せず**。`RIKKA_PTY_DUMP`
  でホストの転送内容を直接見ると、`CSI ?u` / `CSI >1u`（push）/ `CSI =5;1u` /
  `CSI <u`（pop）/ `?1049h` / `?1049l` は **1.24.260710001 でも 1.25 preview
  でも全て端末まで届く**（各 0.4 秒間隔で個別送信）。単発クエリに応答が
  返らないのはこちらが広告を止めているためで、ホストが食っている証拠ではない。
  **ただし burst 条件は未検証** — 事件の実態は teardown の連続出力を途中から
  丸呑みされることで、そこは `alt_exit_probe`（要 yazi）でしか測れない。
  したがって kitty 無効化を外す根拠はまだ無いが、「原理的に不可能」でもない。
  恒久対処 = `mark_conpty()`（conpty reflow
  semantics + kitty keyboard 無効化を一元化）。証跡 probe = `alt_exit_probe`
  （fix変種=1049l 到達 / bug変種=丸呑み を対で記録）。wt が無事なのは
  wt 自身が `? u` に答えないから。SSH セッションは従来どおり広告する。
- 診断 probe（`#[ignore]`・`--nocapture` で実行）は残置:
  `width_semantics_probe` / `vertical_grow_probe` / `width_reflow_probe` /
  `conpty_resize_probe` / `alt_exit_probe`（要 yazi）。
  conhost の挙動疑義はまずこれで実測せよ。
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
