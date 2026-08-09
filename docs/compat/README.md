# Terminal capability comparison

RikkaTerminal answers to `TERM=xterm-ghostty` and reports itself as
`ghostty 1.3.1`, which is a promise: applications consult that terminfo entry
and send whatever it declares. This page is the evidence that the promise
holds, kept honest by a script anyone can re-run.

`terminal-capability-probe.sh` prints one line per capability, each judged by
**looking at the result** rather than by asking the terminal what it supports.
Every sequence it uses is one the `xterm-ghostty` entry declares.

```sh
bash docs/compat/terminal-capability-probe.sh
```

Captured on Windows 10 build 19045 — in-box `conhost.exe` 10.0.19041.4522,
sideloaded `OpenConsole.exe` 1.24.2607.10001 — with the shell reached through
`wsl.exe`.

## How these were captured, and where they fall short

Two things about this machine are worth stating up front, because both cost
us a wrong reading before they were understood.

**The font is not pinned across all three.** Windows Terminal and
RikkaTerminal are both Cascadia Mono 13pt. wezterm is **not**: on this machine
`wezterm.font('Cascadia Mono')` aborts the process inside DirectWrite —
`dwrote-0.11.0/src/font_family.rs:53: assertion failed: hr == 0` — and it does
so in the 2024 stable and the 2026 nightly alike, so it is the environment
rather than one build. The wezterm shots therefore use its default font at
11pt. Compare the capabilities, not the typography, and do not read anything
into glyph shapes in those two images.

**wezterm does not finish painting after the probe.** Line 11 restores from
DECSCNM with `?5l`, and everything printed before it can be left unpainted:
the first ten lines lose their foreground text while their backgrounds, the
red SGR 58 underline and the emoji are already on screen. Lines printed after
the toggle are fine.

The text is not lost. **Select the region and the glyphs are there** — the
cells are in wezterm's model and simply are not being drawn, and selecting
them forces the repaint that shows them. So a capability read from one of
these windows is still valid once it has been painted; what is at risk is
reading "missing" off a screenshot.

Update, 2026-08-10 re-run: the escape hatch above no longer opens. On this
machine the same build now sticks harder — real mouse selection by a human,
focus-free resize/reflow (even one that changed the row count), z-order
fronting, and background PostMessage selection all failed to revive the
region, on Windows and on the Linux AppImage alike; the Linux run froze
mid-DECSCNM and never painted the restore at all. The committed captures
below therefore show the deficit rather than hide it, and the glyph
verdicts were read from the mid-invert Linux frame, where every row is
painted (in reverse video) because nothing after `?5l` had happened yet.

How long it stays unpainted tracks the ConPTY generation, and not in the
direction one would guess:

| Terminal | ConPTY it drove | After `?5l` |
|---|---|---|
| wezterm nightly | its own 1.22.2502.04002 | repainted by ~30s |
| wezterm nightly | our 1.24.2607.10001 | **never repainted** — still blank when the window was closed |
| RikkaTerminal | 1.22.2502.04002 | complete within 10s |
| RikkaTerminal | 1.24.2607.10001 | complete within 10s |

Ours is unaffected on either host, so this is not "old hosts drop things" and
not the host alone — it is wezterm's own repainting. The **newer** host is
the worse one for it, and on that host the region never came back at all.

**It is not confined to ConPTY either.** The same build on Linux, through a
real pty under WSLg, has shown the identical deficit — unchanged across
captures at 22s, 23s and 69s, and repainted by itself only when looked at
again much later. A second Linux run with the same command painted correctly
from the start. A system font had been added in between, but that was not
isolated cleanly enough to blame it, so the trigger is not established.

The shape is the same on both platforms — lines printed before the `?5l` lose
their foreground while backgrounds and emoji stay, and anything that forces a
redraw brings the text back. What differs is whether it ever recovers on its
own: minutes on Linux, about thirty seconds on the older ConPTY, and never on
the newer one.

Treat it as a property of wezterm's rendering to work around when capturing,
not as a finding about any console host.

Practically: force a repaint, or take two captures minutes apart and compare
them, before publishing one. And do not read a delta between two captures as
correctness — a window that is stuck shows no change at all, which is exactly
how the 1.24 row above first got mistaken for "stable".

RikkaTerminal driven by the in-box conhost has also been seen to sit
incomplete for a while. That case is not explained here; it is a much older
host and may be a different mechanism.

