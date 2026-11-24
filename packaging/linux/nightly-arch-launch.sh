#!/bin/sh
set -eu

real_bin="$(dirname "$0")/.filmcraft-nightly/filmcraft"
exec "$real_bin" "$@"
