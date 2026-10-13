#!/usr/bin/env bash
# Runs INSIDE rt (launched as `-e wsl.exe bash replay-in-rt.sh ...`): replays
# a `script --log-timing` recording at its recorded pace, then keeps the tab
# as it is until anim-shift-run.ps1 closes the window.
#
# Usage: replay-in-rt.sh TIMING TYPESCRIPT [divisor=1]
set -euo pipefail
timing=${1:?timing file}
typescript=${2:?typescript file}
divisor=${3:-1}
sleep "${ANIM_SHIFT_PRE_SLEEP:-1}"
scriptreplay --log-timing "$timing" --log-out "$typescript" --divisor "$divisor"
sleep "${ANIM_SHIFT_POST_SLEEP:-600}"