Captures here are taken with `PrintWindow(PW_RENDERFULLCONTENT)` against a
background window, from a DPI-aware process. Never with synthetic keystrokes:
those go to whatever holds focus, which is not necessarily the window under
test (see the warning at the top of `e2e/rikka-sixel-local.ps1`).

## Windows Terminal

![Windows Terminal running the probe](probe-windows-terminal.png)

Captured at the default window size (30 rows). That took probe surgery: the
DECSLRM block used to sit at the bottom-anchored absolute row
`tput lines - 12`, which in a 30-row window lands on top of rows 10-12 —
an earlier committed capture had them silently overwritten. The probe now
asks the terminal where the cursor is (DSR `CSI 6n` — every host under test
answers it) and lays the block right after the content, scrolling only on
genuine shortfall. The host-attestation line moved to the very end of the
output for the same reason: the bottom is the one place layout padding can
never scroll it away.

## wezterm

![wezterm running the probe](probe-wezterm.png)

`wezterm-gui.exe 20240203-110809-5046fc22` — the current stable install on
this machine. Re-shot 2026-08-10 under the current probe: what is painted is
everything printed after the `?5l` restore — the DECSLRM block coming out
correct (its own 2024 host interprets margins) and the attestation line
naming that host — while rows 1-10 keep only their backgrounds and emoji,
the deficit described above in its no-longer-recoverable form. The emoji
that survive are themselves evidence: the ZWJ family is one glyph, the VS16
heart is emoji-presentation, the flag is drawn. The full glyph verdicts
(9b-9d columns in the table) were read from the Linux frame below.

## RikkaTerminal

![RikkaTerminal running the probe](probe-rikka-terminal.png)

## Results

| # | Capability | Windows Terminal | wezterm 20240203 | RikkaTerminal |
|---|------------|------------------|------------------|---------------|
| 1 | SGR truecolor, semicolons | yes | yes | yes |
| 2 | SGR truecolor, **colons** (`38:2:r:g:b`) | **no** | **no** (host) | **yes** |
| 3 | Styled underlines (curly / dotted / dashed) | yes | **no** (host) | yes |
| 4 | Underline colour (SGR 58) | yes | **no** (host) | yes |
| 5 | dim / inverse / both | yes | yes | yes |
| 6 | REP (`CSI b`) | yes | yes | yes |
| 7 | ECH (`CSI X`) | yes | yes | yes |
| 8 | ICH / DCH | yes | yes | yes |
| 9 | Wide chars, half-width pair `ﾊ`+`ﾟ`, emoji | yes (ring small and high) | side by side | side by side |
| 9b | Combining marks (`ハ`+U+309A → パ, `e`+U+0301 → é) | yes | yes | yes |
| 9c | Ambiguous width (`○×■│┐`) | narrow | narrow | narrow |
| 9d | Emoji ZWJ family | yes (one glyph, 2 cells) | yes | yes (fixed same day — see below) |
| 9d | Emoji VS16 (`❤`+FE0F) | yes | yes | yes |
| 9d | Emoji flag (RI pair 🇯🇵) | **letter fallback**, 4 cells | yes (flag drawn) | yes (flag drawn) |
| 10 | Box drawing and shade blocks | font glyphs | font glyphs | drawn as geometry |
| 11 | DECSCNM (`?5`) screen reverse | yes | yes | yes |
| 12 | DECSLRM left/right margins | yes | yes | yes |

Row 9 has a correction to own up to. The probe's original line was titled
"wide+combining+emoji" but sent `U+FF8A U+FF9F` — the HALF-WIDTH semi-voiced
mark, which is a *spacing* character. Nothing combining was ever tested, and
the verdicts written against it ("mark dropped" for Windows Terminal,
"composed" for the rest) judged a test that did not exist. The probe now
carries both cases explicitly: the spacing pair, whose correct rendering is
two narrow cells side by side, and a true combining mark (`ハ` + U+309A),
whose correct rendering is one wide cell with the ring attached. Re-run
under the corrected probe, Windows Terminal and RikkaTerminal both render
both correctly — including Windows Terminal's half-width ring, which is
drawn small and high and had been misread as dropped.

The mislabeled test earned its keep anyway. Treating two spacing characters
as if they combined is a real failure mode — rio 0.4.2 did exactly that,
stacking the ring over the `ﾊ` on both hosts (0.5.19 stopped stacking and
now drops true combining marks instead; see the rio aside) — so the pair
stays in the probe as
row 9 under an honest name, as a discriminator in its own right rather than
a stand-in for the combining test it never was.

