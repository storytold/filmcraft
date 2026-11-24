#!/usr/bin/env bash
# Check an x86_64 Linux Nightly tar as the Launcher unpacks it, then ask the GUI its version:
#
#   packaging/linux/nightly-check.sh --input /abs/filmcraft-<version>-linux-x86_64-nightly.tar.gz
#
# The archive holds exactly the GUI-only Nightly: `filmcraft` (the launcher, mode 755),
# `filmcraft-bin` (the GUI it starts with the Launcher's private profile, mode 755), the channel
# icon and the notices, all flat regular files. The launcher then answers both version queries —
# plain and the Launcher's form `--launch-profile nightly-v1 --profile-root <fresh dir> --version`
# — with exactly `FilmCraft <version> (nightly-v1)` on standard output, nothing on standard error,
# exit 0, and without touching the profile root (the query runs before any profile or window work).
# No signing material may be in the environment: this runs the payload.
set -euo pipefail
INPUT=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --input) [[ $# -ge 2 ]] || { echo 'missing --input value' >&2; exit 2; }; INPUT="$2"; shift 2 ;;
    -h | --help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "unsupported argument: $1" >&2; exit 2 ;;
  esac
done
die() { printf 'error: %s\n' "$*" >&2; exit 2; }
for name in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD KEYCHAIN_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID \
  MACOS_SIGN_IDENTITY MACOS_KEYCHAIN WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD AZURE_TENANT_ID AZURE_CLIENT_ID \
  AZURE_CLIENT_SECRET AZURE_SIGNING_ENDPOINT AZURE_SIGNING_ACCOUNT AZURE_CERT_PROFILE LINUX_SIGNING_KEY; do
  [[ -z "${!name:-}" ]] || die "refusing to run the payload with signing material in the environment ($name)"
done
[[ "$INPUT" == /* && -f "$INPUT" && ! -L "$INPUT" ]] || die '--input must be an absolute regular file'
NAME="$(basename -- "$INPUT")"
[[ "$NAME" =~ ^filmcraft-([0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\.[0-9]+\.g[0-9a-f]{12})-linux-x86_64-nightly\.tar\.gz$ ]] || die 'the input is not a prepared Nightly archive name'
VERSION="${BASH_REMATCH[1]}"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/filmcraft-nightly-check.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
mkdir -p "$WORK/payload" "$WORK/home" "$WORK/profile"

# Flat members only: no directory entries, links or absolute names.
python3 -I - "$INPUT" <<'PY' || die 'the archive holds a member that is not a flat regular file'
import sys, tarfile
with tarfile.open(sys.argv[1], 'r:gz') as archive:
    for member in archive.getmembers():
        name = member.name
        if member.isdir():
            continue
        if not member.isfile() or '/' in name.rstrip('/') or name.startswith('/') or '\\' in name:
            sys.exit(f"refused member: {name!r}")
PY
tar -xzf "$INPUT" -C "$WORK/payload"
entries="$(ls -A "$WORK/payload")"
for name in filmcraft filmcraft-bin filmcraft-nightly.png LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
  [[ -f "$WORK/payload/$name" && ! -L "$WORK/payload/$name" ]] || die "the archive has no regular $name"
done
for name in $entries; do
  case "$name" in
    filmcraft | filmcraft-bin | filmcraft-nightly.png | README.md | LICENSE | LICENSE-MIT | LICENSE-APACHE | COPYRIGHT | NOTICE | ATTRIBUTION.md) ;;
    OFL-*.txt) ;;
    *) die "unexpected nightly member: $name" ;;
  esac
done
for name in filmcraft filmcraft-bin; do
  [[ -x "$WORK/payload/$name" && "$(stat -c '%a' "$WORK/payload/$name")" == 755 ]] || die "$name must be executable with mode 755"
done
# The shipped launcher is the one that injects the Launcher's private profile, not the bare GUI.
grep -q -- '--launch-profile nightly-v1' "$WORK/payload/filmcraft" || die 'the launcher does not start the GUI with the Launcher profile'
[[ -z "$(find "$WORK/payload" ! -type f ! -type d -print -quit)" ]] || die 'the archive contains links or special files'

query() {
  local out
  out="$(timeout 20 env -i HOME="$WORK/home" XDG_DATA_HOME="$WORK/home/.local/share" PATH=/usr/bin:/bin \
    "$WORK/payload/filmcraft" "$@" 2>"$WORK/stderr")" || die "the version query ($*) failed or did not finish within 20 s"
  [[ ! -s "$WORK/stderr" ]] || die "the version query ($*) wrote to standard error: $(head -c 2000 "$WORK/stderr")"
  [[ "$out" == "FilmCraft $VERSION (nightly-v1)" ]] || die "the version query ($*) reports '$out', not FilmCraft $VERSION (nightly-v1)"
}
before="$(ls -A "$WORK/profile")"
query --version
query --launch-profile nightly-v1 --profile-root "$WORK/profile" --version
[[ "$(ls -A "$WORK/profile")" == "$before" ]] || die 'the version query created state in the profile root'
echo "==> verified: FilmCraft $VERSION (nightly-v1), plain and in the Launcher's profile form"
