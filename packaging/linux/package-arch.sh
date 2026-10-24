#!/usr/bin/env bash
# Build a native Arch package from the already-staged FHS tree.
set -euo pipefail
export LC_ALL=C

if [ "$#" -ne 3 ]; then
    echo 'usage: package-arch.sh STAGE OUTPUT_DIR PKGVER-PKGREL' >&2
    exit 2
fi
stage=$1
out=$2
version=$3
usr=$stage/usr
name=filmcraft
arch=x86_64

if [[ ! "$version" =~ ^([0-9]+:)?[A-Za-z0-9][A-Za-z0-9._+]*-[0-9]+(\.[0-9]+)*$ ]]; then
    echo "invalid Arch pkgver/pkgrel: $version" >&2
    exit 2
fi
for command in readelf bsdtar gzip zstd sha256sum rg sort tail find awk date mv; do
    command -v "$command" >/dev/null 2>&1 || { echo "missing package tool: $command" >&2; exit 1; }
done
[ -d "$usr" ] || { echo "missing staged usr tree: $usr" >&2; exit 1; }
for file in "$usr/lib/filmcraft/filmcraft-bin" "$usr/lib/filmcraft/filmcraft-cli-bin"; do
    [ -f "$file" ] && [ -x "$file" ] || { echo "missing executable: $file" >&2; exit 1; }
    machine=$(readelf -h "$file" 2>/dev/null | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p') || {
        echo "invalid ELF input: $file" >&2; exit 1;
    }
    [ "$machine" = 'Advanced Micro Devices X86-64' ] || {
        echo "wrong ELF architecture for Arch x86_64: $file ($machine)" >&2; exit 1;
    }
done

# Fail closed: every dynamic ELF dependency must have a known Arch package mapping.
declare -A dependencies=()
glibc_floor=
for file in "$usr/lib/filmcraft/filmcraft-bin" "$usr/lib/filmcraft/filmcraft-cli-bin"; do
    dynamic=$(readelf -d "$file" 2>/dev/null) || { echo "cannot read DT_NEEDED: $file" >&2; exit 1; }
    while IFS= read -r soname; do
        [ -n "$soname" ] || continue
        case "$soname" in
            libc.so.*|libm.so.*|libpthread.so.*|libdl.so.*|librt.so.*|libresolv.so.*|ld-linux-x86-64.so.*) dependency=glibc ;;
            libgcc_s.so.*|libstdc++.so.*|libgomp.so.*|libatomic.so.*) dependency=gcc-libs ;;
            libva.so.*|libva-drm.so.*|libva-x11.so.*|libva-wayland.so.*) dependency=libva ;;
            libvulkan.so.*) dependency=vulkan-icd-loader ;;
            libasound.so.*) dependency=alsa-lib ;;
            libxkbcommon-x11.so.*) dependency=libxkbcommon-x11 ;;
            libxkbcommon.so.*) dependency=libxkbcommon ;;
            libX11.so.*|libX11-xcb.so.*) dependency=libx11 ;;
            libxcb.so.1) dependency=libxcb ;;
            libwayland-*.so.*) dependency=wayland ;;
            libEGL.so.*|libGL.so.*|libGLESv2.so.*) dependency=libglvnd ;;
            libdrm.so.*) dependency=libdrm ;;
            libXcursor.so.*) dependency=libxcursor ;;
            libXi.so.*) dependency=libxi ;;
            libXrandr.so.*) dependency=libxrandr ;;
            libXfixes.so.*) dependency=libxfixes ;;
            libXext.so.*) dependency=libxext ;;
            libXrender.so.*) dependency=libxrender ;;
            libXss.so.*) dependency=libxscrnsaver ;;
            libXau.so.*) dependency=libxau ;;
            libXdmcp.so.*) dependency=libxdmcp ;;
            libfontconfig.so.*) dependency=fontconfig ;;
            libfreetype.so.*) dependency=freetype2 ;;
            libz.so.*) dependency=zlib ;;
            libzstd.so.*) dependency=zstd ;;
            libbz2.so.*) dependency=bzip2 ;;
            liblzma.so.*) dependency=xz ;;
            libssl.so.*|libcrypto.so.*) dependency=openssl ;;
            libffi.so.*) dependency=libffi ;;
            libexpat.so.*) dependency=expat ;;
            libdbus-1.so.*) dependency=dbus ;;
            libgio-2.0.so.*|libglib-2.0.so.*|libgobject-2.0.so.*|libgmodule-2.0.so.*) dependency=glib2 ;;
            libepoxy.so.*) dependency=libepoxy ;;
            *) echo "unmapped Arch DT_NEEDED $soname in $file" >&2; exit 1 ;;
        esac
        dependencies["$dependency"]=1
    done < <(printf '%s\n' "$dynamic" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p')

    versions=$(readelf --version-info --wide "$file" 2>/dev/null) || { echo "cannot read ELF symbol versions: $file" >&2; exit 1; }
    versions=$(printf '%s\n' "$versions" | rg -o 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -V | tail -n 1 || true)
    [ -n "$versions" ] || { echo "cannot determine GLIBC floor: $file" >&2; exit 1; }
    if [ -z "$glibc_floor" ] || [ "$(printf '%s\n' "$glibc_floor" "$versions" | sort -V | tail -n 1)" != "$glibc_floor" ]; then
        glibc_floor=$versions
    fi
done

dependencies[glibc]=1
dependencies[gcc-libs]=1
dependencies[vulkan-icd-loader]=1
dependencies[libva]=1
dependencies[alsa-lib]=1
dependencies[libxkbcommon]=1
dependencies[python]=1
dependencies[python-numpy]=1
dependencies[python-scipy]=1

packager=${FILMCRAFT_MAINTAINER:-'FilmCraft maintainers <filmcraft@storyteller.ai>'}
case "$packager" in *$'\n'*|*$'\r'*) echo 'packager must be a single line' >&2; exit 2 ;; esac
builddate=${SOURCE_DATE_EPOCH:-$(date +%s)}
case "$builddate" in ''|*[!0-9]*) echo "invalid SOURCE_DATE_EPOCH: $builddate" >&2; exit 2 ;; esac
size=$(find "$usr" -type f -printf '%s\n' | awk '{ total += $1 } END { print total + 0 }')
pkg="$name-$version-$arch.pkg.tar.zst"
[ -n "$out" ] && [ "$out" != / ] || { echo 'invalid Arch output directory' >&2; exit 2; }
[ ! -e "$out" ] && [ ! -L "$out" ] || { echo "refusing existing Arch output: $out" >&2; exit 1; }
parent=$(dirname -- "$out")
base=$(basename -- "$out")
mkdir -p "$parent"
stage_out=$(mktemp -d "$parent/.$base.stage.XXXXXX")
cleanup() {
    if [ -n "${stage_out:-}" ] && [ -d "$stage_out" ]; then
        rm -rf -- "$stage_out"
    fi
}
trap cleanup EXIT HUP INT TERM

