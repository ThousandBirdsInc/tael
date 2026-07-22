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

crates.io auth uses [Trusted Publishing](https://crates.io/docs/trusted-publishing)
(OIDC) — there is no long-lived API token stored in GitHub. The workflow
exchanges its GitHub Actions identity for a short-lived publish token via
`rust-lang/crates-io-auth-action`.

For **each** of `tael-server`, `tael-gui`, and `tael-cli` on crates.io
(you must be an owner): crate page → **Settings → Trusted Publishing →
Add** a GitHub config with:

- Repository owner: `ThousandBirdsInc`
- Repository name: `tael`
- Workflow filename: `release.yml`
- Environment: `release`

The `release` GitHub environment is created automatically the first time the
publish job runs; optionally add protection rules to it (required reviewers,
tag-only deployment) under **Settings → Environments** for an extra gate
before anything reaches crates.io.

Once trusted publishing is configured, consider revoking any old crates.io
API tokens that had publish scope for these crates.

If `main` has branch-protection rules, GitHub Actions must be allowed to push
the version-bump commit (e.g. add it to the rule's bypass list).

## Manual fallback

`./publish.sh` at the repo root is the interactive crates.io publish script
from the pre-automation days; keep it around for emergencies only.
