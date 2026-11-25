#!/usr/bin/env bash
# Build and package FilmCraft for Linux (<arch> is x86_64 or aarch64, or riscv64 when cross-compiling):
#
#   $DIST/filmcraft-<version>-linux-<arch>.AppImage  any distro with glibc >= the build host's
#   $DIST/filmcraft-<version>-linux-<arch>.AppImage.zsync  delta updates (needs zsyncmake)
#   $DIST/filmcraft-<version>-linux-<arch>.deb       Debian, Ubuntu, Mint, Pop!_OS, ...
#   $DIST/filmcraft-<version>-linux-<arch>.rpm       Fedora, openSUSE, RHEL, ...
#   $DIST/filmcraft-<version>-linux-<arch>.tar.gz    plain FHS-style tree (bin/, share/)
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb rpm tar"] [--arch-tar-dir DIRECTORY]
#
# Needs: cargo; nfpm for deb/rpm (https://nfpm.goreleaser.com); appimagetool for the AppImage
# (downloaded into $CARGO_TARGET_DIR if missing). Build on an old distro (CI: Ubuntu 22.04,
# glibc 2.35) so the binaries run on newer ones. Optional: desktop-file-validate, appstreamcli,
# zsyncmake (the zsync package) for the AppImage's .zsync.
#
# Cross-compiling (CI's riscv64 job, tar only): set CROSS_ARCH (riscv64), CROSS_TARGET (the Rust
# target), CROSS_COMPILE (binutils prefix, for strip) and optionally EMULATOR (qemu-riscv64) to
# smoke-test the CLI.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
APP_ID=ai.storyteller.filmcraft

SKIP_BUILD=0
FORMATS="appimage deb rpm tar"
ARCH_TAR_DIR=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    --arch-tar-dir)
      [[ -n "${2:-}" ]] || { echo "error: --arch-tar-dir needs a directory" >&2; exit 2; }
      ARCH_TAR_DIR="$2"
      shift 2
      ;;
    -h | --help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ARCH="${CROSS_ARCH:-$(uname -m)}"
case "$ARCH" in
  riscv64) DEB_ARCH=riscv64 ;;
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
export FILMCRAFT_MAINTAINER="${FILMCRAFT_MAINTAINER:-FilmCraft maintainers <filmcraft@storyteller.ai>}"
IS_NIGHTLY=0
if [[ "${FILMCRAFT_BUILD_CHANNEL:-}" == nightly-v1 || "$VERSION" == *-nightly* ]]; then
  IS_NIGHTLY=1
  # The binary compiles the exact prepared identity (build.rs refuses anything else), so a
  # Nightly package never invents a version: two different nights must never share one.
  if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\.[0-9]+\.g[0-9a-f]{12}$ ]]; then
    echo "error: a Nightly package requires the prepared version identity" >&2
    echo "       (X.Y.Z-nightly.YYYYMMDD.RUN.ATTEMPT.gSHA12; set FILMCRAFT_VERSION and FILMCRAFT_BUILD_CHANNEL=nightly-v1)" >&2
    exit 2
  fi
  export FILMCRAFT_BUILD_CHANNEL=nightly-v1
  export FILMCRAFT_VERSION="$VERSION"
fi
# The Nightly channel's own icon; stable keeps the hicolor tree.
NIGHTLY_ICON="$ROOT/assets/app-icon/filmcraft-nightly.png"
if [[ "$IS_NIGHTLY" = 1 && ! -f "$NIGHTLY_ICON" ]]; then
  echo "error: the Nightly icon $NIGHTLY_ICON is missing" >&2
  exit 2
fi
BASENAME="filmcraft-$VERSION-linux-$ARCH"

if [[ -n "$ARCH_TAR_DIR" && "$IS_NIGHTLY" != 1 ]]; then
  echo "error: --arch-tar-dir is only for Nightly builds" >&2
  exit 2
fi

echo "==> FilmCraft $VERSION for Linux $ARCH ($FORMATS, channel=${FILMCRAFT_BUILD_CHANNEL:-stable})"

