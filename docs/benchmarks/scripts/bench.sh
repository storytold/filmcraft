#!/usr/bin/env bash
# Runs each built variant N times, prints median/min/max ms per 1080p frame.
set -euo pipefail
N=${N:-7}; HERE=$(cd "$(dirname "$0")" && pwd)
run() { local tag=$1; shift; for i in $(seq $N); do "$@"; done | python3 "$HERE/stats.py" "$tag"; }
for L in up fork; do run "native-$L" /tmp/fcbench-$L/target/release/fcbench 20; done
for L in up-nosimd up fork; do run "wasm-$L" node --no-warnings "$HERE/wasi.mjs" /tmp/fcbench-$L/target/wasm32-wasip1/release/fcbench.wasm 10; done
