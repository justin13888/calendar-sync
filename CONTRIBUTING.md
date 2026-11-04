# Contributing to calendar-sync

Setup, the repository rules, the gates and the commit convention are in [CLAUDE.md](CLAUDE.md). This file adds the release flow.

## Releasing

Releases are automated by [release-plz](https://release-plz.dev/), configured in `release-plz.toml` and run by `.github/workflows/release-plz.yml` on every push to `master`. No repository secret is involved.

1. **A change merges.** Pull requests are squash-merged, so each Conventional Commit title on `master` is one changelog entry.
2. **The release pull request opens or updates.** The `release-pr` job bumps the versions the commits call for, writes each crate's `CHANGELOG.md` with the rules in `cliff.toml`, and opens or updates one pull request titled `chore(release): release`, labelled `release`. It also runs cargo-semver-checks against each crate's last published version: an incompatible API change it finds forces a breaking version bump, and the pull request body reports it.
3. **CI runs on it through a dispatch.** The pull request is opened with the default `GITHUB_TOKEN`, and GitHub starts no `pull_request` workflow for it. The job therefore runs `gh workflow run ci.yml --ref <release branch>`, and the dispatched run reports `ci-success` on the release branch's head commit. To re-run CI there by hand, run the same command.
4. **A maintainer reviews and merges it.**
5. **The crates publish.** On the merge commit, the `release` job gets a short-lived crates.io token through [trusted publishing](https://crates.io/docs/trusted-publishing), publishes every crate whose new version is not on crates.io yet, then creates a `<crate>-v<version>` tag and a GitHub release for each. On any other push it publishes nothing (`release_always = false`).

### What the changelog shows

| Commit type | Section |
|---|---|
| `feat` | Added |
| `fix` | Fixed |
| `perf` | Performance |
| `refactor` | Changed |
| `revert` | Reverted |
| `docs` | Documentation |
| `chore(deps)` | Dependencies |

`build`, `ci`, `test`, `style`, other `chore` commits and the release commits are left out. A breaking change (`!` or a `BREAKING CHANGE:` footer) is always listed, marked **breaking**.

### Versions

The crates share one version, `[workspace.package] version` in `Cargo.toml`, so a release bumps them together. Before 1.0, a breaking change bumps the minor version and anything else bumps the patch version.

### Publishing a crate for the first time

Every crate starts with `publish = false`, and release-plz skips those crates entirely. The `release` job publishes nothing, and does not ask crates.io for a token, until at least one crate is publishable. Trusted publishing can only be configured for a crate that already exists on crates.io, so a crate's first release is manual:

1. The crate's release issue removes `publish = false`.
2. A maintainer runs `cargo publish -p <crate>` locally.
3. The maintainer adds a trusted publisher for the crate on crates.io: repository `justin13888/calendar-sync`, workflow `release-plz.yml`.

From then on, release pull requests publish the crate.

### Dependency updates

Dependabot (`.github/dependabot.yml`) opens weekly updates for Cargo dependencies and GitHub Actions. Their commit messages start with `chore(deps):`, so they pass the commit gate.
