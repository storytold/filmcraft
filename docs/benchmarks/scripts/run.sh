#!/usr/bin/env bash
# Usage: [WASM_RUSTFLAGS=...] run.sh <checkout-dir> <label>  (default WASM_RUSTFLAGS="-C target-feature=+simd128"; set to "" for the upstream default build)
set -euo pipefail
SRC=$(realpath "$1"); L=$2; HERE=$(cd "$(dirname "$0")" && pwd); B=/tmp/fcbench-$L
mkdir -p $B/src && cp "$HERE/main.rs" $B/src/main.rs
cat > $B/Cargo.toml <<T
[package]
name = "fcbench"
version = "0.0.0"
edition = "2024"
[dependencies]
filmcraft-frame = { path = "$SRC/crates/frame" }
filmcraft-color = { path = "$SRC/crates/color" }
filmcraft-time = { path = "$SRC/crates/time" }
[profile.release]
debug = false
[workspace]
T
cp "$SRC/Cargo.lock" $B/Cargo.lock 2>/dev/null || true
cd $B
cargo build -q --release
RUSTFLAGS="${WASM_RUSTFLAGS--C target-feature=+simd128}" cargo build -q --release --target wasm32-wasip1
echo built $L
