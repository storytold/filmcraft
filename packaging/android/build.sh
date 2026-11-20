#!/usr/bin/env bash
# Build a signed native APK from the local Cargo workspace using the Android NDK and SDK.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"

: "${ANDROID_SDK_ROOT:?Set ANDROID_SDK_ROOT to the Android SDK}"
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to Android NDK r28 or newer}"
: "${FILMCRAFT_ANDROID_KEYSTORE:?Set FILMCRAFT_ANDROID_KEYSTORE to your signing keystore}"
: "${FILMCRAFT_ANDROID_KEY_ALIAS:?Set FILMCRAFT_ANDROID_KEY_ALIAS to the signing key alias}"
: "${FILMCRAFT_ANDROID_KEYSTORE_PASSWORD:?Set FILMCRAFT_ANDROID_KEYSTORE_PASSWORD}"

ndk="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/${FILMCRAFT_NDK_HOST:-linux-x86_64}/bin"
tools="${FILMCRAFT_ANDROID_BUILD_TOOLS:-$ANDROID_SDK_ROOT/build-tools/36.0.0}"
platform="$ANDROID_SDK_ROOT/platforms/android-36/android.jar"
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
out="${FILMCRAFT_ANDROID_OUT:-$target_dir/android-apk}"
profile="${FILMCRAFT_ANDROID_PROFILE:-release}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
stage="$(mktemp -d "$out/.build.XXXXXXXX")"
trap 'rm -rf -- "$stage"' EXIT

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)"
IFS=. read -r major minor patch <<< "$version"
version_code="$((major * 1000000 + minor * 1000 + patch))"
version_code="${FILMCRAFT_ANDROID_VERSION_CODE:-$version_code}"

mkdir -p "$stage/res/drawable-nodpi" "$stage/assets/licenses"
cp assets/app-icon/hicolor/256x256/apps/ai.storyteller.filmcraft.png "$stage/res/drawable-nodpi/filmcraft.png"
cp LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md "$stage/assets/licenses/"
cp assets/fonts/OFL-*.txt "$stage/assets/licenses/"
if [[ -n "${CRAFT_FONTS_DIR:-}" ]]; then
    python3 - "$CRAFT_FONTS_DIR" "$stage/assets/licenses" <<'PY'
from pathlib import Path
from shutil import copyfile
import sys

source, output = (Path(value).resolve() for value in sys.argv[1:])
manifest = source / "fonts/manifest.txt"
copyfile(manifest, output / "fonts.txt")
for line in manifest.read_text().splitlines():
    if not line.strip() or line.lstrip().startswith("#"):
        continue
    _, _, _, _, _, relative, *_ = (field.strip() for field in line.split(" | "))
    license_file = (source / relative).resolve()
    if not license_file.is_relative_to(source):
        raise ValueError("font license path must stay inside CRAFT_FONTS_DIR")
    filename = f"{license_file.parent.name}-{license_file.name}"
    copyfile(license_file, output / filename)
PY
fi
"$tools/aapt2" compile --dir "$stage/res" -o "$stage/res.zip"
"$tools/aapt2" link -I "$platform" --manifest packaging/android/AndroidManifest.xml \
    --min-sdk-version 26 --target-sdk-version 36 --version-code "$version_code" \
    --version-name "$version" -A "$stage/assets" -o "$stage/base.apk" "$stage/res.zip"

mkdir -p "$stage/classes" "$stage/dex"
java -m jdk.compiler/com.sun.tools.javac.Main -source 8 -target 8 -bootclasspath "$platform" \
    -d "$stage/classes" packaging/android/FilmCraftActivity.java
"$tools/d8" --min-api 26 --lib "$platform" --output "$stage/dex" \
    "$stage/classes/org/filmcraft/googlebook/FilmCraftActivity.class"

build_abi() {
    local abi="$1" triple="$2" clang="$ndk/$2"26-clang env_triple
    env_triple="${triple//-/_}"
    env "CARGO_TARGET_${env_triple^^}_LINKER=$clang" \
        "CC_$env_triple=$clang" "CXX_$env_triple=$clang++" "AR_$env_triple=$ndk/llvm-ar" \
        cargo build --locked --profile "$profile" --target "$triple" -p filmcraft-android
    mkdir -p "$stage/lib/$abi"
    "$ndk/llvm-strip" --strip-unneeded "$target_dir/$triple/$profile/libfilmcraft_android.so" \
        -o "$stage/lib/$abi/libfilmcraft_android.so"
}

for abi in ${FILMCRAFT_ANDROID_ABIS:-arm64-v8a x86_64}; do
    case "$abi" in
        arm64-v8a) build_abi "$abi" aarch64-linux-android ;;
        x86_64) build_abi "$abi" x86_64-linux-android ;;
        *) printf 'Unknown Android ABI: %s\n' "$abi" >&2; exit 2 ;;
    esac
done

# extractNativeLibs=false requires libraries to be stored and page-aligned in the APK.
python3 - "$stage" <<'PY'
from pathlib import Path
import sys
from zipfile import ZIP_STORED, ZipFile

stage = Path(sys.argv[1])
with ZipFile(stage / "base.apk", "a", compression=ZIP_STORED) as apk:
    apk.write(stage / "dex/classes.dex", "classes.dex")
    for library in sorted((stage / "lib").glob("*/*.so")):
        apk.write(library, library.relative_to(stage).as_posix())
        library.unlink()
PY

"$tools/zipalign" -P 16 -f 4 "$stage/base.apk" "$stage/aligned.apk"
rm -- "$stage/base.apk"
"$tools/apksigner" sign --ks "$FILMCRAFT_ANDROID_KEYSTORE" \
    --ks-key-alias "$FILMCRAFT_ANDROID_KEY_ALIAS" --ks-pass env:FILMCRAFT_ANDROID_KEYSTORE_PASSWORD \
    --out "$stage/FilmCraft-GoogleBook.apk" "$stage/aligned.apk"
"$tools/apksigner" verify "$stage/FilmCraft-GoogleBook.apk"
"$tools/zipalign" -c -P 16 4 "$stage/FilmCraft-GoogleBook.apk"
mv "$stage/FilmCraft-GoogleBook.apk" "$out/FilmCraft-GoogleBook.apk"
printf 'APK saved to %s/FilmCraft-GoogleBook.apk\n' "$out"
