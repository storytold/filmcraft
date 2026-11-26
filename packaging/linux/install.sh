#!/usr/bin/env bash
# Install the extracted Linux tarball. No sudo is needed for the default prefix.
set -euo pipefail

usage() {
  echo 'Usage: ./install.sh [--prefix /absolute/path]'
  echo 'Installs FilmCraft and its desktop integration into ~/.local by default.'
  echo 'For all users: sudo ./install.sh --prefix /usr/local'
}
fail() { echo "error: $*" >&2; exit 1; }
prefix="${HOME:?HOME is not set}/.local"
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --prefix)
      [[ "$#" -ge 2 ]] || fail '--prefix needs an absolute path'
      prefix="$2"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; fail "unknown argument: $1" ;;
  esac
done
case "$prefix" in /*) ;; *) fail '--prefix must be an absolute path' ;; esac
prefix="$(realpath -m -- "$prefix")"
# Desktop Exec paths cannot contain '='; launchers also misinterpret '%' in executable names.
[[ "$prefix" != *'='* && "$prefix" != *'%'* && ! "$prefix" =~ [[:cntrl:]] ]] || fail 'prefix cannot contain =, % or control characters'
[[ "$prefix" != / ]] || fail 'use /usr/local or a user directory, not /'
source_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)"
case "$prefix/" in "$source_dir/"*) fail 'choose an installation prefix outside the extracted tarball' ;; *) ;; esac
app_id=ai.storyteller.filmcraft
for binary in filmcraft filmcraft-cli; do
  [[ -f "$source_dir/bin/$binary" ]] || fail "missing bin/$binary; extract the complete tarball first"
done
[[ -f "$source_dir/share/applications/$app_id.desktop" ]] || fail 'missing desktop entry; extract the complete tarball first'
# A Nightly tarball ships its GUI as bin/filmcraft-bin beside the launcher that starts it with the
# Launcher's private profile. It installs under its own names, so a Stable install in the same
# prefix keeps its launcher, CLI, desktop entry and icons.
nightly=0
if [[ -f "$source_dir/bin/filmcraft-bin" ]]; then
  nightly=1
  product='FilmCraft Nightly'
  launcher=filmcraft-nightly
  cli=filmcraft-nightly-cli
  doc=filmcraft-nightly
  desktop_id="$app_id.nightly-v1"
else
  product=FilmCraft
  launcher=filmcraft
  cli=filmcraft-cli
  doc=filmcraft
  desktop_id="$app_id"
fi

mkdir -p -- "$prefix/bin" "$prefix/share"
install -m755 -- "$source_dir/bin/filmcraft" "$prefix/bin/$launcher"
install -m755 -- "$source_dir/bin/filmcraft-cli" "$prefix/bin/$cli"
if [[ "$nightly" = 1 ]]; then
  # Only the Nightly's own files here: Stable's desktop entry, icons and MIME database stay as they
  # are, and the Nightly's entry gets its own icon name instead of replacing Stable's.
  install -m755 -- "$source_dir/bin/filmcraft-bin" "$prefix/bin/filmcraft-bin"
  if [[ -d "$source_dir/share/doc/filmcraft" ]]; then
    mkdir -p -- "$prefix/share/doc/$doc"
    cp -R -- "$source_dir/share/doc/filmcraft/." "$prefix/share/doc/$doc/"
  fi
  icon=
  for candidate in "$source_dir/share/icons/hicolor/256x256/apps/$app_id.png" "$source_dir/share/icons/hicolor/"*/apps/"$app_id.png"; do
    if [[ -f "$candidate" ]]; then icon="$candidate"; break; fi
  done
  [[ -z "$icon" ]] || install -Dm644 -- "$icon" "$prefix/share/icons/hicolor/256x256/apps/$desktop_id.png"
else
  cp -R -- "$source_dir/share/." "$prefix/share/"
fi

# Use an absolute path so launching from the application menu does not depend on PATH.
# Escape the Exec argument, then the desktop entry's string value (two separate layers).
# https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
exec_path="$(printf '%s' "$prefix/bin/$launcher" | sed 's/[\\"`$]/\\&/g; s/\\/\\\\/g')"
try_path="$(printf '%s' "$prefix/bin/$launcher" | sed 's/\\/\\\\/g')"
while IFS= read -r line || [[ -n "$line" ]]; do
  if [[ "$nightly" = 1 ]]; then
    # The Nightly claims no associations, as in every other Nightly format, and its MIME database
    # outside the install is Stable's.
    case "$line" in
      MimeType=*) continue ;;
      "Name=FilmCraft") line="Name=$product" ;;
      "Icon=$app_id") line="Icon=$desktop_id" ;;
    esac
  fi
  case "$line" in
    Exec=*) printf 'Exec="%s" %%F\n' "$exec_path" ;;
    TryExec=*) printf 'TryExec=%s\n' "$try_path" ;;
    *) printf '%s\n' "$line" ;;
  esac
done <"$source_dir/share/applications/$app_id.desktop" >"$prefix/share/applications/$desktop_id.desktop"

# Cache tools are optional; installing binaries must also work on minimal systems.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "$prefix/share/applications" || echo 'warning: desktop cache refresh failed' >&2
fi
if command -v update-mime-database >/dev/null 2>&1 && [[ -d "$prefix/share/mime" ]]; then
  update-mime-database "$prefix/share/mime" || echo 'warning: MIME cache refresh failed' >&2
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [[ -d "$prefix/share/icons/hicolor" ]]; then
  gtk-update-icon-cache -q -t -f "$prefix/share/icons/hicolor" || echo 'warning: icon cache refresh failed' >&2
fi
printf 'Installed %s in %s\nLaunch it from the application menu or run "%s/bin/%s".\n' "$product" "$prefix" "$prefix" "$launcher"
case ":${PATH:-}:" in
  *":$prefix/bin:"*) ;;
  *) printf 'For terminal commands, add %s/bin to your PATH.\n' "$prefix" ;;
esac
