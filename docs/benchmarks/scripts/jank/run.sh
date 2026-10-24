#!/usr/bin/env bash
# One-command rerun of the browser jank benchmark (fork HEAD vs upstream cee1feb).
#   docs/benchmarks/scripts/jank/run.sh            # RUNS=3 SECONDS_=20 CPUS="1 4"
# Needs: cargo + wasm32-unknown-unknown, wasm-bindgen-cli 0.2.129, ffmpeg, python3, node,
# and Playwright's Chromium (npx playwright-core install chromium-headless-shell).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); REPO=$(cd "$HERE/../../../.." && pwd)
PF=${PORT_FORK:-18731}; PU=${PORT_UP:-18732}
UP_REV=${UP_REV:-cee1feb}; RUNS=${RUNS:-3}; SECS=${SECONDS_:-20}; CPUS=${CPUS:-"1 4"}
W=${WORK:-/tmp/fc-jank}; mkdir -p "$W"
command -v wasm-bindgen >/dev/null || cargo install wasm-bindgen-cli --version 0.2.129 --locked
(cd "$HERE" && npm install --silent)
# builds (separate target dirs; neither uses wasm-opt unless it is on PATH, so keep it off for parity)
if [ ! -d "$W/upstream-src" ]; then git -C "$REPO" worktree add -f "$W/upstream-src" "$UP_REV"; fi
(cd "$REPO" && CARGO_TARGET_DIR="$W/target-fork" cargo xtask web)
(cd "$W/upstream-src" && CARGO_TARGET_DIR="$W/target-up" cargo xtask web)
rm -rf "$W/fork" "$W/upstream"; cp -r "$W/target-fork/web/dist" "$W/fork"; cp -r "$W/target-up/web/dist" "$W/upstream"
# the same 1080p30 H.264 clip (25 s, moving test pattern) for both
[ -f "$W/clip1080.mp4" ] || ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=25 \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 -b:v 8M "$W/clip1080.mp4"
cp "$W/clip1080.mp4" "$W/fork/"; cp "$W/clip1080.mp4" "$W/upstream/"
python3 -m http.server $PF -b 127.0.0.1 -d "$W/fork" >/dev/null 2>&1 & P1=$!
python3 -m http.server $PU -b 127.0.0.1 -d "$W/upstream" >/dev/null 2>&1 & P2=$!
trap 'kill $P1 $P2 2>/dev/null' EXIT; sleep 1
OUT="$HERE/results"; mkdir -p "$OUT"
for c in $CPUS; do   # alternate fork/upstream so drift in the VM hits both
  node "$HERE/bench.mjs" --url http://127.0.0.1:$PF/ --label fork --cpu "$c" --runs "$RUNS" --seconds "$SECS" --out "$OUT/fork-cpu$c.json"
  node "$HERE/bench.mjs" --url http://127.0.0.1:$PU/ --label upstream --cpu "$c" --runs "$RUNS" --seconds "$SECS" --out "$OUT/upstream-cpu$c.json"
done
python3 "$HERE/table.py" "$OUT" | tee "$OUT/table.md"
