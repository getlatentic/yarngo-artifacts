#!/usr/bin/env bash
# Capture just the app window for design comparison. Activates first so other
# windows do not sit on top, and captures only the app's own rectangle rather
# than the whole desktop.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
OUT="${1:-design/current/app.png}"
mkdir -p "$(dirname "$OUT")"
osascript -e 'tell application "yarngo studio" to activate' 2>/dev/null \
  || open -a "$(pwd)/target/debug/yarngo studio.app"
sleep 1.5
# origin(120,120) size 1180x820 from WindowOptions, plus the title bar.
screencapture -x -o -R "120,96,1180,850" "$OUT"
echo "$OUT"