if [[ "$SKIP_BUILD" = 0 ]]; then
  if [[ -n "${CROSS_TARGET:-}" ]]; then
    # The riscv64 cross job is not assumed to build candle/Kokoro. Official native packages
    # include natural voices (downloaded on first use); the UI hides the download when this
    # feature is off (#2318, #3773).
    (cd "$ROOT" && cargo build --release --locked -p filmcraft -p filmcraft-cli --target "$CROSS_TARGET")
  else
    # Official packages include Kokoro natural voices (downloaded on first use). #2318 #3773
    (cd "$ROOT" && cargo build --release --locked -p filmcraft -p filmcraft-cli --features filmcraft/neural-voices)
  fi
fi
BIN="$CARGO_TARGET_DIR/${CROSS_TARGET:+$CROSS_TARGET/}release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- stage an FHS tree (shared by every format) -------------------------------------------------
install -Dm755 "$BIN/filmcraft" "$STAGE/usr/bin/filmcraft"
install -Dm755 "$BIN/filmcraft-cli" "$STAGE/usr/bin/filmcraft-cli"
"${CROSS_COMPILE:-}strip" "$STAGE/usr/bin/filmcraft" "$STAGE/usr/bin/filmcraft-cli" 2>/dev/null || true
install -Dm644 "$HERE/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
if [[ "$IS_NIGHTLY" = 1 ]]; then
  sed -e 's|^StartupWMClass=.*|StartupWMClass=ai.storyteller.filmcraft.nightly-v1|' \
      "$STAGE/usr/share/applications/$APP_ID.desktop" >"$STAGE/usr/share/applications/$APP_ID.desktop.tmp"
  mv "$STAGE/usr/share/applications/$APP_ID.desktop.tmp" "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
install -Dm644 "$HERE/$APP_ID.mime.xml" "$STAGE/usr/share/mime/packages/$APP_ID.xml"
mkdir -p "$STAGE/usr/share/metainfo"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$FILMCRAFT_BUILD_DATE/g" \
  "$HERE/$APP_ID.metainfo.xml.in" >"$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
mkdir -p "$STAGE/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/usr/share/icons/"
mkdir -p "$STAGE/usr/share/doc/filmcraft"
copy_docs "$STAGE/usr/share/doc/filmcraft"

if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net --explain "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
fi

has() { local fmt="$1"; case " $FORMATS " in *" $fmt "*) return 0 ;; *) return 1 ;; esac; }

# ---- .tar.gz ------------------------------------------------------------------------------------
if has tar; then
  mkdir -p "$WORK/tar"
  cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"
  if [[ "$IS_NIGHTLY" = 1 ]]; then
    # A Nightly binary only starts with its private profile, so an unpacked tarball gets the same
    # launcher the other Nightly formats install: `filmcraft` starts `filmcraft-bin`.
    mv "$WORK/tar/$BASENAME/bin/filmcraft" "$WORK/tar/$BASENAME/bin/filmcraft-bin"
    install -Dm755 "$HERE/nightly-launch.sh" "$WORK/tar/$BASENAME/bin/filmcraft"
  fi
  install -m755 "$HERE/install.sh" "$WORK/tar/$BASENAME/install.sh"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# The Arch handoff keeps the same basename as the plain tarball in a separate directory.
# Flatpak, OCI and direct tar consumers still get the original binary, not this launcher.
if [[ -n "$ARCH_TAR_DIR" ]]; then
  mkdir -p "$ARCH_TAR_DIR"
  if [[ "$(cd "$ARCH_TAR_DIR" && pwd -P)" == "$(cd "$DIST" && pwd -P)" ]]; then
    echo "error: the Arch tar directory must differ from the release directory" >&2
    exit 2
  fi
  bash "$HERE/nightly-arch-tar.sh" "$STAGE/usr" "$WORK/arch-tar" "$ARCH_TAR_DIR" "$BASENAME"
fi

