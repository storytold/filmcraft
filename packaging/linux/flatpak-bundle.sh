#!/usr/bin/env bash
# Repackage a FilmCraft Linux tarball as a single-file Flatpak bundle:
#
#   $DIST/filmcraft-<version>-linux-<arch>.flatpak
#
# Usage: packaging/linux/flatpak-bundle.sh [--no-test] [TARBALL]
#
# TARBALL defaults to $DIST/filmcraft-<version>-linux-<arch>.tar.gz from package.sh. The bundle
# is built for the host architecture (x86_64 or aarch64), which must match the tarball's.
# Needs flatpak, flatpak-builder and the SVG pixbuf loader (librsvg2-common) for the host's
# `appstreamcli compose`; the freedesktop runtime and SDK named in the manifest are
# installed per-user from Flathub. Unless --no-test, the bundle is then installed per-user and
# `filmcraft-cli --version` is run inside the sandbox as a smoke test.
# Manifest: packaging/linux/flatpak/ai.storyteller.filmcraft.bundle.yml.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
APP_ID=ai.storyteller.filmcraft
FLATPAK_ID="$APP_ID"
# The remote indexes both app IDs on the stable ref branch; IDs isolate installations.
FLATPAK_BRANCH=stable
if [[ "${FILMCRAFT_BUILD_CHANNEL:-}" == nightly-v1 || "$VERSION" == *-nightly* ]]; then
  FLATPAK_ID="$APP_ID.nightly"
fi
FLATHUB=https://dl.flathub.org/repo/flathub.flatpakrepo

TEST=1
OCI=0
TARBALL=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-test) TEST=0; shift ;;
    --oci) OCI=1; shift ;;
    -h | --help) sed -n '2,14p' "$0"; exit 0 ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    *) TARBALL="$1"; shift ;;
  esac
done

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) ;;
  aarch64 | arm64) ARCH=aarch64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
BASENAME="filmcraft-$VERSION-linux-$ARCH"
TARBALL="${TARBALL:-$DIST/$BASENAME.tar.gz}"
[[ -f "$TARBALL" ]] || { echo "error: $TARBALL not found (run packaging/linux/package.sh --formats tar first)" >&2; exit 1; }
for tool in flatpak flatpak-builder; do
  command -v "$tool" >/dev/null || { echo "error: $tool not found (apt install flatpak flatpak-builder librsvg2-common)" >&2; exit 1; }
done

echo "==> FilmCraft $VERSION Flatpak bundle for $ARCH from $(basename "$TARBALL")"

WORK="$CARGO_TARGET_DIR/flatpak-bundle"
rm -rf "$WORK"
mkdir -p "$WORK/stage"
tar -xzf "$TARBALL" -C "$WORK/stage" --strip-components=1
[[ -x "$WORK/stage/bin/filmcraft" ]] || { echo "error: $TARBALL has no bin/filmcraft" >&2; exit 1; }
if [[ "$FLATPAK_ID" != "$APP_ID" ]]; then
  # The GUI runs with the Launcher's private profile, as on every other Nightly format: inside the
  # sandbox $XDG_DATA_HOME is this app's own per-app folder, so the profile never meets stable's.
  # A Nightly tarball from package.sh already separates the launcher from the GUI; an older one
  # does not. Either way this bundle installs its own launcher and the GUI in /app/libexec.
  if [[ -e "$WORK/stage/bin/filmcraft-bin" ]]; then
    rm -f "$WORK/stage/bin/filmcraft"
  else
    mv "$WORK/stage/bin/filmcraft" "$WORK/stage/bin/filmcraft-bin"
  fi
  cat >"$WORK/stage/bin/filmcraft" <<'WRAPPER'
#!/bin/sh
# FilmCraft Nightly: the GUI runs with the Launcher's private profile, which holds every per-user
# file (preferences, recovery autosaves, media cache, presets, logs, downloaded models).
set -eu
bin=/app/libexec/filmcraft
profile="${XDG_DATA_HOME:-${HOME:-}/.local/share}/filmcraft-nightly/profile"
# The Launcher's protocol, a bare version query and the usage screen pass through untouched.
case "${1-}" in
  --launch-profile | --profile-root | --version | -V | --help | -h) exec "$bin" "$@" ;;
