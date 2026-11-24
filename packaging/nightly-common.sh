# shellcheck shell=bash
# Shared setup for app-owned native nightly archives; stable packaging does not source this file.

nightly_die() { printf 'error: %s\n' "$*" >&2; exit 2; }

nightly_init() {
  local os="$1" arch="$2" target="$3" date_digits
  [[ "${FILMCRAFT_BUILD_CHANNEL:-}" == nightly-v1 ]] || nightly_die 'FILMCRAFT_BUILD_CHANNEL must be nightly-v1'
  [[ "${FILMCRAFT_VERSION:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\.[0-9]+\.g[0-9a-f]{12}$ ]] || nightly_die 'an exact prepared nightly version is required'
  [[ "${FILMCRAFT_BUILD_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || nightly_die 'an exact source commit is required'
  [[ "${FILMCRAFT_VERSION##*.g}" == "${FILMCRAFT_BUILD_SHA:0:12}" ]] || nightly_die 'version/source commit mismatch'
  [[ "${FILMCRAFT_BUILD_DATE:-}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || nightly_die 'the original run-created UTC date is required'
  date_digits="${FILMCRAFT_VERSION#*-nightly.}"
  date_digits="${date_digits%%.*}"
  [[ "${FILMCRAFT_BUILD_DATE//-/}" == "$date_digits" ]] || nightly_die 'version/original-run date mismatch'
  [[ "${CARGO_TARGET_DIR:-}" == /* && "$CARGO_TARGET_DIR" != / ]] || nightly_die 'an absolute owned CARGO_TARGET_DIR base is required'
  [[ "${DIST:-}" == /* && "$DIST" != / ]] || nightly_die 'an absolute DIST output directory is required'
  [[ "/$CARGO_TARGET_DIR/" != *'/../'* && "/$DIST/" != *'/../'* ]] || nightly_die 'output paths must not contain parent traversal'
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$FILMCRAFT_BUILD_SHA" ]] || nightly_die 'prepared source is not the checked-out commit'
  [[ -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'nightly packaging requires the frozen clean source'
  if [[ -n "${CRAFT_FONTS_DIR:-}" && ! -d "$CRAFT_FONTS_DIR/fonts" ]]; then
    nightly_die 'declared CRAFT_FONTS_DIR has no fonts directory'
  fi
  # env.sh retains the established notice copier. Required values above prohibit its stable fallbacks.
  . "$ROOT/packaging/env.sh"
  export CARGO_TARGET_DIR="$CARGO_TARGET_DIR/nightly-v1/$os-$arch"
  NIGHTLY_RUST_TARGET="$target"
  mkdir -p "$CARGO_TARGET_DIR"
  NIGHTLY_WORK="$(mktemp -d "$CARGO_TARGET_DIR/nightly-package.XXXXXX")"
  trap 'rm -rf -- "$NIGHTLY_WORK"' EXIT
}

# Compile only: macOS runs the version query after signing (packaging/macos/nightly-verify.sh).
nightly_compile_gui() {
  # Signing material never reaches compilation (the Windows -CheckBuildEnvironment rule).
  nightly_refuse_signing_env
  # The same prepared identity the artifacts are named after: build.rs refuses a nightly whose
  # version, commit and date disagree.
  # The same features the official package builds with (packaging/linux/package.sh): without
  # natural voices the Nightly would quietly have fewer voices than stable.
  (cd "$ROOT" && FILMCRAFT_VERSION="$VERSION" FILMCRAFT_BUILD_CHANNEL=nightly-v1 cargo build --release --locked \
    -p filmcraft --features filmcraft/neural-voices --target "$NIGHTLY_RUST_TARGET")
  NIGHTLY_GUI="$CARGO_TARGET_DIR/$NIGHTLY_RUST_TARGET/release/filmcraft"
  [[ -f "$NIGHTLY_GUI" && ! -L "$NIGHTLY_GUI" ]] || nightly_die 'the owned GUI build did not produce a regular executable'
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$FILMCRAFT_BUILD_SHA" && -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'source changed during the build'
}

nightly_build_gui() {
  nightly_compile_gui
  local actual
  actual="$("$NIGHTLY_GUI" --version)" || nightly_die 'side-effect-free nightly version verification failed'
  [[ "$actual" == "FilmCraft $VERSION (nightly-v1)" ]] || nightly_die 'the built version/flavor is not the prepared identity'
  [[ "$(git -C "$ROOT" rev-parse HEAD)" == "$FILMCRAFT_BUILD_SHA" && -z "$(git -C "$ROOT" status --porcelain)" ]] || nightly_die 'source changed during the version query'
}

# Signing material never reaches compilation (the Windows -CheckBuildEnvironment rule).
nightly_refuse_signing_env() {
  local name
  # The signing names release.yml passes, for every platform, and import-cert.sh's outputs.
  for name in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD KEYCHAIN_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID \
    MACOS_SIGN_IDENTITY MACOS_KEYCHAIN WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD AZURE_TENANT_ID AZURE_CLIENT_ID \
    AZURE_CLIENT_SECRET AZURE_SIGNING_ENDPOINT AZURE_SIGNING_ACCOUNT AZURE_CERT_PROFILE LINUX_SIGNING_KEY; do
    [[ -z "${!name:-}" ]] || nightly_die "compilation must not receive signing configuration ($name); sign in the separate trusted step"
  done
}

nightly_copy_notices() {
  local destination="$1" required
  for required in README.md LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
    [[ -f "$ROOT/$required" && ! -L "$ROOT/$required" ]] || nightly_die "missing regular app notice: $required"
  done
  copy_docs "$destination"
  # `copy_docs` (env.sh) copies the licence and README only; nightly-archive.py and
  # nightly-check.sh require ATTRIBUTION.md in the tar and the macOS bundle.
  cp "$ROOT/ATTRIBUTION.md" "$destination/"
}

nightly_publish_archive() {
  # Exclusive creation: never replace an existing artifact, even if another writer raced the preflight.
  python3 -I - "$1" "$2" <<'PY'
import os, shutil, sys
source, destination = sys.argv[1:]
created = False
try:
    # Open the input first: a missing source must not leave an empty final artifact.
    with open(source, 'rb') as incoming:
        with open(destination, 'xb') as output:
            created = True
            shutil.copyfileobj(incoming, output, length=1024 * 1024)
            output.flush()
            os.fsync(output.fileno())
except BaseException:
    if created:
        os.unlink(destination)
    raise
PY
  printf 'Nightly artifact: %s\n' "$2"
}
