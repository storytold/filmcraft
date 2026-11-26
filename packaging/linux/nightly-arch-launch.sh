#!/bin/sh
# The FilmCraft Nightly launcher for the Arch handoff (packaging/linux/nightly-arch-tar.sh): the
# real GUI stays in .filmcraft-nightly/, and this starts it with the Launcher's private profile,
# as every other Nightly package does.
set -eu

bin="$(dirname "$0")/.filmcraft-nightly/filmcraft"
profile="${XDG_DATA_HOME:-${HOME:-}/.local/share}/filmcraft-nightly/profile"

# The Launcher's protocol, a bare version query and the usage screen pass through untouched: the
# first already names its profile, the others must answer without one.
case "${1-}" in
  --launch-profile | --profile-root | --version | -V | --help | -h) exec "$bin" "$@" ;;
esac
# A caller-written `--` already separates the files from the options: drop it, so the launcher's
# own separator never arrives as a file name.
if [ "${1-}" = -- ]; then shift; fi
exec "$bin" --launch-profile nightly-v1 --profile-root "$profile" -- "$@"