printf '%s\n' \
    'pkgname = filmcraft' \
    'pkgbase = filmcraft' \
    'xdata = pkgtype=pkg' \
    "pkgver = $version" \
    'pkgdesc = Open-source native video editor' \
    'url = https://getartcraft.com/apps/filmcraft' \
    "builddate = $builddate" \
    "packager = $packager" \
    "size = $size" \
    "arch = $arch" \
    'license = MIT' \
    'license = Apache-2.0' > "$stage_out/.PKGINFO"
mapfile -t dependency_names < <(printf '%s\n' "${!dependencies[@]}" | sort -V)
for dependency in "${dependency_names[@]}"; do
    if [ "$dependency" = glibc ]; then
        printf 'depend = glibc>=%s\n' "$glibc_floor" >> "$stage_out/.PKGINFO"
    else
        printf 'depend = %s\n' "$dependency" >> "$stage_out/.PKGINFO"
    fi
done
cat >> "$stage_out/.PKGINFO" <<'EOF_OPTDEP'
optdepend = libx11: X11 desktop session
optdepend = wayland: Wayland desktop session
optdepend = xdg-desktop-portal: portal file dialogs and desktop integration
optdepend = libxcursor: cursor support on X11
optdepend = libxi: input extension support on X11
optdepend = libxrandr: display discovery on X11
optdepend = libva-mesa-driver: VA-API hardware acceleration on Mesa AMD
optdepend = vulkan-radeon: AMD Vulkan driver
EOF_OPTDEP
# Mirrors packaging/linux/postinst.sh: refresh desktop, MIME and icon caches.
cat > "$stage_out/.INSTALL" <<'EOF_INSTALL'
_refresh_caches() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q /usr/share/applications || true
    fi
    if command -v update-mime-database >/dev/null 2>&1; then
        update-mime-database /usr/share/mime || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
    fi
}
post_install() { _refresh_caches; }
post_upgrade() { _refresh_caches; }
post_remove() { _refresh_caches; }
EOF_INSTALL
bsdtar --format=mtree --options='!all,use-set,type,uid,gid,mode,size,time,sha256' \
    --uid 0 --gid 0 --uname root --gname root -cf - \
    -C "$stage_out" .PKGINFO .INSTALL -C "$stage" usr | gzip -n -9 > "$stage_out/.MTREE"
bsdtar --format=ustar --uid 0 --gid 0 --uname root --gname root -cf - \
    -C "$stage_out" .PKGINFO .INSTALL .MTREE -C "$stage" usr | zstd -T1 -q -o "$stage_out/$pkg"
# Metadata files are only inputs to the archive; they must not leak into the published directory.
rm -f -- "$stage_out/.PKGINFO" "$stage_out/.INSTALL" "$stage_out/.MTREE"
chmod 0755 -- "$stage_out"
(cd "$stage_out" && sha256sum "$pkg" > SHA256SUMS && sha256sum -c SHA256SUMS >/dev/null)

# Publish both package and checksum together; -n prevents replacing a raced-in destination.
mv -T -n -- "$stage_out" "$out"
if [ -d "$stage_out" ]; then
    echo "Arch output appeared before no-clobber publication: $out" >&2
    exit 1
fi
stage_out=
printf 'package=%s\nchecksums=%s\n' "$out/$pkg" "$out/SHA256SUMS"