Rows 9c and 9d judge **advance width**, not glyph shape: each run of glyphs
is chased by a `<` marker, and the marker lands wherever the cursor ended
up — read its column against the ruler line, which shares the same 30-column
ASCII prefix. On 9c both Windows Terminal and RikkaTerminal advance the five
East Asian Ambiguous characters narrow, one cell each, `<` at column 5 —
agreeing with each other and with their hosts, which is the actual pass
condition (a terminal drawing them wide while the host counts narrow is how
cursor positions drift). 9d is where the two split, in opposite directions:
Windows Terminal composes the ZWJ family into one glyph and honours VS16 but
renders the regional-indicator flag as letter fallback across four cells,
while RikkaTerminal drew the flag and honoured VS16 but broke the ZWJ
family into three separate faces. That fragmentation was fixed the same day
the row caught it, in the vendored engine's `Handler::input`: a char that
continues the previous cell's cluster — a pictograph after a ZWJ trailer, or
a skin-tone modifier after an emoji base — now stacks onto that cell as a
zero-width trailer instead of opening new cells, and the cursor advances by
the base width only. That is the model the sideloaded grapheme-segmenting
ConPTY host already uses, so stacking is also what keeps the grid agreeing
with the sender. ZWJ between letters (Arabic/Indic joining control) is
excluded and still opens a fresh cell. A second same-day fix rides on the
same mechanism: VS16 now promotes a narrow pictograph cell to wide — ❤ is
width 1 by wcwidth, so ❤️ was drawn as a 2-cell-class glyph over a 1-cell
advance and dug into its neighbour (殿 caught it overlapping the < marker
in the committed capture). The renderer needed no change — it
already shaped base+trailer cells as single clusters. The committed capture
is the post-fix run; SGR 58 stayed at 143 px, confirming nothing else moved.
Windows Terminal's flag fallback stands, which is the point of carrying the
hard cases: 😀 alone, the previous emoji test, passes everywhere and
distinguishes nothing.

The probe also prints a `live OpenConsole hosts:` line — every OpenConsole
process alive at probe time, stamped into the screenshot itself. That line
earned its place on its very first run: it listed
`C:\Program Files\WezTerm\OpenConsole.exe` under a RikkaTerminal capture,
because the test copy of the exe had `OpenConsole.exe` beside it but not
`conpty.dll` — so `LoadLibrary` walked PATH, found WezTerm's dll, and that
dll started WezTerm's 2024-era host, which stripped every colon-form SGR:
the SGR 58 pixel count collapsed from 143 to **zero**. Completing the pair
beside the exe and launching with a minimal PATH (`launch-clean.ps1`, in
this directory) restored 143 exactly. PATH hygiene cannot live inside the
probe script — the host is chosen at terminal launch, before bash ever runs
— so the launcher sanitizes and the probe attests.

Rows 1, 2 and 4 are colour claims, so they were checked by sampling pixels
rather than by eye. The SGR 58 underline is the clearest: the probe asks for
`58:2::255:80:80`, and a row of exactly `(255, 80, 80)` runs the width of the
words `red-underline` — 182 px in Windows Terminal, **zero** in wezterm
20240203.

**The wezterm column is not measuring wezterm.** Rows 2, 3 and 4 are all
colon-separated SGR sub-parameters, and the obvious reading is that its
parser does not handle the colon syntax. That reading is wrong. Run the
**same build** on Linux, where ConPTY is not in the path at all, and all
three pass:

| | rows 2 / 3 | SGR 58 red pixels |
|---|---|---|
| wezterm 20240203, Windows, its own 2024-era ConPTY | fail | **0** |
| wezterm 20240203, Linux (AppImage, real pty) | pass | **117** |

