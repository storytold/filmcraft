#!/bin/sh
set -eu

export FILMCRAFT_DATA_DIR="${FILMCRAFT_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/filmcraft-nightly}"
real_bin="$(dirname "$0")/.filmcraft-nightly/filmcraft"
exec "$real_bin" "$@"
