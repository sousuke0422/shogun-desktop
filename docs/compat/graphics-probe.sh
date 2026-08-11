#!/usr/bin/env bash
# Graphics probe — sixel, kept SEPARATE from terminal-capability-probe.sh:
# the text probe's layout budget is exactly one 30-row window, and a raster
# block needs vertical room the text rows cannot give up.
#
#   bash graphics-probe.sh
#
# Kitty graphics is deliberately absent: behind ConPTY its detection loses
# the DA1 race no matter what the terminal does (see "Graphics protocols
# behind ConPTY" in README.md). Sixel is the one raster protocol a console
# host lets through, so it is the one this probe measures.
set -u
esc=$'\033'

clear
printf '%s\n\n' "graphics probe (sixel) — expect three solid bars: RED, GREEN, BLUE"

# Three 120x18px bars, one per sixel band row ('-' starts the next band).
printf '%sPq' "$esc"
printf '#0;2;100;0;0#1;2;0;100;0#2;2;0;0;100'
printf '#0!120~-#0!120~-#0!120~-'
printf '#1!120~-#1!120~-#1!120~-'
printf '#2!120~-#2!120~-#2!120~'
printf '%s\\' "$esc"

printf '\n\n%s\n' "no bars, or bars drawn as ~ characters = sixel not honoured"

# Host attestation, same as the text probe: the screenshot itself carries
# who sat between the shell and the terminal (see README "PATH-walk hazard").
ps_exe=$(command -v powershell.exe || true)
[ -n "$ps_exe" ] || ps_exe=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
if [ -x "$ps_exe" ]; then
    hosts=$("$ps_exe" -NoProfile -Command \
        'Get-Process OpenConsole -ErrorAction SilentlyContinue | ForEach-Object { $_.Path }' \
        2>/dev/null | tr -d '\r' | sort -u | paste -sd' ' -)
    printf 'live OpenConsole hosts: %s\n' "${hosts:-none (in-box conhost or non-Windows)}"
fi

sleep 3600