Same binary, same probe. What differs is the console host, so the strip
belongs to the 2024-era ConPTY pair wezterm bundles — not to its engine. A
Windows user does see the failures, which is why the row stays in the table,
but nothing here says anything about wezterm's own VT parsing. The screenshot
behind those numbers is in [an aside](#wezterm-on-linux-the-colon-form-control),
kept out of the way because it cannot be read as a full capability run.

That also joins up with the section below: the in-box conhost drops **the
same three things**. Stripping colon-form SGR sub-parameters looks like a
trait of older ConPTY hosts generally, fixed somewhere between the pair
wezterm bundles and 1.22.2502.04002.

## On Windows the console host counts too

Every terminal here drives the shell through ConPTY, so what reaches its
engine is whatever the console host chose to forward. **Which host** matters
as much as the engine. RikkaTerminal ships `conpty.dll` and `OpenConsole.exe`
beside the exe and drives that pair directly; Windows Terminal bundles its
own; wezterm bundles its own.

For the rest, "falls back to the in-box conhost" is only the end of the
story. Terminals in this family (alacritty, rio) load the sideload DLL with
a bare `LoadLibrary("conpty.dll")`, and when the exe's own directory has
none, **that search walks `PATH`**. On this machine WezTerm's install dir is
on `PATH`, so today a stock alacritty and a stock rio both come up hosted by
`C:\Program Files\WezTerm\OpenConsole.exe` — spotted in Task Manager, then
confirmed from the process tree. Installing one terminal silently changed
which console host every other sideload-capable terminal runs on. Only when
no `conpty.dll` is findable anywhere does the in-box conhost actually
apply. (The alacritty capture below predates WezTerm's installation on this
machine, so its in-box attribution held at the time it was taken; it would
not reproduce today without scrubbing `PATH`.)

That fallback is not equivalent. Below is the *same RikkaTerminal binary* —
the engine that produced the correct picture above — with the sideloaded pair
removed so it lands on the in-box conhost:

![RikkaTerminal forced onto the system conhost](probe-rikka-on-system-conhost.png)

Three things are gone, and the same pixel sampling shows it:

- **DECSLRM.** The margin block comes out in the "wrong" shape the probe
  names: whole lines moved, `M01`/`M02` scrolled away, every `B` still
  present. The fence never arrives, so whether the engine implements the
  sequence stops mattering.
- **Colon-form truecolor.** Line 2 loses its colour entirely. The coloured
  band is one text row tall instead of two, with about half the coloured
  pixels (362 orange / 2073 blue, against 636 / 3922 through the sideloaded
  pair).
- **Underline colour.** Not one pixel of `(255, 80, 80)` survives, against
  143 through the sideloaded pair.

That last pair is worse than it looks for a terminal in our position. The
`xterm-ghostty` entry spells `setrgbf` with **colons**, so the colon form is
what an application actually sends once it believes the terminfo. On the
in-box conhost a terminal claiming that entry **cannot honour its own
terminfo**, however correct its engine is.

**This is one host build, not a rule about old hosts.** It is tempting to
read "in-box conhost is old, old hosts drop things" — but the measurements do
not support the general claim. Worse, "survives" turned out to be two
different things wearing one word. Rio is the instrument that splits them:
its engine has **no margin support at all** (proven on our pass-through host,
where its margin block comes out wrong), so whatever correctness appears on
another host was manufactured *by that host*. The same rio binary, three
hosts:

| Host | Origin | DECSLRM strategy | rio's margin block |
|---|---|---|---|
| conhost 10.0.19041.4522 | in-box, Windows 10 19045 | **stripped** | wrong |
| ConPTY 2024-era | wezterm 20240203 | **applied by the host** — margins are fenced in the host's own buffer and the re-emitted output is already correct | **correct**, engine notwithstanding |
| ConPTY 1.24.2607.10001 | RikkaTerminal / Windows Terminal | **forwarded** — the sequence reaches the terminal, and the engine must implement it | wrong |

Three generations, three strategies: strip, interpret, forward. The earlier
version of this table said "forwarded" for all of them — wezterm could not
expose the difference, because its engine implements margins either way; a
terminal that cannot is what makes the host's own hand visible.

Two consequences worth spelling out. On a modern, forwarding host,
engine-side DECSLRM is *mandatory* — which is exactly why the karo-pane wipe
needed an engine fix and not a host swap. And an old interpreting host can
make a margin-less terminal LOOK margin-capable: a capability screenshot
taken through it says nothing about the engine.

It is still why the sideloaded pair is not a developer convenience. A host of
this vintage is not an artefact of one stale desktop: Server releases, the
LTSC and IoT LTSC channels, and machines carried on extended security updates
all keep the console host they shipped with, for support horizons measured in
years. Carrying `conpty.dll` + `OpenConsole.exe` makes the host a known
quantity instead of an environment variable.

## The terminal we claim to be

The page opens by calling `TERM=xterm-ghostty` a promise. The control for that
is ghostty itself, answering the same probe:

![ghostty running the probe](probe-ghostty.png)

**Twelve of twelve**, with the SGR 58 underline in exact `(255, 80, 80)` —
the same reading RikkaTerminal gives. So the entry we claim is one the
reference implementation honours in full, and claiming it does not overstate
what we do.

Re-shot 2026-08-10 as ghostty 1.3.1 (`1.3.1~ppa2-resolute1`, the very
version RikkaTerminal's XTVERSION self-report names) under the current
probe, through WSLg — a real Linux pty, no console host anywhere in the
path, which the attestation line corroborates by listing no OpenConsole of
its own. The glyph rows all pass: the spacing pair sits side by side with
the ring drawn small and high, パ and é compose into single cells,
ambiguous advances narrow, and row 9d is a clean sweep — ZWJ family as one
glyph, VS16 heart in emoji presentation, the flag actually drawn. Three terminals
on this page pass 9d whole — ghostty, RikkaTerminal, and wezterm on a
Linux pty — which is the right shape for a terminal and the terminfo entry
it answers to. (An earlier revision of this paragraph praised ghostty for
"keeping the mark" on the row-9 pair — that was the mislabeled combining
test; see the row 9 correction above.)

Which leaves a conclusion the 2026-08 re-measurement tour earned rather
than asserted: **within what this probe measures, RikkaTerminal is the
only Windows-native terminal that passes everything** — all twelve
sequence rows and every glyph row. The other two full glyph sweeps happen
on a Linux pty; on Windows, Windows Terminal strips colon-form SGR and
falls back on flags, Konsole inherits the in-box conhost's failures,
wezterm's host kills rows 2-4 behind an unrecoverable repaint deficit, and
rio and alacritty fail margins and width handling in their engines. Two
scope caveats keep the sentence honest: the probe measures these seventeen
items and nothing else, and the DECSLRM and DCS rows are earned with the
sideloaded pair in place — a controlled host is part of the design, not a
neutral condition.

Details worth knowing before re-running it:

- This is ghostty `1.3.1~ppa2-resolute1` from the `mkasberg/ghostty-ubuntu`
  PPA (its `--version` banner says `1.3.0-dev` — trust dpkg, not the
  banner), run under WSLg. We report ourselves as `ghostty 1.3.1`.
- The **AppImage build will not start here**: ghostty requires OpenGL 4.3,
  WSLg's d3d12 path offers 3.3, and forcing `GALLIUM_DRIVER=llvmpipe` fails
  earlier still with "Could not initialize EGL display". The PPA build runs.
  A ghostty process being alive is not evidence it rendered — check the
  window, which shows "Oh, no. Unable to acquire an OpenGL context" when it
  did not.
- WSL here ships no CJK fonts, so row 9 came out as replacement glyphs until
  one was added. No root needed: copy a font from `/mnt/c/Windows/Fonts`
  (`msgothic.ttc` was used) into `~/.local/share/fonts` and run `fc-cache -f`.
- The DECSCNM label on line 11 comes out truncated in these captures, and
  differently between runs. Not investigated, and not claimed as ghostty
  behaviour — it looks like the capture catching a repaint. Nothing else on
  the line depends on it.

## Asides

### wezterm nightly

![wezterm nightly running the probe](probe-wezterm-nightly.png)

`wezterm-gui.exe 20260716-195552-76b606ec` passes **all twelve**, including
the three colon-form rows the 2024 stable fails. That is the honest answer to
"does wezterm support this": the current code does, the shipping stable does
not.

It sits in an aside rather than the main table because the stable build is
what one gets by default — not because the nightly is hard to come by. There
is an installer for it, and it is carried by winget and by scoop's `versions`
bucket.

Two things keep people on the stable build anyway, and neither is difficulty.
It is absent from the GitHub releases page, so the download comes from the
project's own site — a detour rather than an obstacle. And it is called
*nightly*, which reads as "may be broken today" regardless of whether it is;
a label like that turns people around before they ever look at what is in the
build. Between the detour and the word, someone not already looking for it
goes back — which is why the stable build is the one the main table measures,
even though the code has moved on.

Worth recording that it does so through ConPTY **1.22.2502.04002**, an older
host than the 1.24.2607.10001 we carry.

### wezterm on Linux (the colon-form control)

![wezterm 20240203 on Linux](probe-wezterm-linux.png)

The `20240203-110809-5046fc22` AppImage — the same build as the Windows
stable — run under WSLg, so the shell is reached through a real pty and no
console host is involved. Rows 2, 3 and 4 pass here and fail on Windows,
which is what pins those failures on the bundled ConPTY rather than on
wezterm.

The committed frame is **mid-DECSCNM**, deliberately: the 2026-08-10 run
froze at the `?5` invert and never painted the restore (the escalated form
of the repaint deficit described at the top of this page), so the one
moment every row is on screen is during the invert — reverse video, cursor
parked on row 11. An earlier revision of this section warned that the
AppImage container had no CJK fonts; that is no longer the case, and the
glyph rows are all readable and real. This frame is where the wezterm
column's 9b-9d verdicts come from, and they are a clean sweep: パ and é
compose, ambiguous advances narrow, the ZWJ family is one glyph, the VS16
heart is emoji-presentation, the flag is drawn. A 2024 engine passes every
glyph row on a real pty — width handling was never wezterm's problem
either; on Windows the deficit and the host sit in front of it.

### Alacritty upstream — and what our fork actually adds

RikkaTerminal's engine is a vendored fork of `alacritty_terminal`, so upstream
Alacritty is the closest thing to a control for our own engine. It is worth
unmasking, and it can be: Alacritty loads a sideloaded `conpty.dll` when one
sits beside the exe and falls back to the system API otherwise
(`tty/windows/conpty.rs`). The install ships no pair, so by default it runs on
the in-box conhost — but drop ours next to a copy of `alacritty.exe` and the
engine comes out from behind the host.

Same binary, same window, two hosts:

| | rows 2 / 3 | SGR 58 red px | row 12 DECSLRM |
|---|---|---|---|
| in-box conhost 10.0.19041.4522 | fail | **0** | fail |
| our `OpenConsole.exe` 1.24.2607.10001 | pass | **312** | **still fails** |

![Alacritty on a modern ConPTY pair](probe-alacritty-modern-host.png)

On a modern host upstream passes **eleven of twelve**. The colon-form rows
were never its fault, exactly as with wezterm. What does not come back is
DECSLRM, and that one is the engine's own: upstream keeps no margin state,
and its `vte` dependency does not even hand `CSI s` to the terminal with its
parameters. Both are local additions here — which makes this screenshot the
honest measure of what the fork contributes, rather than the assertion that
used to stand in this section.

Re-shot 2026-08-10 as Alacritty 0.17.0 under the current probe (capture
above; attestation line shows our Temp pair alive, minimal-PATH launch).
The sequence verdicts are unchanged — 312 px of exact `(255, 80, 80)`
again, DECSLRM still the whole-line failure — and the new glyph rows turn
the control comparison pointed, because upstream exhibits precisely the two
defects just fixed in our vendored fork: the ZWJ family fragments into
three faces, and combining marks take a **spacing cell of their own** — パ
renders as `ﾊ ﾟ` with a gap before the next character, é eats the space
that followed it. The rest: ambiguous advances narrow, the VS16 heart stays
a monochrome outline, the flag falls back to `JP` letters, and emoji render
greyscale throughout (no colour-emoji fallback on Windows).

For contrast, the same binary on the in-box conhost, which is what a user
gets by default:

![Alacritty running the probe](probe-alacritty.png)

### Rio (0.4.2 alpha, unmasked on our pair — superseded by 0.5.19 below)

![Rio running the probe on our ConPTY pair](probe-rio-modern-host.png)

Rio's PTY layer does the same `LoadLibrary("conpty.dll")` dance as alacritty,
ships no pair — and on this machine therefore comes up on **WezTerm's**
2024-era host via the `PATH` walk described above. Dropped next to our
1.24.2607 pair instead (child process verified), its engine comes out from
behind the host: **eleven of twelve**, including the colon-form SGR rows
(689 orange px across both truecolor lines, 312 px of exact `(255, 80, 80)`
under `red-underline`).

The one failure is DECSLRM, in the same "wrong" shape as upstream alacritty —
which is no coincidence: rio's parser (`copa`) is a vte fork, and like
upstream it keeps no margin state. The probe also shows two alpha rough
edges that are rio's own: the `12 DECSLRM` header line lands somewhere else
entirely (cursor-addressing disagreement about the grid height), and the
window title renders its literal `{{ TITLE || PROGRAM }}` template.

The stock install, on WezTerm's host via the `PATH` walk, is the mirror
image — and the measurement that split the generation table above:

![Rio on WezTerm's 2024 host](probe-rio-wezterm-host.png)

Colon-form SGR dies (orange in row 1 only, zero red pixels) exactly as that
host always strips it, yet the margin block comes out **correct** — the 2024
host applied DECSLRM itself and handed rio pre-fenced output. Same binary,
opposite verdicts on rows 2/3/4 and row 12, purely from which host sat in
between.

One quirk is rio's own and host-independent: the half-width pair `ﾊ`+`ﾟ`
(two spacing characters, two cells) comes out with the ring **stacked on top
of the `ﾊ`** as though it were a combining mark, on both hosts alike.

Capture notes: `-e` and every custom `[shell]` config exit instantly in the
0.4.2 build, so both probes were typed into the default shell by hand; the
captures are `PrintWindow` from behind, focus untouched.

### Rio 0.5.19, re-measured under the current probe

![Rio 0.5.19 running the probe on our ConPTY pair](probe-rio-0519.png)

Re-shot 2026-08-10 after the installed rio moved to 0.5.19. `-e` works now,
so this capture is automated like the rest: minimal-PATH launch, our pair
beside the exe, and the attestation line at the bottom showing the Temp
`OpenConsole.exe` alive — the PATH-walk unmasking of 0.4.2 can no longer
happen silently.

What moved, in both directions:

- **The spacing-pair stacking is gone.** `ﾊ`+`ﾟ` now comes out side by
  side, ring small and high, like Windows Terminal — the quirk that
  justified keeping row 9 as a discriminator no longer fires on rio.
- **True combining marks are now dropped entirely.** Row 9b renders パ as a
  bare ハ and é as a bare e — the ring and the acute simply vanish. 0.4.2
  stacked things that shouldn't combine; 0.5.19 discards things that
  should. Opposite failure, same row.
- **SGR 58 shifted colour.** 0.4.2 put 312 px of exact `(255, 80, 80)`
  under `red-underline`; 0.5.19 draws the same 312 px at `(236, 94, 87)`.
  The capability is still honoured — the red underline is there — but a
  colour pipeline change moved the value, and an exact-match pixel gate now
  reads **zero**. Judge rio's row 4 by eye, not by the exact-value count.
- **The misplaced `12 DECSLRM` header is fixed**; the header sits where it
  should. The window title still renders its literal
  `{{ TITLE || PROGRAM }}` template.
- **DECSLRM still fails** in the same whole-line shape — M01/M02 gone, B
  bands intact — as expected from a margin-less vte-fork parser.
- Rows 9c/9d, first run on rio: ambiguous advances narrow (agreeing with
  the host); the VS16 heart is emoji-presentation and snug; the ZWJ family
  fragments into three faces; the flag falls back to `JP` letters.

### Konsole for Windows (system conhost)

![Konsole for Windows running the probe](probe-konsole-via-conpty.png)

Re-shot 2026-08-09 under the current probe, launched with a minimal PATH.
The attestation line at the bottom settles what the old caption could only
assume: no Konsole-owned OpenConsole is alive — this build brings no pair of
its own, and with the PATH walk blocked it runs on the **in-box conhost**,
the strip-generation host. Every sequence-level failure on screen belongs to
that host, not to Konsole's engine: colon-form SGR dies (zero SGR 58 red
pixels), styled underlines flatten, and DECSLRM is ignored — M01/M02 vanish
whole and the B bands survive, the exact full-width scroll that wipes tmux
panes.

The glyph rows ARE Konsole's own, though — rendering happens in the
terminal regardless of which host relays the bytes. Its engine composes the
ZWJ family into one glyph, combines パ and é correctly, and advances the
ambiguous set narrow; the VS16 heart stays in text presentation (a narrow
monochrome outline), and the flag falls back to `JP` letters.

## Notes

Line 12 is the one that was missing until DECSLRM was implemented. tmux
scrolls a single pane by fencing the scroll into that pane's column band; a
terminal that ignores the fence scrolls the full width and drags the
neighbouring panes with it. Their content leaves the grid, and tmux —
believing the terminal did as it was told — never resends it, so panes that
produce no further output stay blank until a forced redraw. The probe's last
block reproduces exactly that: if margins are honoured only the `B`s vanish,
and every `A` and `C` stays put.