# ---- .deb / .rpm --------------------------------------------------------------------------------
if has deb || has rpm; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION
  export NFPM_ARCH="$DEB_ARCH"
  NFPM_STAGE="$STAGE"
  export NFPM_NAME="filmcraft"
  export NFPM_BIN="filmcraft"
  export NFPM_CLI="filmcraft-cli"
  if [[ "$IS_NIGHTLY" = 1 ]]; then
    NIGHTLY_DESK_ID="$APP_ID.nightly-v1"
    export NFPM_NAME="filmcraft-nightly"
    export NFPM_BIN="filmcraft-nightly"
    export NFPM_CLI="filmcraft-nightly-cli"
    NFPM_STAGE="$WORK/nfpm-root"
    rm -rf "$NFPM_STAGE"
    cp -R "$STAGE" "$NFPM_STAGE"
    # The GUI lives off PATH, outside the desktop entry and the icon lookup: /usr/bin/filmcraft-nightly
    # is the launcher that starts it with the Launcher's private profile, as on every other format.
    install -Dm755 "$NFPM_STAGE/usr/bin/filmcraft" "$NFPM_STAGE/usr/lib/filmcraft-nightly/filmcraft"
    cat >"$NFPM_STAGE/usr/bin/$NFPM_BIN" <<'WRAPPER'
