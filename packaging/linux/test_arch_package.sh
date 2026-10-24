#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/filmcraft-arch-test.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT HUP INT TERM
stage="$tmp/stage"
usr="$stage/usr/lib/filmcraft"
mkdir -p "$usr"
for binary in filmcraft-bin filmcraft-cli-bin; do
    cp /usr/bin/true "$usr/$binary"
done
"$here/package-arch.sh" "$stage" "$tmp/out" 0.11.0-1
pkg="$tmp/out/filmcraft-0.11.0-1-x86_64.pkg.tar.zst"
zstd -t "$pkg" >/dev/null
bsdtar -tf "$pkg" > "$tmp/list"
for entry in .PKGINFO .INSTALL .MTREE usr/lib/filmcraft/filmcraft-bin usr/lib/filmcraft/filmcraft-cli-bin; do
    rg -Fx "$entry" "$tmp/list" >/dev/null
done
bsdtar -xOf "$pkg" .INSTALL | rg -F -q 'update-mime-database'
if rg -q '^DEBIAN(/|$)' "$tmp/list"; then
    echo 'Debian metadata leaked into Arch package' >&2
    exit 1
fi
bsdtar -xOf "$pkg" .PKGINFO > "$tmp/pkginfo"
rg -Fx 'xdata = pkgtype=pkg' "$tmp/pkginfo" >/dev/null
rg -Fx 'arch = x86_64' "$tmp/pkginfo" >/dev/null
rg -Fx 'pkgver = 0.11.0-1' "$tmp/pkginfo" >/dev/null
glibc_floor=$(readelf --version-info --wide /usr/bin/true | rg -o 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -V | tail -n 1)
rg -Fx "depend = glibc>=$glibc_floor" "$tmp/pkginfo" >/dev/null
for dependency in gcc-libs vulkan-icd-loader alsa-lib libva python python-numpy python-scipy; do
    rg -Fx "depend = $dependency" "$tmp/pkginfo" >/dev/null
done
for optional in 'optdepend = libx11:' 'optdepend = wayland:' 'optdepend = xdg-desktop-portal:'; do
    rg -F "$optional" "$tmp/pkginfo" >/dev/null
done
bsdtar -xOf "$pkg" .MTREE | gzip -dc > "$tmp/mtree"
rg -q 'uid=0' "$tmp/mtree"
rg -q 'gid=0' "$tmp/mtree"
pkginfo_mtree=$(rg 'PKGINFO' "$tmp/mtree")
printf '%s\n' "$pkginfo_mtree" | rg -q 'sha256digest=[0-9a-f]{64}'
if rg -q 'MTREE' "$tmp/mtree"; then
    echo '.MTREE must not list itself' >&2
    exit 1
fi
(cd "$tmp/out" && sha256sum -c SHA256SUMS)
[ "$(find "$tmp/out" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ')" = 'SHA256SUMS filmcraft-0.11.0-1-x86_64.pkg.tar.zst ' ]
sums_before=$(sha256sum "$tmp/out/SHA256SUMS")
if "$here/package-arch.sh" "$stage" "$tmp/out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'overwrote existing Arch output' >&2
    exit 1
fi
[ "$(sha256sum "$tmp/out/SHA256SUMS")" = "$sums_before" ]

mkdir -p "$tmp/fake-bin"
cat > "$tmp/fake-bin/readelf" <<'SH'
#!/usr/bin/env sh
case "$1:$FAKE_READELF_MODE" in
    -h:wrong-arch) printf '  Machine:                           AArch64\n' ;;
    -h:*) printf '  Machine:                           Advanced Micro Devices X86-64\n' ;;
    -d:unknown) printf ' 0x0000000000000001 (NEEDED)             Shared library: [libunmapped.so.7]\n' ;;
    -d:ambiguous) printf ' 0x0000000000000001 (NEEDED)             Shared library: [libxcb-cursor.so.0]\n'; printf ' 0x0000000000000001 (NEEDED)             Shared library: [libnss_systemd.so.2]\n' ;;
    -d:known) printf ' 0x0000000000000001 (NEEDED)             Shared library: [libxcb.so.1]\n' ;;
    -d:*) printf ' 0x0000000000000001 (NEEDED)             Shared library: [libva-drm.so.2]\n'; printf ' 0x0000000000000001 (NEEDED)             Shared library: [libvulkan.so.1]\n' ;;
    --version-info:*) printf '  Name: GLIBC_2.39 (GLIBC_2.39)\n' ;;
    *) exit 2 ;;
