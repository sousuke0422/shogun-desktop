#!/usr/bin/env bash
# Terminal capability probe — the script behind docs/compat/README.md.
#
# Prints one line per capability, each of which is judged by LOOKING at the
# result rather than by asking the terminal. Run it in any two terminals at
# the same size and font and the screenshots are directly comparable.
#
#   bash terminal-capability-probe.sh
#
# Every sequence used here is one the `xterm-ghostty` terminfo entry declares,
# so a terminal claiming that entry is expected to answer all of them.
set -u
esc=$'\033'

clear
# DECRQM report card, printed on the title row (the 30-row budget is full).
# Reply: CSI ? Pm ; Ps $ y — Ps 1/2=set/reset (recognised), 0=not recognised.
# Query goes out via printf, NOT read -p (see the DSR lesson below).
# rqm runs inside $(...): its stdout is CAPTURED, so the query must go to
# /dev/tty explicitly — a bare printf here embeds the escape into the title
# string instead of asking the terminal, and every answer arrives too late,
# echoing as stray "^[[?2004;2$y" text over the rows (measured, once).
rqm() {
    printf '%s[?%s$p' "$esc" "$1" > /dev/tty
    local m ps
    IFS='[;$' read -rsd y -t 1 _ m ps < /dev/tty 2>/dev/null || true
    printf '%s' "${ps:-?}"
}
printf '%s\n' "terminal capability probe — every line states its expected result | DECRQM paste2004=$(rqm 2004) sync2026=$(rqm 2026) grapheme2027=$(rqm 2027)"

printf '%s\n' " 1 SGR truecolor, semicolons   ${esc}[38;2;255;120;0mORANGE${esc}[39m ${esc}[48;2;0;90;180m BLUE-BG ${esc}[49m   (both should be coloured)"
printf '%s\n' " 2 SGR truecolor, colons       ${esc}[38:2:255:120:0mORANGE${esc}[39m ${esc}[48:2:0:90:180m BLUE-BG ${esc}[49m   (terminfo setrgbf uses THIS form)"
printf '%s\n' " 3 styled underlines           ${esc}[4:3mcurly${esc}[4:0m ${esc}[4:4mdotted${esc}[4:0m ${esc}[4:5mdashed${esc}[4:0m        (three DIFFERENT underlines)"
printf '%s\n' " 4 underline colour (SGR 58)   ${esc}[4m${esc}[58:2::255:80:80mred-underline${esc}[59m${esc}[24m        (red line under grey text)"
printf '%s\n' " 5 dim / inverse / both        ${esc}[2mdim${esc}[22m ${esc}[7minverse${esc}[27m ${esc}[2;7mdim+inverse${esc}[0m"
# These three move the cursor backwards, so their expectation is stated
# BEFORE the sequence runs — text printed after it would land on top of the
# result and make the screenshot unreadable.
printf '%s\n' " 6 REP (CSI b)        expect ten X         X${esc}[9b"
printf '%s\n' " 7 ECH (CSI X)        expect five A        AAAAAAAAAA${esc}[10D${esc}[5X"
printf '%s\n' " 8 ICH / DCH          expect 3 blanks+CDEF ABCDEF${esc}[6D${esc}[3@${esc}[3C${esc}[2P"
printf '%s\n' " 9 wide+emoji+halfwidth  |日本語|abcd|😀| + spacing pair ﾊﾟ = TWO cells
 9b marks + IVS    パ(ハ+309A) é(e+301) 葛󠄀≠葛󠄁(845B+E0100/1) = ONE cell each"
# 9c/9d judge ADVANCE WIDTH, not glyph shape: the < marker lands wherever the
# cursor ended up, and the ruler shares the same 30-column ASCII prefix.
printf '%s\n' " 9c ambiguous width           0123456789          <- ruler (col 0 under the 0)"
printf '%s\n' "                              ○×■│┐<              narrow: < at col 5 / wide: < at col 10"
printf '%s\n' " 9d emoji hard cases   👨‍👩‍👧< ❤️< 🇯🇵< 👍🏽< ❤️‍🔥< 1️⃣< 🏴󠁧󠁢󠁳󠁣󠁴󠁿< 🩷<  each ONE glyph, < snug"
printf '%s\n' "10 box drawing / shades                     ╭─┬─╮ █▓▒░ ▁▂▃▄▅▆▇█"