#!/bin/sh
# FilmCraft Nightly: the GUI runs with the Launcher's private profile, which holds every per-user
# file (preferences, recovery autosaves, media cache, presets, logs, downloaded models).
set -eu
bin=/usr/lib/filmcraft-nightly/filmcraft
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
    chmod 755 "$NFPM_STAGE/usr/bin/$NFPM_BIN"
    rm -f "$NFPM_STAGE/usr/bin/filmcraft"
    mv "$NFPM_STAGE/usr/bin/filmcraft-cli" "$NFPM_STAGE/usr/bin/$NFPM_CLI"
    mv "$NFPM_STAGE/usr/share/doc/filmcraft" "$NFPM_STAGE/usr/share/doc/$NFPM_NAME"
    sed -e "s#^Exec=filmcraft\([[:space:]]\|$\)#Exec=$NFPM_BIN\1#" \
        -e "s|^TryExec=filmcraft$|TryExec=$NFPM_BIN|" \
        -e "s|^Icon=$APP_ID$|Icon=$NIGHTLY_DESK_ID|" \
        -e "s|^Name=FilmCraft$|Name=FilmCraft Nightly|" \
        -e '/^MimeType=/d' \
        "$NFPM_STAGE/usr/share/applications/$APP_ID.desktop" >"$NFPM_STAGE/usr/share/applications/$NIGHTLY_DESK_ID.desktop"
    rm -f "$NFPM_STAGE/usr/share/applications/$APP_ID.desktop"
    sed -e "s|<id>$APP_ID</id>|<id>$NIGHTLY_DESK_ID</id>|g" \
        -e "s|<name>FilmCraft</name>|<name>FilmCraft Nightly</name>|g" \
        -e "s|<launchable type=\"desktop-id\">$APP_ID.desktop</launchable>|<launchable type=\"desktop-id\">$NIGHTLY_DESK_ID.desktop</launchable>|g" \
        -e "s|<icon type=\"stock\">$APP_ID</icon>|<icon type=\"stock\">$NIGHTLY_DESK_ID</icon>|g" \
        -e "s|<binary>filmcraft</binary>|<binary>$NFPM_BIN</binary>|g" \
        -e '/<mediatype>/d' \
        "$NFPM_STAGE/usr/share/metainfo/$APP_ID.metainfo.xml" >"$NFPM_STAGE/usr/share/metainfo/$NIGHTLY_DESK_ID.metainfo.xml"
    rm -f "$NFPM_STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
    rm -rf "$NFPM_STAGE/usr/share/mime"
    # The Nightly claims no associations, so it registers none and ships its own icon.
    rm -rf "$NFPM_STAGE/usr/share/icons/hicolor"
    install -Dm644 "$NIGHTLY_ICON" "$NFPM_STAGE/usr/share/icons/hicolor/256x256/apps/$NIGHTLY_DESK_ID.png"
  fi
  # nfpm expands env vars in fields like `version` and `arch`, but not in `contents[].src`.
  DROP_LIB_TREE=
  # shellcheck disable=SC2016
  [[ -d "$NFPM_STAGE/usr/lib" ]] || DROP_LIB_TREE='\|^  - src: \${STAGE}/usr/lib/|,+2d;'
  # The drop commands are a prefix of the first expression: with `-e` sed would read them as a file.
  # shellcheck disable=SC2016
  sed "${DROP_LIB_TREE}s|\${STAGE}|$NFPM_STAGE|g;s|\${NFPM_BIN}|$NFPM_BIN|g;s|\${NFPM_CLI}|$NFPM_CLI|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
  for fmt in deb rpm; do
    if has "$fmt"; then (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt"); fi
  done
fi

# ---- AppImage -----------------------------------------------------------------------------------
if has appimage; then
  APPIMAGETOOL_VERSION=1.9.1
  # appimagetool otherwise embeds type2-runtime's unchecked `continuous` build in every AppImage.
  RUNTIME_VERSION=20251108
  case "$ARCH" in
    x86_64)
      APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
      RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
      ;;
    aarch64)
      APPIMAGETOOL_SHA256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
      RUNTIME_SHA256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
      ;;
    *) APPIMAGETOOL_SHA256='' RUNTIME_SHA256='' ;;
  esac
  APPDIR="$WORK/FilmCraft.AppDir"
  cp -R "$STAGE" "$APPDIR"
  if [[ "$IS_NIGHTLY" = 1 ]]; then
    APPIMAGE_ID="$APP_ID.nightly-v1"
    # AppRun is a script here, not a symlink: it registers the Nightly's .desktop entry and icons
    # in $XDG_DATA_HOME before launching, so Wayland compositors resolve the window's app_id to our
    # icon (logic in packaging/linux/apprun-test.sh). Stable keeps its plain symlink below and, as
    # before, installs nothing on start. The desktop id and the channel line below follow: a
    # Nightly AppImage must start the app with the Launcher's private profile, not stable's folders
    # (AppRun's tail does the exec).
    install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
    awk -v old_id="$APP_ID" -v new_id="$APPIMAGE_ID" '
      $0 == "DESK_ID=" old_id { print "DESK_ID=" new_id; next }
      $0 == "CHANNEL=stable" { print "CHANNEL=nightly-v1"; next }
      { print }
    ' "$APPDIR/AppRun" >"$APPDIR/AppRun.tmp"
    grep -qx 'CHANNEL=nightly-v1' "$APPDIR/AppRun.tmp" || {
      echo "error: AppRun has no CHANNEL=stable line to switch for Nightly" >&2
      exit 1
    }
    mv "$APPDIR/AppRun.tmp" "$APPDIR/AppRun"
    chmod 755 "$APPDIR/AppRun"
    sed -e "s|^Icon=$APP_ID$|Icon=$APPIMAGE_ID|" \
        -e "s|^Name=FilmCraft$|Name=FilmCraft Nightly|" \
        -e '/^MimeType=/d' \
        "$APPDIR/usr/share/applications/$APP_ID.desktop" >"$APPDIR/usr/share/applications/$APPIMAGE_ID.desktop"
    rm -f "$APPDIR/usr/share/applications/$APP_ID.desktop"
    cp "$APPDIR/usr/share/applications/$APPIMAGE_ID.desktop" "$APPDIR/$APPIMAGE_ID.desktop"
    # The Nightly claims no associations, so its metainfo must not advertise any either.
    sed -e "s|<id>$APP_ID</id>|<id>$APPIMAGE_ID</id>|g" \
        -e "s|<name>FilmCraft</name>|<name>FilmCraft Nightly</name>|g" \
        -e "s|<launchable type=\"desktop-id\">$APP_ID.desktop</launchable>|<launchable type=\"desktop-id\">$APPIMAGE_ID.desktop</launchable>|g" \
        -e "s|<icon type=\"stock\">$APP_ID</icon>|<icon type=\"stock\">$APPIMAGE_ID</icon>|g" \
        -e '/<mediatype>/d' \
        "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml" >"$APPDIR/usr/share/metainfo/$APPIMAGE_ID.metainfo.xml"
    rm -f "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
    rm -rf "$APPDIR/usr/share/icons/hicolor"
    install -Dm644 "$NIGHTLY_ICON" "$APPDIR/usr/share/icons/hicolor/256x256/apps/$APPIMAGE_ID.png"
    cp "$NIGHTLY_ICON" "$APPDIR/$APPIMAGE_ID.png"
    ln -s "$APPIMAGE_ID.png" "$APPDIR/.DirIcon"
  else
    ln -s usr/bin/filmcraft "$APPDIR/AppRun"
    cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
    cp "$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
    ln -s "$APP_ID.png" "$APPDIR/.DirIcon"
  fi

  if [[ -z "$RUNTIME_SHA256" ]]; then
    echo "no pinned AppImage runtime for $ARCH" >&2
    exit 2
  fi
  RUNTIME="$CARGO_TARGET_DIR/appimage-runtime-$RUNTIME_VERSION-$ARCH"
  if [[ ! -f "$RUNTIME" ]]; then
    download="$RUNTIME.download"
    curl --proto '=https' -fsSL -o "$download" \
      "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-$ARCH"
    printf '%s  %s\n' "$RUNTIME_SHA256" "$download" | sha256sum -c -
    mv "$download" "$RUNTIME"
  fi
  printf '%s  %s\n' "$RUNTIME_SHA256" "$RUNTIME" | sha256sum -c -

  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [[ -z "$TOOL" ]]; then
    if [[ -z "$APPIMAGETOOL_SHA256" ]]; then
      echo "no pinned appimagetool for $ARCH; set APPIMAGETOOL or put appimagetool on PATH" >&2
      exit 2
    fi
    TOOL="$CARGO_TARGET_DIR/appimagetool-$APPIMAGETOOL_VERSION-$ARCH.AppImage"
    if [[ ! -x "$TOOL" ]]; then
      download="$TOOL.download"
      curl --proto '=https' -fsSL -o "$download" \
        "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-$ARCH.AppImage"
      printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$download" | sha256sum -c -
      chmod 755 "$download"
      mv "$download" "$TOOL"
    fi
    printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$TOOL" | sha256sum -c -
  fi
  # Absolute, because appimagetool runs in $DIST below (CARGO_TARGET_DIR or APPIMAGETOOL may be
  # relative, e.g. target/agent-<name>).
  OUT="$(cd "$DIST" && pwd)/$BASENAME.AppImage"
  APPDIR="$(cd "$APPDIR" && pwd)"
  TOOL="$(cd "$(dirname "$TOOL")" && pwd)/$(basename "$TOOL")"
  RUNTIME="$(cd "$(dirname "$RUNTIME")" && pwd)/$(basename "$RUNTIME")"
  # A .zsync left by an earlier run would hide a missing zsyncmake and describe another file.
  rm -f "$OUT.zsync"
  # Update information: AppImageUpdate, AppImageLauncher and the like read it from the file and
  # fetch only the blocks that changed in a newer release, through the .zsync published next to
  # each AppImage on GitHub Releases. `latest` is the newest published release that is not a
  # pre-release. A fork's builds point at its own releases through GITHUB_REPOSITORY.
  REPO="${GITHUB_REPOSITORY:-storytold/filmcraft}"
  UPDATE_ARGS=()
  if [[ "$IS_NIGHTLY" = 0 ]]; then
    UPDATE_INFO="gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest|filmcraft-*-linux-$ARCH.AppImage.zsync"
    UPDATE_ARGS=(-u "$UPDATE_INFO")
  else
    # Nightly releases are GitHub pre-releases (`latest` only selects stable releases), so point
    # zsync update metadata at pre-release tags matching `-nightly` rather than stable `latest`.
    UPDATE_INFO="gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest-pre|filmcraft-*-nightly*-linux-$ARCH.AppImage.zsync"
    UPDATE_ARGS=(-u "$UPDATE_INFO")
  fi
  # Extract-and-run: works without FUSE (containers, CI). The output embeds the static runtime,
  # so users don't need libfuse2 either. With zsyncmake on the host (CI installs the zsync
  # package) appimagetool also writes the .zsync, into its working directory, hence the cd.
  (cd "$DIST" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream --runtime-file "$RUNTIME" "${UPDATE_ARGS[@]}" "$APPDIR" "$OUT")
  echo "wrote $OUT"
  if [[ -s "$OUT.zsync" ]]; then
    echo "wrote $OUT.zsync"
  else
    warn "zsyncmake not found, so $OUT.zsync was not written; AppImage delta updates need it"
  fi
fi

if [[ -z "${CROSS_TARGET:-}" ]]; then
  "$STAGE/usr/bin/filmcraft-cli" --version
elif [[ -n "${EMULATOR:-}" ]]; then
  "$EMULATOR" "$STAGE/usr/bin/filmcraft-cli" --version
fi
echo "==> done"
ls -lh "$DIST"
