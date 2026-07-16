# microsoft/terminal へ報告する conhost バグ（証跡保全・issue 下書き）

rikka-terminal が回避済みの conhost/OpenConsole 側バグの一次証跡。起票は
gh トークンが RO のため手動（下書きは英語 issue 本文としてそのまま使える）。
再現 probe は `src/pty_local.rs::alt_exit_probe`（`#[ignore]`・要 yazi）。

---

## 1. Kitty keyboard protocol usage makes ConPTY swallow an exiting TUI's restore burst (`?1049l` never emitted)

**Status**: rikka 側は回避済み（`mark_conpty()` が kitty keyboard 広告を止める・
`e33ae3c`）。upstream 未報告。

### Draft issue body

**Environment**
- OpenConsole.exe / conpty.dll 1.24.2605.12001 (the pair shipped with
  Windows Terminal 1.24), hosted by a third-party terminal via
  `ConptyCreatePseudoConsole` (flags = 0)
- Windows 10 22H2 (19045)
- Client: interactive `pwsh` 7.6.2 hosting yazi 25.x (crossterm/ratatui)

**Summary**

If the hosting terminal answers the kitty keyboard protocol query
(`CSI ? u` → `CSI ? 0 u`), a TUI app (yazi) that then uses the protocol's
push/pop can no longer leave the alt screen cleanly: on quit, conhost
forwards/reconciles the mouse-mode disable but **swallows the rest of the
app's restore burst — `?1049l` (and `?25h`, DECSCUSR restore, `?1004l`,
`?2004l`) are never emitted to the terminal**, which stays stuck on the
alt screen while the shell prompt is written into it. The app exits
cleanly (exit code 0) and writes the full burst (verified against yazi's
`Raterm::stop`, which ends `… DisableMouseCapture, OSC 72 ×2, [<1u pop,
DECSCUSR, OSC 2, ?1004l, ?2004l, ?1049l, ?25h`).

Windows Terminal never answers `CSI ? u`, so the app takes its legacy
path and the bug is masked there. Note the client's `CSI > flags u`
push / `CSI < u` pop are also not forwarded to the terminal (expected,
conhost consumes what it doesn't model), so the protocol cannot actually
function through ConPTY — but the side effect above turns a harmless
advertisement into a stuck screen.

**Repro (dose-response)**

Host a ConPTY (80×24, flags 0), answer the startup DA1 with `\x1b[?6c`,
run interactive `pwsh -NoProfile -NoLogo`, type `yazi\r`, wait for
`?1049h`, then send `q` (retry a few times) and record everything conhost
writes to the output pipe. Vary ONE thing — whether the host answers the
forwarded `\x1b[?u` query with `\x1b[?0u`:

| host replies to `CSI ? u` | `?1049h` seen | `?1049l` seen |
|---|---|---|
| no reply (like Windows Terminal) | yes | **yes** — restore OK |
| also answering kitty graphics `a=q` (control) | yes | **yes** |
| `\x1b[?0u` | yes | **NO** — stuck on alt screen |

**Captured tail in the failing case** (raw bytes from the ConPTY output
pipe; the app has just quit): the last app-attributable bytes are the end
of its final synchronized-update frame `…\x1b[?25l\x1b[?2026l`, then
conhost's own reconciliation

```
\x1b[?1003;1006l \x1b]0;C:\Program Files\PowerShell\7\pwsh.exe\x1b\ \r\n PS C:\...>
```

— mouse-off is re-encoded, the title and prompt flow, but no `?1049l`
ever arrives (not even minutes later, with continued shell activity).

**Control experiments** (all forwarded correctly, ruling out the usual
suspects): a pure-VT `?1049h/2J/?1049l` round trip from pwsh; the same
yazi teardown byte sequence replayed synthetically (one Write per
sequence, abrupt process exit, crossterm-style raw console modes);
DA1 answer content (VT102 `?6c` vs a full WT-style parameter list);
conpty creation flags 0/1/2/3; slow vs fast output-pipe draining.
Only the `?0u` reply flips the outcome.

### 手元の再現コマンド

```
cargo test -p rikka-terminal alt_exit_probe -- --ignored --nocapture
```

fix 変種（無応答）= `1049l=true`、bug 変種（`?0u` 応答）= `1049l=false` が
毎回対で出る。生バイト全量も probe が dump する。

---

## 2. ConPTY strips APC (kitty graphics) but leaks the ST's trailing `\`

**Status**: 実測 2026-07-10（詳細は README「kitty graphics」節）。回避=ローカル
画像は sixel。upstream 未報告・下書き未作成。

クライアントが書いた `\x1b_G…\x1b\` の APC 本体が conpty 1.24 で剥がされ、
ST の `\` だけが素通しでテキストとして届く。passthrough するか、消すなら
ST ごと消すのが筋。issue 化する際は最小再現（printf 一発）を添える。
