#!/usr/bin/env bash
# Synthetic stand-in for an agent TUI's status animation, for anim-shift-probe.
# It runs nothing and reads nothing: a braille spinner and a two-line status
# block redrawn in place with \r, erase-line and cursor-up, the way Codex /
# Claude Code repaint their "Working" line.
#
# Usage: synth-anim.sh [frames=80] [interval_s=0.08] [shift_at=0]
#   shift_at > 0: at that frame the block moves down one row (a deliberate
#   drift, so a live run has a positive control). 0 = never moves.
set -euo pipefail
frames=${1:-80}
interval=${2:-0.08}
shift_at=${3:-0}
spin=(⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏)

draw() {
    local i=$1
    printf '\r\033[2K%s Working (%ds - esc to interrupt)\n' "${spin[i % 10]}" $((i / 12))
    printf '\r\033[2K  L reading files... %d/%d' "$i" "$frames"
    printf '\033[1A\r' # back to the first line of the block
}

printf '\033[?25l'   # hide the cursor, as agent TUIs do while animating
printf '\033[2J\033[H'
printf 'synthetic agent session (not a real recording)\n\n'
printf '> summarise the build log\n\n'
draw 0
sleep 4 # hold the first frame so the capture can start on it
for ((i = 1; i < frames; i++)); do
    if ((shift_at > 0 && i == shift_at)); then
        printf '\r\033[2K\n' # the block now starts one row lower
    fi
    draw "$i"
    sleep "$interval"
done
printf '\033[2B\r\n\033[?25h'
