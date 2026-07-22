# Releasing

Releases are fully automated. To cut a release:

1. Go to **Actions → cut-release → Run workflow** (on `main`).
2. Pick a bump level (`patch` / `minor` / `major`), or type an explicit
   version to override it.
3. Click **Run workflow**. That's it.

## What happens

The `cut-release` workflow:

1. Bumps the workspace version in `Cargo.toml` (and the registry versions on
   `tael-cli`'s path deps), refreshes `Cargo.lock`, commits `Release vX.Y.Z`
   to `main`, and pushes the `vX.Y.Z` tag.
2. Dispatches the two tag workflows at that tag (explicitly, because tags
   pushed with the built-in `GITHUB_TOKEN` don't fire `on: push` workflows):
   - **release** — builds prebuilt `tael` binaries for the four
     Linux/macOS targets, attaches them to the GitHub Release (with
     auto-generated notes), then publishes `tael-server`, `tael-gui`, and
     `tael-cli` to crates.io in dependency order.
   - **docker** — builds and pushes the multi-arch image to GHCR
     (`:vX.Y.Z`, `:X.Y`, `:latest`).

Binaries are attached *before* the crates.io publish so
`cargo binstall tael-cli` never resolves a version whose prebuilt archives
aren't up yet.

The publish step is idempotent: crates already on crates.io for that version
are skipped, so a partially failed run can simply be re-run
(`Actions → release → Run workflow` at the `vX.Y.Z` tag).

Manually pushing a `vX.Y.Z` tag still works and triggers the same
release/docker workflows — `cut-release` is just the zero-touch front door.

## One-time setup

The release workflow needs a `CARGO_REGISTRY_TOKEN` repository secret: a
crates.io API token (https://crates.io/settings/tokens) with the
`publish-update` scope for `tael-server`, `tael-gui`, and `tael-cli`
(`publish-new` too if publishing a new crate). Set it under
**Settings → Secrets and variables → Actions**.

If `main` has branch-protection rules, GitHub Actions must be allowed to push
the version-bump commit (e.g. add it to the rule's bypass list).

## Manual fallback

`./publish.sh` at the repo root is the interactive crates.io publish script
from the pre-automation days; keep it around for emergencies only.
