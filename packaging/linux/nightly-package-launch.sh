#!/bin/sh
# The FilmCraft Nightly launcher inside the distro tarball package.sh builds (bin/filmcraft beside
# bin/filmcraft-bin). Those distro artifacts keep their own profile folder
# (`XDG_DATA_HOME/filmcraft-nightly/profile`); the plain nightly tar the Launcher installs uses the
# canonical shared template in nightly-launch.sh instead.
# The entry point inside the Nightly tarball (packaging/linux/nightly.sh stages it as `filmcraft`,
# beside the GUI as `filmcraft-bin`), and the form every other Nightly Linux package installs.
#
# A Nightly build only starts with its private profile, and every package format passes the
# Launcher's literal protocol: `--launch-profile nightly-v1 --profile-root <dir> -- <files>`. The
# profile sits outside stable's folders (settings, recovery autosaves, media cache, presets, logs,
# models) and outside this unpacked copy, which the app's own fence would refuse.
set -eu

here="$(dirname "$(readlink -f "$0")")"
bin="$here/filmcraft-bin"
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
