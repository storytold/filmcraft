#!/usr/bin/env bash
# Smoke test for packaging/linux/AppRun: the AppImage's desktop integration writes and refreshes the
# host .desktop entry and icons, is idempotent, honours the opt-out and an unwritable HOME, always
# launches the app, and a Nightly AppImage starts it with the Launcher's private profile.
#
# Runs in seconds with no build (CI: .github/workflows/packaging-lint.yml).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP_ID=ai.storyteller.filmcraft

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Fake AppDir with the real AppRun, .desktop template, icons, and a stub binary.
APPDIR="$TMP/FilmCraft.AppDir"
mkdir -p "$APPDIR/usr/bin"
install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
install -Dm644 "$HERE/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
mkdir -p "$APPDIR/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$APPDIR/usr/share/icons/"
printf '#!/bin/sh\necho "stub: $*"\n' >"$APPDIR/usr/bin/filmcraft"
chmod +x "$APPDIR/usr/bin/filmcraft"

APPIMAGE="$TMP/filmcraft test.AppImage" # spaces on purpose: Exec must stay quoted
: >"$APPIMAGE"
export APPIMAGE
export HOME="$TMP/home"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$HOME"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# 1. Launch works and passes arguments through.
out="$("$APPDIR/AppRun" open project.fcproj)" || fail "AppRun exited non-zero"
[[ "$out" = "stub: open project.fcproj" ]] || fail "arguments not forwarded: $out"

DESKTOP="$XDG_DATA_HOME/applications/$APP_ID.desktop"
[[ -f "$DESKTOP" ]] || fail "desktop entry not installed"

# 2. Exec rewritten to the AppImage (quoted, spaces intact), TryExec dropped,
#    everything else preserved from the packaged entry.
expected_exec="Exec=\"$(readlink -f "$APPIMAGE")\" %F"
grep -Fxq "$expected_exec" "$DESKTOP" || fail "Exec not rewritten: $(grep '^Exec=' "$DESKTOP")"
grep -q '^TryExec=' "$DESKTOP" && fail "TryExec kept, the entry would be hidden"
grep -Fxq "Icon=$APP_ID" "$DESKTOP" || fail "Icon line lost"
grep -Fxq 'Categories=AudioVideo;Video;AudioVideoEditing;' "$DESKTOP" || fail "metadata lines lost"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$DESKTOP" || fail "generated desktop entry is invalid"
fi

# 3. Icons landed under the name the .desktop entry references.
for f in "16x16/apps/$APP_ID.png" "256x256/apps/$APP_ID.png" "scalable/apps/$APP_ID.svg"; do
  [[ -f "$XDG_DATA_HOME/icons/hicolor/$f" ]] || fail "icon missing: $f"
done

# 4. Idempotent: a second launch must not rewrite anything (inode unchanged).
before="$(stat -c %i "$DESKTOP")"
out="$("$APPDIR/AppRun")" || fail "second launch exited non-zero"
[[ "$out" = "stub: " ]] || fail "second launch output: $out"
after="$(stat -c %i "$DESKTOP")"
[[ "$before" = "$after" ]] || fail "desktop entry rewritten although unchanged"

# 5. Moving the AppImage refreshes Exec.
mv "$APPIMAGE" "$TMP/moved.AppImage"
export APPIMAGE="$TMP/moved.AppImage"
"$APPDIR/AppRun" >/dev/null || fail "launch after move exited non-zero"
grep -Fxq "Exec=\"$(readlink -f "$APPIMAGE")\" %F" "$DESKTOP" || fail "Exec not refreshed after move"

# 6. Opt-out installs nothing.
rm -rf "$XDG_DATA_HOME"
FILMCRAFT_NO_DESKTOP_INTEGRATION=1 "$APPDIR/AppRun" >/dev/null || fail "opt-out launch exited non-zero"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "opt-out still integrated"

# 7. An unwritable data dir never blocks the launch.
mkdir -p "$TMP/ro" && chmod a-w "$TMP/ro"
out="$(XDG_DATA_HOME="$TMP/ro/data" "$APPDIR/AppRun" out.fcproj)" || fail "launch blocked by unwritable data dir"
[[ "$out" = "stub: out.fcproj" ]] || fail "stub not run against unwritable data dir: $out"
chmod u+w "$TMP/ro"

# 8. No $APPIMAGE (extracted AppDir): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
env -u APPIMAGE "$APPDIR/AppRun" >/dev/null || fail "extracted-AppDir launch exited non-zero"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "integrated without $APPIMAGE"

# 9. A path that would need Exec escaping (here a `$`): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
odd="$TMP/odd\$(id).AppImage"
: >"$odd"
out="$(APPIMAGE="$odd" "$APPDIR/AppRun" x)" || fail "launch with an odd path exited non-zero"
[[ "$out" = "stub: x" ]] || fail "odd path: $out"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "integrated an AppImage path that needs escaping"

# 10. A Nightly AppImage (package.sh rewrites the DESK_ID and channel lines) starts the GUI with the
#     Launcher's private profile; a version query still passes through untouched.
NIGHTLY_APPDIR="$TMP/Nightly.AppDir"
mkdir -p "$NIGHTLY_APPDIR/usr/bin"
install -Dm755 "$HERE/AppRun" "$NIGHTLY_APPDIR/AppRun"
cat >"$NIGHTLY_APPDIR/usr/bin/filmcraft" <<'RECORDER'
#!/bin/sh
printf '<%s>\n' "$@"
RECORDER
chmod +x "$NIGHTLY_APPDIR/usr/bin/filmcraft"
sed -i 's|^CHANNEL=stable$|CHANNEL=nightly-v1|' "$NIGHTLY_APPDIR/AppRun"
grep -qx 'CHANNEL=nightly-v1' "$NIGHTLY_APPDIR/AppRun" || fail "Nightly AppRun was not switched"
out="$(env -u APPIMAGE XDG_DATA_HOME="$TMP/nightly-data" "$NIGHTLY_APPDIR/AppRun" a.fcproj)" || fail "Nightly AppRun exited non-zero"
expected="$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$TMP/nightly-data/filmcraft-nightly/profile" -- a.fcproj)"
[[ "$out" = "$expected" ]] || fail "Nightly AppRun did not use the Launcher profile: $out"
out="$(env -u APPIMAGE "$NIGHTLY_APPDIR/AppRun" --version)" || fail "Nightly version query exited non-zero"
[[ "$out" = "<--version>" ]] || fail "Nightly AppRun rewrote a version query: $out"
out="$(env -u APPIMAGE "$NIGHTLY_APPDIR/AppRun" --help)" || fail "Nightly usage query exited non-zero"
[[ "$out" = "<--help>" ]] || fail "Nightly AppRun rewrote a usage query: $out"
# A caller-written `--` is dropped: it must never arrive as a file name.
out="$(env -u APPIMAGE XDG_DATA_HOME="$TMP/nightly-data" "$NIGHTLY_APPDIR/AppRun" -- b.fcproj --version)" || fail "Nightly AppRun exited non-zero"
expected="$(printf '<%s>\n' --launch-profile nightly-v1 --profile-root "$TMP/nightly-data/filmcraft-nightly/profile" -- b.fcproj --version)"
[[ "$out" = "$expected" ]] || fail "Nightly AppRun kept a caller-written separator: $out"

echo "AppRun desktop integration ok"
