#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
PYTHON="$ROOT/target/rotoscope/.venv/bin/python"
export GRADIO_ANALYTICS_ENABLED=False
if [ ! -x "$PYTHON" ]; then
    uv venv --python python3.12 "$ROOT/target/rotoscope/.venv"
fi
if [ ! -f "$ROOT/target/rotoscope/.installed" ] || ! cmp -s experiments/rotoscope/requirements.txt target/rotoscope/.installed || [ "${1:-}" = "--setup" ]; then
    UV_LINK_MODE=copy uv pip install -q --python "$PYTHON" -r experiments/rotoscope/requirements.txt
    "$PYTHON" experiments/rotoscope/models.py --setup
    cp experiments/rotoscope/requirements.txt target/rotoscope/.installed
fi
if [ "${1:-}" = "--setup" ]; then
    shift
fi
exec "$PYTHON" experiments/rotoscope/app.py "$@"
