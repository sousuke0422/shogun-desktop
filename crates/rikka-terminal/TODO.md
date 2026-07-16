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
- [ ] フォント同梱（Cascadia 等）。

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
