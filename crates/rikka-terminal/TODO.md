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
      タブ帯ドロップ→タブ化を確認。残: 分割線ドラッグリサイズ・分割タブ
      の移送/複製・broadcast input・ズーム(単ペイン最大化)・クロス窓の
      ゾーン表示。
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
      実機でナビ切替確認。残: キーバインド/テーマの本格エディタ (v2)・
      フォント候補リスト。
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

## 保守メモ

- **ConPTY 越しに kitty keyboard を広告するな**（2026-07-16 yazi 事件）:
  `CSI ? u` に `?0u` を返すと TUI が push/pop を使い、OpenConsole 1.24 が
  終了 restore burst の途中から丸呑み → `?1049l` が届かず alt screen 残留。
  そもそも client の push は conhost に食われて端末へ届かない（プロトコルは
  ConPTY 経由では機能しない）。恒久対処 = `mark_conpty()`（conpty reflow
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
