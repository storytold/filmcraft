#!/bin/sh
# Install the FilmCraft fidelity bridge panel into Adobe Premiere Pro (macOS).
#
# Copies ./bridge to Premiere's per-user extension folder and turns on CEP's PlayerDebugMode, which
# is what lets Premiere load an unsigned panel. Then restart Premiere, open any project, and open
# Window > Extensions > FilmCraft Fidelity once; render.py does the rest. `install.sh --uninstall`
# removes the panel again (PlayerDebugMode is left as it is).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
DEST="$HOME/Library/Application Support/Adobe/CEP/extensions/org.filmcraft.fidelity.bridge"
if [ "${1:-}" = "--uninstall" ]; then
  rm -rf "$DEST"
  echo "removed $DEST"
  exit 0
fi
for v in 9 10 11 12 13 14 15; do
  defaults write "com.adobe.CSXS.$v" PlayerDebugMode 1
done
rm -rf "$DEST"
mkdir -p "$DEST"
cp -R "$HERE/bridge/." "$DEST/"
echo "installed $DEST"
echo "next: restart Premiere Pro, open a project, then Window > Extensions > FilmCraft Fidelity (once)."
