#!/usr/bin/env bash
# Package real x86_64 and AArch64 builds supplied in separate directories.
set -eu
set -o pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$here/../env.sh"
repo_root=$(CDPATH= cd -- "$here/../../.." && pwd)
out=${FILMCRAFT_DEB_DIR:-$DIST/deb}
arch_package_dir=${FILMCRAFT_ARCH_PACKAGE_DIR:-}
arch_list=${FILMCRAFT_DEB_ARCHES:-'x86_64 aarch64'}
read -r -a architectures <<< "$arch_list"
if [ "${#architectures[@]}" -eq 0 ]; then
    echo 'FILMCRAFT_DEB_ARCHES must select x86_64, aarch64, or both.' >&2
    exit 2
fi
declare -A selected_arches=()
for arch in "${architectures[@]}"; do
    case "$arch" in x86_64|aarch64) ;; *) echo "unsupported package architecture: $arch" >&2; exit 2 ;; esac
    if [ -n "${selected_arches[$arch]+x}" ]; then
        echo "duplicate package architecture: $arch" >&2
        exit 2
    fi
    selected_arches[$arch]=1
done
if [ -n "$arch_package_dir" ]; then
    export LC_ALL=C
    if [ "${#architectures[@]}" -ne 1 ] || [ "${architectures[0]}" != x86_64 ]; then
        echo 'FILMCRAFT_ARCH_PACKAGE_DIR requires FILMCRAFT_DEB_ARCHES=x86_64.' >&2
        exit 2
    fi
    for command in bsdtar gzip zstd sha256sum; do
        command -v "$command" >/dev/null 2>&1 || { echo "missing Arch package tool: $command" >&2; exit 1; }
    done
fi
x86_bins=${FILMCRAFT_BIN_DIR_X86_64:-}
arm_bins=${FILMCRAFT_BIN_DIR_AARCH64:-}
work_base=${FILMCRAFT_DEB_WORK:-$CARGO_TARGET_DIR/deb-multiarch}
version=${FILMCRAFT_PACKAGE_VERSION:-$VERSION-1}
arch_version=${FILMCRAFT_ARCH_PACKAGE_VERSION:-$VERSION-1}
app_id=ai.storyteller.filmcraft

for arch in "${architectures[@]}"; do
    case "$arch" in
        x86_64) values=("$x86_bins") ;;
        aarch64) values=("$arm_bins") ;;
    esac
    for value in "${values[@]}"; do
        [ -n "$value" ] || {
            echo "Set the real binary directory for selected architecture $arch." >&2
            exit 2
        }
    done
done
command -v readelf >/dev/null 2>&1 || { echo 'readelf is required to verify ELF architectures and dependencies.' >&2; exit 1; }
command -v dpkg-deb >/dev/null 2>&1 || { echo 'dpkg-deb is required.' >&2; exit 1; }
command -v sort >/dev/null 2>&1 || { echo 'sort is required.' >&2; exit 1; }
command -v rg >/dev/null 2>&1 || { echo 'ripgrep is required.' >&2; exit 1; }

case "$version" in ''|*[!0-9A-Za-z.+:~-]*) echo "invalid package version: $version" >&2; exit 2 ;; esac

depends_for() {
    # Map each NEEDED soname to its Debian runtime package; unknown libraries fail closed.
    for file do
        readelf -d "$file" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p'
    done | sort -u | while IFS= read -r soname; do
        case "$soname" in
            libc.so.6|libm.so.6|libstdc++.so.6|ld-linux-x86-64.so.2|ld-linux-aarch64.so.1) package= ;;
            libgcc_s.so.1) package=libgcc-s1 ;;
            libgomp.so.1) package=libgomp1 ;;
            libxkbcommon.so.0) package=libxkbcommon0 ;;
            libvulkan.so.1) package=libvulkan1 ;;
            libEGL.so.1) package=libegl1 ;;
            libasound.so.2) package='libasound2 | libasound2t64' ;;
            libva.so.2) package=libva2 ;;
            libva-drm.so.2) package=libva-drm2 ;;
            libdrm.so.2) package=libdrm2 ;;
            libxcb.so.1) package=libxcb1 ;;
            libX11.so.6) package=libx11-6 ;;
            libX11-xcb.so.1) package=libx11-xcb1 ;;
            libwayland-client.so.0) package=libwayland-client0 ;;
            *) echo "unmapped ELF dependency: $soname" >&2; exit 1 ;;
        esac
        [ -z "$package" ] || printf '%s\n' "$package"
    done | sort -u | paste -sd ',' -
}

max_symbol_version() {
    prefix=$1
    shift
    result=$(for file do
        readelf --version-info --wide "$file" | sed -n "s/.*Name: \($prefix[0-9.]*\).*/\1/p"
    done | sort -Vu | tail -n 1)
    [ -n "$result" ] || { echo "cannot prove required $prefix version" >&2; return 1; }
    printf '%s\n' "$result"
}

