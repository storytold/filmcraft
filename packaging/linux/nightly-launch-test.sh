#!/usr/bin/env bash
# The launcher inside the plain Nightly tarball (packaging/linux/nightly-launch.sh), which is the
# canonical shared template from craft-common-actions: a launch starts `filmcraft-bin` beside the
# script with the Launcher's private profile under the Nightly package identity, a version query
# reaches the GUI as one plain `--version`, and every other argument is a file argument. The distro
# tarball's own launcher (packaging/linux/nightly-package-launch.sh) is pinned by
# packaging/nightly-release-tests.py.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree"

# An argv recorder stands in for the GUI. It never opens a window.
cat >"$work/tree/filmcraft-bin" <<'RECORDER'
#!/bin/sh
printf '<%s>\n' "$@"
RECORDER
chmod 755 "$work/tree/filmcraft-bin"
cp "$HERE/nightly-launch.sh" "$work/tree/filmcraft"

identity=ai.storyteller.filmcraft.nightly
profile="${XDG_DATA_HOME:-${HOME:-}/.local/share}/$identity"
expect() {
  expected="$1"
  shift
  actual=$("$work/tree/filmcraft" "$@")
  [[ "$actual" == "$expected" ]] || {
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  }
}
launch() { printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$profile" -- "$@"; }

# The profile is the identity's own folder: absolute and outside the unpacked copy.
case "$profile" in /*) ;; *) echo "FAIL: the profile is not absolute: $profile" >&2; exit 1 ;; esac
case "$profile/" in "$work/tree/"*) echo "FAIL: the profile is inside the unpacked copy" >&2; exit 1 ;; esac

expect "$(launch)" 
expect "$(launch 'project with spaces.fcproj' '--other-name.fcproj')" 'project with spaces.fcproj' '--other-name.fcproj'
# A file spelled like a flag is a file once the caller writes the delimiter, and only the first
# argument can be a version query: the template keeps the caller's own `--`, so it arrives once more.
expect "$(launch -- --version)" -- --version
# Both version spellings reach the GUI as one plain `--version`.
expect "$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$profile" --version)" --version
expect "$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$profile" --version)" -V
# The ArtCraft Launcher starts the entry point with its own profile pair and per-install root; the
# template hands that line to the GUI unchanged.
expect "$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root '/launcher profile' -- a.fcproj)" \
  --launch-profile nightly-v1 --profile-root '/launcher profile' -- a.fcproj
# Anything else is wrapped in the template's own root, including a profile line that does not open
# the arguments.
expect "$(launch --profile-root /launcher/profile --)" --profile-root /launcher/profile --
# XDG_DATA_HOME decides the root when it is set.
XDG_DATA_HOME="$work/xdg with space" expect "$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$work/xdg with space/$identity" -- b.fcproj)" b.fcproj
# Nothing but the GUI beside the script is started, and no data folder means no launch.
mv "$work/tree/filmcraft-bin" "$work/moved"
if "$work/tree/filmcraft" a.fcproj >/dev/null 2>&1; then
  echo "FAIL: the launcher started without its GUI beside it" >&2
  exit 1
fi
mv "$work/moved" "$work/tree/filmcraft-bin"
if env -u XDG_DATA_HOME -u HOME "$work/tree/filmcraft" a.fcproj >/dev/null 2>&1; then
  echo "FAIL: the launcher started without a data folder" >&2
  exit 1
fi

echo "PASS: tarball Nightly launcher starts the GUI with the Launcher profile and forwards files"
