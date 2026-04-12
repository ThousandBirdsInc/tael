#!/usr/bin/env bash
set -euo pipefail

# Publish tael crates to crates.io

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

# Dry run first
echo "==> Running dry-run for tael-server..."
cargo publish -p tael-server --dry-run

echo ""
echo "==> Running dry-run for tael-cli..."
cargo publish -p tael-cli --dry-run

echo ""
read -p "Dry runs passed. Publish to crates.io? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

echo ""
echo "==> Publishing tael-server..."
cargo publish -p tael-server

echo "==> Waiting for crates.io to index tael-server..."
sleep 15

echo "==> Publishing tael-cli..."
cargo publish -p tael-cli

echo ""
echo "Done. Published tael-server and tael-cli v$(cargo metadata --no-deps --format-version 1 | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4)"