# ── DECSCNM (?5): the visual bell. Held long enough to photograph. ──────────
printf '%s' "11 DECSCNM (?5) screen reverse — inverting for 2s"
printf '%s' "${esc}[?5h"
sleep 2
printf '%s' "${esc}[?5l"
printf '%s\n' "  … restored"

# ── DECSLRM: scroll ONLY a column band. ─────────────────────────────────────
printf '%s\n' "12 DECSLRM left/right margins — rows below scroll up by 2 INSIDE [] ONLY"

# Place the block right after the content, NOT at a bottom-anchored absolute
# row: `lines - 12` lands on top of rows 10-12 in a 30-row window (Windows
# Terminal's default), which silently overwrote them in an earlier committed
# capture. Ask the terminal where the cursor is (DSR — every host under test
# answers it), and scroll only the shortfall if the block wouldn't fit.
lines=$(tput lines)
row=''
# Drain stragglers first: a DECRQM reply that arrived after its 1s window
# would otherwise sit in the input buffer and corrupt the DSR parse below.
while IFS= read -rs -t 0.05 -N 64 _; do :; done
# The query goes out via printf, NOT via read -p: -p writes its prompt to
# stderr, so any stderr redirect on the read silently swallows the query and
# every host looks mute (cost one wrong "WT doesn't answer DSR" conclusion).
printf '%s[6n' "$esc"
IFS='[;' read -rsd R -t 2 _ row _ 2>/dev/null || true
if ! [ "$row" -ge 1 ] 2>/dev/null; then
    row=$(( lines - 12 ))          # DSR unanswered — old bottom-anchored layout
fi
need=12                            # blank + 6 M rows + blank + 2 captions + hosts (wraps to 2)
if [ $(( row + need )) -gt "$lines" ]; then
    # Newlines only scroll once the cursor reaches the bottom row, so the
    # ride down (lines - row) must be paid IN ADDITION to the scroll amount
    # (row + need - lines) — together that is always exactly `need` newlines.
    # Padding only the shortfall moved the cursor without scrolling anything,
    # and the block overwrote the row-12 header it was meant to protect.
    for _ in $(seq 1 "$need"); do printf '\n'; done
    row=$(( lines - need ))
fi
base=$(( row + 1 ))
for i in 1 2 3 4 5 6; do
    printf '%s%s\n' "${esc}[$((base + i - 1));1H${esc}[K" "M0$i AAAAAAAAAA[BBBBBBBBBB]CCCCCCCCCC"
done
# enable margins, fence to the [] band, scroll the six rows up by two
printf '%s' "${esc}[?69h${esc}[${base};$((base + 5))r${esc}[16;26s${esc}[2S${esc}[r${esc}[?69l"
printf '%s\n' "${esc}[$((base + 7));1H   correct: M05/M06 lose only the B's; every A and C stays put."
printf '%s\n' "   wrong  : whole lines moved, so M01-M02 are gone and A/C shifted too."

# Which console hosts are alive now that every test has run, stamped into the
# screenshot itself: LoadLibrary walks PATH and silently re-hosts terminals
# (see README), and this line is how a capture proves it wasn't re-hosted.
# Printed LAST because the layout padding scrolls the top of the screen away
# in short windows — the bottom is the one place nothing can evict it from.
# command -v is not enough for the interop lookup: a terminal launched by app
# activation (wt.exe alias) does not inherit the caller's environment, and
# the session's PATH may lack the PowerShell dir — hence the fixed fallback.
ps_exe=$(command -v powershell.exe || true)
[ -n "$ps_exe" ] || ps_exe=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
if [ -x "$ps_exe" ]; then
    hosts=$("$ps_exe" -NoProfile -Command \
        'Get-Process OpenConsole -ErrorAction SilentlyContinue | ForEach-Object { $_.Path }' \
        2>/dev/null | tr -d '\r' | sort -u | paste -sd' ' -)
    printf 'live OpenConsole hosts: %s\n' "${hosts:-none (in-box conhost or non-Windows)}"
fi

sleep 3600
