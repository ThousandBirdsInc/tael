#!/usr/bin/env bash
set -euo pipefail

# Publish tael crates to crates.io — MANUAL FALLBACK ONLY.
#
# Releases are normally fully automated: run the `cut-release` workflow from
# the GitHub Actions tab, which bumps the version, tags, builds binaries,
# publishes to crates.io, and pushes the Docker image. See docs/releasing.md.
# Use this script only if the CI publish job is broken.

# Ensure we're in the workspace root
if [[ ! -f Cargo.toml ]]; then
  echo "Error: must be run from the workspace root"
  exit 1
fi

# Check for uncommitted changes
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: there are uncommitted changes. Commit before publishing."
  echo ""
  git status --short
  exit 1
fi

# Dry run and publish libraries first. tael-cli's packaged verification resolves
# tael-server and tael-gui from crates.io, so the new library versions must be
# indexed before the CLI dry-run can use new APIs.
echo "==> Running dry-run for tael-server..."
cargo publish -p tael-server --dry-run

echo ""
read -p "tael-server dry run passed. Publish tael-server to crates.io? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

echo ""
echo "==> Publishing tael-server..."
cargo publish -p tael-server

echo "==> Waiting for crates.io to index tael-server..."
sleep 15

echo ""
echo "==> Running dry-run for tael-gui..."
cargo publish -p tael-gui --dry-run

echo ""
read -p "tael-gui dry run passed. Publish tael-gui to crates.io? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

echo ""
echo "==> Publishing tael-gui..."
cargo publish -p tael-gui

echo "==> Waiting for crates.io to index tael-gui..."
sleep 15

echo ""
echo "==> Running dry-run for tael-cli..."
cargo publish -p tael-cli --dry-run

echo ""
read -p "tael-cli dry run passed. Publish tael-cli to crates.io? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

echo ""
echo "==> Publishing tael-cli..."
cargo publish -p tael-cli

echo ""
echo "Done. Published tael-server, tael-gui, and tael-cli v$(cargo metadata --no-deps --format-version 1 | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4)"
