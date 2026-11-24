#!/usr/bin/env bash
# Validate the staged Arch launcher without building or opening FilmCraft.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/source"
dst="$work/dist"
base=filmcraft-0.0.1-nightly.1-linux-x86_64
mkdir -p "$stage/bin" "$dst" "$work/extract"

# A local argv recorder stands in for the compiled binaries. It never opens a UI.
cat >"$stage/bin/filmcraft" <<'RECORDER'
#!/bin/sh
printf '<%s>\n' "$@"
RECORDER
chmod 755 "$stage/bin/filmcraft"
cp "$stage/bin/filmcraft" "$stage/bin/filmcraft-cli"

bash "$HERE/nightly-arch-tar.sh" "$stage" "$work/stage" "$dst" "$base"
tar -xzf "$dst/$base.tar.gz" -C "$work/extract"
bin="$work/extract/$base/bin"
# Simulate the existing Arch bin-tree copy and public launcher rename.
mkdir -p "$work/installed"
cp -R "$bin" "$work/installed/bin"
bin="$work/installed/bin"
mv "$bin/filmcraft" "$bin/filmcraft-nightly"
mv "$bin/filmcraft-cli" "$bin/filmcraft-nightly-cli"

[[ ! -e "$stage/bin/.filmcraft-nightly" ]]
cmp "$stage/bin/filmcraft" "$bin/.filmcraft-nightly/filmcraft"
cmp "$stage/bin/filmcraft-cli" "$bin/filmcraft-nightly-cli"
expect() {
  expected="$1"
  shift
  actual=$(PATH="$bin:$PATH" filmcraft-nightly "$@")
  if [[ "$actual" != "$expected" ]]; then
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  fi
}
expect "$(printf '<%s>\n' 'clip with spaces.fcproj' '--other-name.fcproj')" \
  'clip with spaces.fcproj' '--other-name.fcproj'
expect '<--version>' --version
expect '<-V>' -V

echo "PASS: Arch Nightly handoff preserves launch arguments"