esac
# A caller-written `--` already separates the files from the options: drop it, so the launcher's
# own separator never arrives as a file name.
if [ "${1-}" = -- ]; then shift; fi
exec "$bin" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
WRAPPER
  chmod 755 "$WORK/stage/bin/filmcraft"
  NIGHTLY_ICON="$ROOT/assets/app-icon/filmcraft-nightly.png"
  if [[ ! -f "$NIGHTLY_ICON" ]]; then
    echo "error: the Nightly icon $NIGHTLY_ICON is missing" >&2
    exit 2
  fi
  sed -e "s|^Icon=$APP_ID$|Icon=$FLATPAK_ID|" \
      -e "s|^Name=FilmCraft$|Name=FilmCraft Nightly|" \
      -e 's|^StartupWMClass=.*|StartupWMClass=ai.storyteller.filmcraft.nightly-v1|' \
      -e '/^MimeType=/d' \
      "$WORK/stage/share/applications/$APP_ID.desktop" >"$WORK/stage/share/applications/$FLATPAK_ID.desktop"
  rm -f "$WORK/stage/share/applications/$APP_ID.desktop"
  # The Nightly claims no associations, so its metainfo advertises none and it registers no MIME.
  sed -e "s|<id>$APP_ID</id>|<id>$FLATPAK_ID</id>|g" \
      -e "s|<name>FilmCraft</name>|<name>FilmCraft Nightly</name>|g" \
      -e "s|<launchable type=\"desktop-id\">$APP_ID.desktop</launchable>|<launchable type=\"desktop-id\">$FLATPAK_ID.desktop</launchable>|g" \
      -e "s|<icon type=\"stock\">$APP_ID</icon>|<icon type=\"stock\">$FLATPAK_ID</icon>|g" \
      -e '/<mediatype>/d' \
      "$WORK/stage/share/metainfo/$APP_ID.metainfo.xml" >"$WORK/stage/share/metainfo/$FLATPAK_ID.metainfo.xml"
  rm -f "$WORK/stage/share/metainfo/$APP_ID.metainfo.xml"
  rm -rf "$WORK/stage/share/mime"
  rm -rf "$WORK/stage/share/icons/hicolor"
  install -Dm644 "$NIGHTLY_ICON" "$WORK/stage/share/icons/hicolor/256x256/apps/$FLATPAK_ID.png"
  # No MIME install, and no host-Flatpak updater talk-name: a Nightly never asks the stable updater
  # (the app disables it), and the /app/libexec line is the GUI the launcher starts.
  sed -e "s|ai\.storyteller\.filmcraft|$FLATPAK_ID|g" \
      -e '/share\/mime\/packages\//d' \
      -e '/--talk-name=org\.freedesktop\.Flatpak/d' \
      -e 's|- install -Dm755 bin/filmcraft /app/bin/filmcraft|- install -Dm755 bin/filmcraft-bin /app/libexec/filmcraft\n      - install -Dm755 bin/filmcraft /app/bin/filmcraft|' \
      "$HERE/flatpak/$APP_ID.bundle.yml" >"$WORK/$FLATPAK_ID.yml"
else
  cp "$HERE/flatpak/$APP_ID.bundle.yml" "$WORK/$FLATPAK_ID.yml"
fi

flatpak remote-add --user --if-not-exists flathub "$FLATHUB"
# --disable-rofiles-fuse: no FUSE in containers and CI runners.
flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse --force-clean \
  --default-branch="$FLATPAK_BRANCH" --repo="$WORK/repo" --state-dir="$WORK/state" "$WORK/build" "$WORK/$FLATPAK_ID.yml"

OUT="$DIST/$BASENAME.flatpak"
# --runtime-repo lets `flatpak install` fetch the freedesktop runtime from Flathub if missing.
flatpak build-bundle --runtime-repo="$FLATHUB" "$WORK/repo" "$OUT" "$FLATPAK_ID" "$FLATPAK_BRANCH"
echo "wrote $OUT"
if [[ "$OCI" = 1 ]]; then
  OUT_OCI="$DIST/$BASENAME-oci"
  rm -rf "$OUT_OCI"
  flatpak build-bundle --oci "$WORK/repo" "$OUT_OCI" "$FLATPAK_ID" "$FLATPAK_BRANCH"
  echo "wrote OCI layout to $OUT_OCI"
fi

if [[ "$TEST" = 1 ]]; then
  flatpak install --user -y --noninteractive --reinstall "$OUT"
  flatpak info --user "$FLATPAK_ID"
  flatpak run --command=filmcraft-cli "$FLATPAK_ID" --version
  if [[ "$FLATPAK_ID" == "$APP_ID" ]]; then
    flatpak run --command=sh "$FLATPAK_ID" -c 'ls /app/share/applications /app/share/metainfo /app/share/mime/packages /app/share/icons/hicolor/*/apps'
  else
    flatpak run --command=sh "$FLATPAK_ID" -c 'ls /app/share/applications /app/share/metainfo /app/share/icons/hicolor/*/apps && test ! -e /app/share/mime/packages && test -x /app/bin/filmcraft && test -x /app/libexec/filmcraft'
  fi
fi
echo "==> done"
ls -lh "$OUT"
