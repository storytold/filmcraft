#!/usr/bin/env bash
# Build the browser version and zip it:  $DIST/filmcraft-web-<version>.zip
#
# Usage: packaging/web/package.sh [--skip-build]
#
# Needs: the wasm32-unknown-unknown target and the wasm-bindgen CLI at the crate's exact version
# (cargo install wasm-bindgen-cli --version <see xtask WASM_BINDGEN> --locked); `cargo xtask web` builds it. The zip holds a self-contained static site in filmcraft-web-<version>/ that works
# from any URL path and inside an <iframe>. Hosting notes: packaging/web/README.md.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/web"

if [[ "${1:-}" != "--skip-build" ]]; then
  # CI (the shared release workflow installs only trunk): install the wasm-bindgen CLI at the
  # crate's exact version. Locally, `cargo xtask web` says which version to install.
  if [[ -n "${CI:-}" ]]; then
    WASM_BINDGEN_VERSION="$(sed -n 's/^const WASM_BINDGEN: &str = "\(.*\)";$/\1/p' "$ROOT/xtask/src/main.rs")"
    if ! { command -v wasm-bindgen >/dev/null && wasm-bindgen --version | grep -qF "$WASM_BINDGEN_VERSION"; }; then
      cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked
    fi
  fi
  (cd "$ROOT" && cargo xtask web)
fi

SITE="$CARGO_TARGET_DIR/web/dist"
[[ -f "$SITE/index.html" ]] || { echo "error: $SITE/index.html missing; run without --skip-build" >&2; exit 1; }
# Paths must be relative so the site works under any prefix.
if grep -Eq '(src|href)="/[^/]' "$SITE/index.html"; then
  echo "error: $SITE/index.html has root-absolute URLs; it would break when served from a sub-path" >&2
  exit 1
fi

NAME="filmcraft-web-$VERSION"
WORK="$CARGO_TARGET_DIR/web-package"
rm -rf "$WORK"
mkdir -p "$WORK/$NAME"
cp -R "$SITE/." "$WORK/$NAME/"
# Sample server configs (MIME type, caching, compression); harmless where unused.
cp "$HERE/_headers" "$HERE/.htaccess" "$WORK/$NAME/"
cp "$HERE/README.md" "$WORK/$NAME/HOSTING.md"
copy_docs "$WORK/$NAME"
rm -f "$DIST/$NAME.zip"
(cd "$WORK" && zip -qr9 "$DIST/$NAME.zip" "$NAME")
echo "wrote $DIST/$NAME.zip"
ls -lh "$DIST/$NAME.zip"
