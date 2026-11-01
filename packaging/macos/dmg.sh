# shellcheck shell=bash
# DMG helpers shared by package.sh and dmg-test.sh. Source it after ../env.sh (`warn`).

# `hdiutil create` and `attach` attach a device, which fails now and then on CI runners ("Resource
# busy"), so they get a few tries. A retry can't end a hang: where a sandbox blocks the disk-image
# helper (some agent sandboxes do), hdiutil can wait forever, so build and check DMGs outside one.
# Usage: hdiutil_retry <what> <hdiutil args…>; tries five times with growing pauses.
hdiutil_retry() {
  local what="$1" attempt
  shift
  for attempt in 1 2 3 4 5; do
    if hdiutil "$@"; then
      return 0
    fi
    if [[ "$attempt" = 5 ]]; then
      echo "error: hdiutil $what failed 5 times" >&2
      return 1
    fi
    warn "hdiutil $what failed (attempt $attempt of 5); retrying in $((attempt * 10))s"
    sleep $((attempt * 10))
  done
}

# The app must pass `codesign --verify --strict --deep` as users get it: in the mounted image, and
# copied out of it with ditto, which keeps extended attributes the way a Finder drag to Applications
# does. Checking the bundle before it goes on the image is not enough: `hdiutil makehybrid -hfs`
# gave every file in the image a com.apple.FinderInfo, which --strict rejects as "resource fork,
# Finder information, or similar detritus not allowed" (#882), while the DMG's own signature and
# Gatekeeper still passed.
# Usage: check_dmg_app <dmg> <App.app>
#
# The check runs in a subshell whose EXIT trap detaches the image and removes the scratch directory
# however it ends: a failed step, `set -e`, Ctrl-C, or the SIGTERM a cancelled job gets (bash skips
# the EXIT trap when a signal kills it, so HUP, INT and TERM are turned into exits). The traps are
# the subshell's own, so a caller's EXIT trap is left alone.
check_dmg_app() (
  dmg="$1" app="$2" rc=0
  dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dmg-check.XXXXXX")"
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'dmg_check_cleanup "$dir" "$?"' EXIT
  mkdir "$dir/mnt" "$dir/copy"
  hdiutil_retry attach attach -readonly -nobrowse -noautoopen -mountpoint "$dir/mnt" "$dmg" >/dev/null || exit 1
  echo "==> codesign --verify --strict --deep: $app in $(basename "$dmg"), then a copy installed from it"
  if ! codesign --verify --strict --deep --verbose=2 "$dir/mnt/$app"; then
    rc=1
  elif ! ditto "$dir/mnt/$app" "$dir/copy/$app" || ! codesign --verify --strict --deep --verbose=2 "$dir/copy/$app"; then
    rc=1
  fi
  if [[ "$rc" != 0 ]]; then
    echo "error: $app in $(basename "$dmg") fails codesign --verify --strict --deep" >&2
  fi
  exit "$rc"
)

# check_dmg_app's EXIT trap. Whatever is mounted on <dir>/mnt is detached (found by its device, not
# a flag, so an image attached just before a signal goes too), then <dir> is removed. Exits with
# <status>, or 1 when the image won't detach (then <dir> stays, with the mount in it).
# Usage: dmg_check_cleanup <dir> <status>
dmg_check_cleanup() {
  local dir="$1" rc="$2" dev
  dev="$(stat -f %d "$dir/mnt" 2>/dev/null)" || dev=""
  if [[ -n "$dev" && "$dev" != "$(stat -f %d "$dir")" ]] && ! hdiutil_retry detach detach "$dir/mnt" >/dev/null; then
    echo "error: could not detach $dir/mnt" >&2
    exit 1
  fi
  rm -rf "$dir"
  exit "$rc"
}
