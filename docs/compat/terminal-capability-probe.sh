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
printf '%s\n\n' "terminal capability probe — expected result is written on each line"

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
 9b combining U+309A      パ = ONE wide cell with the ring attached"
printf '%s\n' "10 box drawing / shades                     ╭─┬─╮ █▓▒░ ▁▂▃▄▅▆▇█"

# ── DECSCNM (?5): the visual bell. Held long enough to photograph. ──────────
printf '\n%s' "11 DECSCNM (?5) screen reverse — inverting for 2s"
printf '%s' "${esc}[?5h"
sleep 2
printf '%s' "${esc}[?5l"
printf '%s\n' "  … restored"

# ── DECSLRM: scroll ONLY a column band. ─────────────────────────────────────
printf '\n%s\n\n' "12 DECSLRM left/right margins — rows below scroll up by 2 INSIDE [] ONLY"
base=$(( $(tput lines) - 12 ))
for i in 1 2 3 4 5 6; do
    printf '%s%s\n' "${esc}[$((base + i - 1));1H${esc}[K" "M0$i AAAAAAAAAA[BBBBBBBBBB]CCCCCCCCCC"
done
# enable margins, fence to the [] band, scroll the six rows up by two
printf '%s' "${esc}[?69h${esc}[${base};$((base + 5))r${esc}[16;26s${esc}[2S${esc}[r${esc}[?69l"
printf '%s\n' "${esc}[$((base + 7));1H   correct: M05/M06 lose only the B's; every A and C stays put."
printf '%s\n' "   wrong  : whole lines moved, so M01-M02 are gone and A/C shifted too."

sleep 3600
