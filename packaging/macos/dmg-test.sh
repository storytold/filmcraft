#!/usr/bin/env bash
# The app in the macOS DMG, and a copy installed from it, pass `codesign --verify --strict --deep`
# (#882). Runs package.sh end to end, signed ad hoc, around a stand-in executable (no Rust build,
# no secrets), with a font licence that carries Finder info as the input. Then shows the check
# rejects the image `hdiutil makehybrid -hfs` made of the same files. macOS only;
# CI: .github/workflows/packaging-lint.yml.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
if [[ "$(uname -s)" != Darwin ]]; then
  echo "skip: needs macOS (hdiutil, codesign)"
  exit 0
fi
case "$(uname -m)" in
  arm64) ARCH=aarch64 TRIPLE=aarch64-apple-darwin CC_ARCH=arm64 ;;
  x86_64) ARCH=x86_64 TRIPLE=x86_64-apple-darwin CC_ARCH=x86_64 ;;
  *) fail "unknown machine $(uname -m)" ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# Ad hoc, not notarized, and no Actions annotations from the script under test.
unset MACOS_SIGN_IDENTITY MACOS_KEYCHAIN APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID GITHUB_ACTIONS
export CARGO_TARGET_DIR="$TMP/target" DIST="$TMP/dist" CRAFT_FONTS_DIR="$TMP/craft-fonts"
# shellcheck source=../env.sh
. "$HERE/../env.sh"
# shellcheck source=dmg.sh
. "$HERE/dmg.sh"

# package.sh only joins, signs and runs (`filmcraft-cli --version`) the binaries, so any
# executable stands in for them.
bin="$CARGO_TARGET_DIR/$TRIPLE/release"
mkdir -p "$bin"
printf 'int main(void) { return 0; }\n' | cc -arch "$CC_ARCH" -x c -o "$bin/filmcraft" -
cp "$bin/filmcraft" "$bin/filmcraft-cli"
# A licence with Finder info (what makehybrid gave every file): `cp` brings it into the bundle, so
# the strip before signing is tested too.
mkdir -p "$CRAFT_FONTS_DIR/fonts/test-sans"
echo "test licence" >"$CRAFT_FONTS_DIR/fonts/test-sans/OFL.txt"
/usr/bin/xattr -wx com.apple.FinderInfo 00000000000000000000FFFFFFFF000000000000000000000000000000000000 \
  "$CRAFT_FONTS_DIR/fonts/test-sans/OFL.txt"

"$HERE/package.sh" --arch "$ARCH" --skip-build || fail "package.sh failed"
DMG="$DIST/filmcraft-$VERSION-macos-$ARCH.dmg"
check_dmg_app "$DMG" FilmCraft.app || fail "the app in $(basename "$DMG") fails codesign --strict"

# Interrupted while the image is mounted (SIGTERM, as a cancelled job gets it: a stand-in codesign
# sends it to the check), the check still detaches the image and removes its scratch directory.
mkdir "$TMP/stub" "$TMP/interrupted"
cat >"$TMP/stub/codesign" <<'EOF'
#!/bin/sh
kill -TERM "$PPID"
EOF
chmod +x "$TMP/stub/codesign"
if (
  PATH="$TMP/stub:$PATH"
  export RUNNER_TEMP="$TMP/interrupted"
  check_dmg_app "$DMG" FilmCraft.app
) >/dev/null 2>&1; then
  fail "check_dmg_app passed though it was interrupted"
fi
left="$(ls -A "$TMP/interrupted")"
if [[ -n "$left" ]]; then
  hdiutil detach -force "$TMP/interrupted/$left/mnt" >/dev/null 2>&1 || true
  fail "an interrupted check_dmg_app left $left behind, its image still attached"
fi

# The check must catch the defect it guards against: the same staged files, imaged the old way.
OLD="$TMP/makehybrid.dmg"
hdiutil makehybrid -quiet -hfs -hfs-volume-name FilmCraft -o "$OLD" "$CARGO_TARGET_DIR/macos-package/dmg"
if out="$(check_dmg_app "$OLD" FilmCraft.app 2>&1)"; then
  fail "check_dmg_app accepted a makehybrid image"
fi
case "$out" in
  *"Finder information, or similar detritus"*) ;;
  *) fail "check_dmg_app rejected the makehybrid image for another reason: $out" ;;
esac

echo "ok: the app in the DMG passes codesign --strict; an interrupted check detaches; a makehybrid image of it is rejected"
