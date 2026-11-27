#!/usr/bin/env bash
# The launcher inside the plain Nightly tarball (packaging/linux/nightly-launch.sh): a launch adds
# the Launcher's private profile, a version query or an existing protocol passes through, and the
# fence's shape holds (absolute root, no parent traversal, outside this copy).
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

profile="${XDG_DATA_HOME:-${HOME:-}/.local/share}/filmcraft-nightly/profile"
# The profile must be something the app's fence accepts: absolute, no parent traversal, and not
# inside the unpacked copy (the running install).
case "$profile" in
  /*) ;;
  *) echo "FAIL: the launcher profile is not absolute: $profile" >&2; exit 1 ;;
esac
case "/$profile/" in
  */../*) echo "FAIL: the launcher profile contains parent traversal" >&2; exit 1 ;;
esac
case "$profile/" in
  "$work/tree/"*) echo "FAIL: the launcher profile is inside the unpacked copy" >&2; exit 1 ;;
esac

expect_launch() {
  expected="$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$profile" -- "$@")"
  actual=$("$work/tree/filmcraft" "$@")
  [[ "$actual" == "$expected" ]] || {
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  }
}
expect_forwarded() {
  expected="$(printf '<%s>\n' "$@")"
  actual=$("$work/tree/filmcraft" "$@")
  [[ "$actual" == "$expected" ]] || {
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    exit 1
  }
}

expect_launch
expect_launch 'project with spaces.fcproj' '--other-name.fcproj'
# A caller-written `--` is dropped: the launcher's own separator must arrive once, never twice,
# and what follows the caller's separator stays a file name (here `--version`).
expected="$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$profile" -- 'project with spaces.fcproj' --version)"
actual=$("$work/tree/filmcraft" -- 'project with spaces.fcproj' --version)
[[ "$actual" == "$expected" ]] || {
  printf 'caller separator:\nexpected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
  exit 1
}
expect_forwarded --version
expect_forwarded -V
expect_forwarded --help
expect_forwarded -h
expect_forwarded --launch-profile nightly-v1 --profile-root /launcher/profile --version
expect_forwarded --launch-profile nightly-v1 --profile-root /launcher/profile -- a.fcproj

echo "PASS: tarball Nightly launcher starts the GUI with the Launcher profile and forwards files"
