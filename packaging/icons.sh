#!/usr/bin/env bash
# Regenerate every derived app icon from assets/app-icon/filmcraft.svg (the canonical artwork; a
# 512-unit full-bleed tile with rx=112). filmcraft-small.svg is a lighter trace of the same drawing
# and is copied, not generated.
#
# Needs: resvg (brew install resvg / cargo install resvg). On macOS, iconutil also writes the
# .icns. The outputs are committed, so builds and packaging never need these tools.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/filmcraft.svg"
APP_ID="ai.storyteller.filmcraft"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# macOS icons follow Apple's grid: the tile body is 824 of 1024 px with a transparent margin, so
# the Dock shows it at the same size as other apps. 512 * 1024 / 824 = 636.27 units, margin 62.14.
MAC="$TMP/mac.svg"
sed 's/viewBox="0 0 512 512"/viewBox="-62.14 -62.14 636.27 636.27"/' "$SVG" >"$MAC"
grep -q 'viewBox="-62.14' "$MAC" || { echo "error: unexpected viewBox in $SVG" >&2; exit 1; }

render() { resvg -w "$2" -h "$2" "$1" "$3" </dev/null; }

render "$SVG" 1024 "$DIR/filmcraft-1024.png"
# Runtime window/Dock icon on macOS (apps/filmcraft/src/main.rs embeds it).
render "$MAC" 512 "$DIR/filmcraft-macos-512.png"

# Linux hicolor theme. Windows and Linux use the full-bleed tile.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$APP_ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$DIR/filmcraft-small.svg" "$DIR/hicolor/scalable/apps/$APP_ID.svg"

# Web app favicon and loading image.
render "$SVG" 128 "$ROOT/apps/filmcraft-web/web/favicon.png"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$SVG" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/filmcraft.ico" "${ICO_PNGS[@]}")

# macOS .icns.
if command -v iconutil >/dev/null; then
  SET="$TMP/filmcraft.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    render "$MAC" "$s" "$SET/icon_${s}x${s}.png"
    render "$MAC" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns -o "$DIR/filmcraft.icns" "$SET"
else
  echo "warning: iconutil not found (macOS only); filmcraft.icns not regenerated" >&2
fi
echo "icons written to $DIR"
