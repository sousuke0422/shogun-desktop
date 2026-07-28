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

The two screenshots below are the same script at the same font (Cascadia Mono
13pt) and the same window size. Both terminals feed their shell through a
modern ConPTY — RikkaTerminal through the `conpty.dll` + `OpenConsole.exe`
pair it ships, Windows Terminal through the one it bundles — so the
differences between them are engine behaviour. (That premise is not free on
Windows; see [On Windows the console host counts too](#on-windows-the-console-host-counts-too).)

## RikkaTerminal

![RikkaTerminal running the probe](probe-rikka-terminal.png)

## Windows Terminal

![Windows Terminal running the probe](probe-windows-terminal.png)

## Results

| # | Capability | Windows Terminal | RikkaTerminal |
|---|------------|------------------|---------------|
| 1 | SGR truecolor, semicolons | yes | yes |
| 2 | SGR truecolor, **colons** (`38:2:r:g:b`) | **no** | **yes** |
| 3 | Styled underlines (curly / dotted / dashed) | yes | yes |
| 4 | Underline colour (SGR 58) | yes | yes |
| 5 | dim / inverse / both | yes | yes |
| 6 | REP (`CSI b`) | yes | yes |
| 7 | ECH (`CSI X`) | yes | yes |
| 8 | ICH / DCH | yes | yes |
| 9 | Wide chars, combining marks, emoji | mark dropped from `ﾊﾟ` | composed |
| 10 | Box drawing and shade blocks | font glyphs | drawn as geometry |
| 11 | DECSCNM (`?5`) screen reverse | yes | yes |
| 12 | **DECSLRM left/right margins** | yes | **yes** |

## On Windows the console host counts too

Every terminal here drives the shell through ConPTY, so what reaches its
engine is whatever the console host chose to forward. **Which host** matters
as much as the engine. RikkaTerminal ships `conpty.dll` and `OpenConsole.exe`
beside the exe and drives that pair directly; Windows Terminal bundles its
own; everything else falls back to the copy in `C:\Windows\System32`.

That fallback is not equivalent. Below is the *same RikkaTerminal binary* —
the engine that produced the correct picture above — with the sideloaded pair
removed so it lands on the in-box conhost:

![RikkaTerminal forced onto the system conhost](probe-rikka-on-system-conhost.png)

The margin block comes out in the "wrong" shape: whole lines moved, `M01`/`M02`
scrolled away, every `B` still present. **The in-box conhost strips DECSLRM**,
so the fence never arrives and whether the engine implements it stops
mattering.

So a screenshot taken on the system conhost cannot tell you what that
terminal's engine supports. Two are kept below anyway, labelled as such,
because they show what the older host costs.

### Alacritty (upstream, system conhost)

![Alacritty running the probe](probe-alacritty.png)

RikkaTerminal's engine is a vendored fork of `alacritty_terminal`, and DECSLRM
is one of the local additions: upstream keeps no margin state, and its vte
dependency does not even hand `CSI s` to the terminal with its parameters. So
the margin row would fail on the engine's own merits here regardless of the
host. The other rows are the shared inheritance, and they match.

### Konsole for Windows (system conhost)

![Konsole for Windows running the probe](probe-konsole-via-conpty.png)

Konsole's own engine is not on trial here, for the same reason.

## Notes

Line 2 matters more than it looks: the `xterm-ghostty` entry spells `setrgbf`
with **colons**, so this is the form an application actually sends when it
believes it is talking to ghostty. Windows Terminal never claims that entry,
so nothing is broken there — but a terminal that does claim it has to accept
the colon form, and RikkaTerminal does.

Line 12 is the one that was missing until DECSLRM was implemented. tmux scrolls
a single pane by fencing the scroll into that pane's column band; a terminal
that ignores the fence scrolls the full width and drags the neighbouring panes
with it. Their content leaves the grid, and tmux — believing the terminal did
as it was told — never resends it, so panes that produce no further output stay
blank until a forced redraw. The probe's last block reproduces exactly that:
if margins are honoured only the `B`s vanish, and every `A` and `C` stays put.
