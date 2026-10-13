#!/usr/bin/env bash
# Records synth-anim.sh with `script --log-timing`, the same form a real
# recording takes, into OUT_DIR/timing and OUT_DIR/typescript.
#
# Usage: make-synth-recording.sh OUT_DIR [frames] [interval_s] [shift_at]
#
# Recordings stay out of git (see .gitignore here): a real one can carry
# tokens, paths and private text.
set -euo pipefail
out=${1:?usage: make-synth-recording.sh OUT_DIR [frames] [interval_s] [shift_at]}
shift
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
cmd="bash '$here/synth-anim.sh'"
for a in "$@"; do cmd+=" '$a'"; done
script -q -E never --log-timing "$out/timing" --log-out "$out/typescript" -c "$cmd" >/dev/null
echo "recorded: $out/timing $out/typescript"
