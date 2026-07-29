#!/usr/bin/env bash
set -euo pipefail

# Cut a tael release.
#
# Publishing itself happens in CI: pushing a `v*` tag triggers
# .github/workflows/release.yml, which builds the prebuilt `cargo binstall`
# binaries and then publishes tael-server, tael-gui, and tael-cli to crates.io
# using Trusted Publishing (OIDC — no API token stored anywhere). This script
# only does the local half: sanity-check the tree, tag, and push.
#
# Usage: ./publish.sh

if [[ ! -f Cargo.toml ]]; then
  echo "Error: must be run from the workspace root"
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: there are uncommitted changes. Commit before releasing."
  echo ""
  git status --short
  exit 1
fi

version=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
tag="v${version}"
branch=$(git rev-parse --abbrev-ref HEAD)

# The release workflow re-checks this, but failing here saves a round trip.
if git rev-parse "$tag" >/dev/null 2>&1; then
  echo "Error: tag $tag already exists. Bump the version in Cargo.toml first."
  exit 1
fi

# Every version reference has to move together: the workspace version drives the
# crates.io release, the tael-cli dep pins have to resolve to the just-published
# libraries, and the GUI's package.json/tauri.conf.json feed the app bundle.
echo "==> Checking version references are in sync with $version..."
stale=$(grep -rn --include='*.toml' --include='*.json' '"0\.[0-9]*\.[0-9]*"' \
  Cargo.toml tael-cli/Cargo.toml tael-gui/package.json \
  tael-gui/src-tauri/tauri.conf.json 2>/dev/null |
  grep -E 'version' | grep -v "\"${version}\"" || true)
if [[ -n "$stale" ]]; then
  echo "Error: these version references don't match $version:"
  echo "$stale"
  exit 1
fi

echo "==> Verifying the workspace publishes cleanly..."
cargo publish --workspace --dry-run --locked

echo ""
echo "  version : $version"
echo "  tag     : $tag"
echo "  branch  : $branch"
echo "  commit  : $(git rev-parse --short HEAD)"
echo ""
echo "Pushing $tag will publish to crates.io. This is irreversible (crates.io"
echo "releases can be yanked but never replaced or deleted)."
echo ""
read -p "Tag and push? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

git push origin "$branch"
git tag -a "$tag" -m "Release $tag"
git push origin "$tag"

echo ""
echo "Pushed $tag. Watch the release run:"
echo "  gh run watch \$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