esac
SH
chmod 0755 "$tmp/fake-bin/readelf"
FAKE_READELF_MODE=known PATH="$tmp/fake-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/mapped" 0.11.0-1
bsdtar -xOf "$tmp/mapped/filmcraft-0.11.0-1-x86_64.pkg.tar.zst" .PKGINFO > "$tmp/mapped-pkginfo"
rg -Fx 'depend = glibc>=2.39' "$tmp/mapped-pkginfo" >/dev/null
rg -Fx 'depend = libva' "$tmp/mapped-pkginfo" >/dev/null
rg -Fx 'depend = vulkan-icd-loader' "$tmp/mapped-pkginfo" >/dev/null
FAKE_READELF_MODE=known PATH="$tmp/fake-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/libxcb-exact-out" 0.11.0-1
bsdtar -xOf "$tmp/libxcb-exact-out/filmcraft-0.11.0-1-x86_64.pkg.tar.zst" .PKGINFO | rg -Fx 'depend = libxcb' >/dev/null
if FAKE_READELF_MODE=ambiguous PATH="$tmp/fake-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/ambiguous-soname-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'accepted unverified libxcb-cursor/libnss_systemd soname mapping' >&2
    exit 1
fi
[ ! -e "$tmp/ambiguous-soname-out" ]
if FAKE_READELF_MODE=unknown PATH="$tmp/fake-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/unknown-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'accepted unmapped DT_NEEDED' >&2
    exit 1
fi
[ ! -e "$tmp/unknown-out" ]
if FAKE_READELF_MODE=wrong-arch PATH="$tmp/fake-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/wrong-arch-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'accepted wrong ELF architecture' >&2
    exit 1
fi
[ ! -e "$tmp/wrong-arch-out" ]

bad="$tmp/bad-stage"
mkdir -p "$bad/usr/lib/filmcraft"
cp /usr/bin/true "$bad/usr/lib/filmcraft/filmcraft-bin"
printf 'not an ELF\n' > "$bad/usr/lib/filmcraft/filmcraft-cli-bin"
if "$here/package-arch.sh" "$bad" "$tmp/bad-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'accepted invalid ELF input' >&2
    exit 1
fi
[ ! -e "$tmp/bad-out" ]
if "$here/package-arch.sh" "$stage" "$tmp/bad-version-out" 0.11.0-1+debian-test >/dev/null 2>&1; then
    echo 'accepted Debian +revision as an Arch pkgrel' >&2
    exit 1
fi
[ ! -e "$tmp/bad-version-out" ]
"$here/package-arch.sh" "$stage" "$tmp/epoch-version-out" 1:0.11.0-1.2 >/dev/null
bsdtar -xOf "$tmp/epoch-version-out/filmcraft-1:0.11.0-1.2-x86_64.pkg.tar.zst" .PKGINFO | rg -Fx 'pkgver = 1:0.11.0-1.2' >/dev/null

mkdir -p "$tmp/hash-failure-bin"
cat > "$tmp/hash-failure-bin/sha256sum" <<'SH'
#!/usr/bin/env sh
exit 41
SH
chmod 0755 "$tmp/hash-failure-bin/sha256sum"
if PATH="$tmp/hash-failure-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/hash-failure-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'ignored injected checksum failure' >&2
    exit 1
fi
[ ! -e "$tmp/hash-failure-out" ]
if find "$tmp" -maxdepth 1 -name '.hash-failure-out.stage.*' -print -quit | rg -q .; then
    echo 'left a checksum-failure staging directory' >&2
    exit 1
fi
"$here/package-arch.sh" "$stage" "$tmp/hash-failure-out" 0.11.0-1 >/dev/null

mkdir -p "$tmp/publish-failure-bin"
cat > "$tmp/publish-failure-bin/mv" <<'SH'
#!/usr/bin/env sh
exit 42
SH
chmod 0755 "$tmp/publish-failure-bin/mv"
if PATH="$tmp/publish-failure-bin:/usr/bin:/bin" "$here/package-arch.sh" "$stage" "$tmp/publish-failure-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'ignored injected publication failure' >&2
    exit 1
fi
[ ! -e "$tmp/publish-failure-out" ]
if find "$tmp" -maxdepth 1 -name '.publish-failure-out.stage.*' -print -quit | rg -q .; then
    echo 'left a publication-failure staging directory' >&2
    exit 1
fi
"$here/package-arch.sh" "$stage" "$tmp/publish-failure-out" 0.11.0-1 >/dev/null

mkdir -p "$tmp/preexisting-out"
printf 'preserve-me\n' > "$tmp/preexisting-out/sentinel"
if "$here/package-arch.sh" "$stage" "$tmp/preexisting-out" 0.11.0-1 >/dev/null 2>&1; then
    echo 'replaced a pre-existing Arch output directory' >&2
    exit 1
fi
rg -Fx 'preserve-me' "$tmp/preexisting-out/sentinel" >/dev/null
if FILMCRAFT_ARCH_PACKAGE_DIR="$tmp/default-arch" bash "$here/build-deb.sh" >/dev/null 2>&1; then
    echo 'accepted Arch option with default multi-architecture Debian selection' >&2
    exit 1
fi
[ ! -e "$tmp/default-arch" ]
printf 'Arch package synthetic tests passed.\n'
