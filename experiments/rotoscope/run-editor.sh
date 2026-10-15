#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
ROTO_DATA="$ROOT/target/rotoscope"
PYTHON="$ROTO_DATA/.venv/bin/python"
if [ ! -x "$PYTHON" ]; then
    uv venv --python python3.12 "$ROTO_DATA/.venv"
fi
if [ ! -f "$ROTO_DATA/.installed" ] || ! cmp -s experiments/rotoscope/requirements.txt "$ROTO_DATA/.installed"; then
    UV_LINK_MODE=copy uv pip install -q --python "$PYTHON" -r experiments/rotoscope/requirements.txt
    "$PYTHON" experiments/rotoscope/models.py --setup
    cp experiments/rotoscope/requirements.txt "$ROTO_DATA/.installed"
fi
if ! "$PYTHON" experiments/rotoscope/export_native.py --check > "$ROTO_DATA/native-model-check.log" 2>&1; then
    "$PYTHON" experiments/rotoscope/export_native.py
fi
cargo +1.95 build -p filmcraft --features experimental-ai-rotoscope
if [ "$#" -eq 0 ] && [ -f "$ROTO_DATA/FilmCraft-AI-Test.fcproj" ]; then
    set -- "$ROTO_DATA/FilmCraft-AI-Test.fcproj"
fi
FILMCRAFT_AI_ROTO_MODELS="$ROTO_DATA/models/native" "$ROOT/target/debug/filmcraft" --control 9876 --data-dir "$ROTO_DATA/editor-data" --no-recover --empty "$@"