package_arch() {
    rust_arch=$1 deb_arch=$2 expected=$3 bins=$4
    for name in filmcraft filmcraft-cli; do
        [ -f "$bins/$name" ] || { echo "missing $rust_arch binary: $bins/$name" >&2; return 1; }
    done
    for file in "$bins/filmcraft" "$bins/filmcraft-cli"; do
        machine=$(readelf -h "$file" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')
        case "$expected:$machine" in
            X86-64:*X86-64*) ;;
            AArch64:*AArch64*) ;;
            *) echo "wrong ELF architecture for $rust_arch: $file ($machine)" >&2; return 1 ;;
        esac
    done
    glibc_floor=$(max_symbol_version GLIBC_ "$bins/filmcraft" "$bins/filmcraft-cli")
    glibc_floor=${glibc_floor#GLIBC_}

    stage=$work/$rust_arch/root
    rm -rf -- "$stage"
    install -d "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" \
        "$stage/usr/share/mime/packages" "$stage/usr/share/icons" \
        "$stage/usr/share/doc/filmcraft" "$stage/usr/lib/filmcraft"
    install -m 0755 "$bins/filmcraft" "$stage/usr/lib/filmcraft/filmcraft-bin"
    install -m 0755 "$bins/filmcraft-cli" "$stage/usr/lib/filmcraft/filmcraft-cli-bin"
    install -m 0644 "$ROOT/NOTICE" "$ROOT/ATTRIBUTION.md" \
        "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$stage/usr/share/doc/filmcraft/"
    cp -R "$here/../../assets/app-icon/hicolor" "$stage/usr/share/icons/"
    install -m 0644 "$here/$app_id.desktop" "$stage/usr/share/applications/$app_id.desktop"
    install -m 0644 "$here/$app_id.mime.xml" "$stage/usr/share/mime/packages/$app_id.xml"

    for pair in 'filmcraft filmcraft-bin' 'filmcraft-cli filmcraft-cli-bin'; do
        set -- $pair
        cat > "$stage/usr/bin/$1" <<EOF
#!/bin/sh
prefix=\$(CDPATH= cd -- "\$(dirname -- "\$0")/.." && pwd)
exec "\$prefix/lib/filmcraft/$2" "\$@"
EOF
        chmod 0755 "$stage/usr/bin/$1"
    done

    depends=$(depends_for "$bins/filmcraft" "$bins/filmcraft-cli")
    cat > "$stage/DEBIAN/control" <<EOF
Package: filmcraft
Version: $version
Section: video
Priority: optional
Architecture: $deb_arch
Maintainer: FilmCraft maintainers <filmcraft@storyteller.ai>
Depends: $depends, libc6 (>= $glibc_floor), libvulkan1, libva2, libva-drm2
Description: Open-source native video editor
 Timeline editing, color grading, effects, audio mixing and export.
EOF
    cat > "$stage/DEBIAN/postinst" <<'EOF'
#!/bin/sh
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q /usr/share/applications || true; fi
if command -v update-mime-database >/dev/null 2>&1; then update-mime-database /usr/share/mime || true; fi
exit 0
EOF
    chmod 0755 "$stage/DEBIAN/postinst"
    installed_size=$(du -sk "$stage/usr" | awk '{print $1}')
    sed -i "s/^Priority: optional$/Priority: optional\nInstalled-Size: $installed_size/" "$stage/DEBIAN/control"
    (cd "$stage" && find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
    mkdir -p "$work/packages/$rust_arch"
    dpkg-deb --root-owner-group --threads-max=1 -Zgzip -z6 --build "$stage" "$work/packages/$rust_arch/filmcraft-$version-linux-$rust_arch.deb"
    if [ -n "$arch_package_dir" ] && [ "$rust_arch" = x86_64 ]; then
        "$here/package-arch.sh" "$stage" "$arch_package_dir" "$arch_version"
    fi
}

mkdir -p "$work_base" "$out"
work=$(mktemp -d "$work_base/build.XXXXXX")
trap 'rm -rf -- "$work"' EXIT HUP INT TERM
for arch in "${architectures[@]}"; do
    case "$arch" in
        x86_64) package_arch x86_64 amd64 X86-64 "$x86_bins" ;;
        aarch64) package_arch aarch64 arm64 AArch64 "$arm_bins" ;;
    esac
done
checksum_inputs=()
for arch in "${architectures[@]}"; do
    package_name=filmcraft-$version-linux-$arch.deb
    mkdir -p "$out/$arch"
    mv -f "$work/packages/$arch/$package_name" "$out/$arch/$package_name"
    checksum_inputs+=("$arch/$package_name")
done
(cd "$out" && sha256sum "${checksum_inputs[@]}" > SHA256SUMS.tmp && mv -f SHA256SUMS.tmp SHA256SUMS)
printf 'packages=%s\nchecksums=%s/SHA256SUMS\n' \
    "$(printf '%s ' "${checksum_inputs[@]}")" "$out"
