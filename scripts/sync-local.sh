#!/usr/bin/env bash
# scripts/sync-local.sh
# Synchronizes local/main integration branch with upstream/main and active feature branches,
# then compiles and deploys the release binary to ~/.local/bin/filmcraft.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

echo "==> Fetching upstream..."
git fetch upstream

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
TARGET_FEATURE="${1:-}"

echo "==> Switching to local/main..."
git checkout local/main

echo "==> Merging upstream/main into local/main..."
git merge --no-edit upstream/main

if [ -n "$TARGET_FEATURE" ]; then
    echo "==> Merging feature branch '$TARGET_FEATURE' into local/main..."
    git merge --no-edit "$TARGET_FEATURE"
fi

echo "==> Compiling release binary..."
cargo build --release -p filmcraft

echo "==> Installing to ~/.local/bin/filmcraft..."
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/filmcraft "$HOME/.local/bin/filmcraft"

echo "==> Verifying binary..."
"$HOME/.local/bin/filmcraft" --help >/dev/null

echo "==> Restoring branch: $CURRENT_BRANCH"
if [ "$CURRENT_BRANCH" != "local/main" ]; then
    git checkout "$CURRENT_BRANCH"
fi

echo "==> Local FilmCraft successfully updated and installed to ~/.local/bin/filmcraft!"
