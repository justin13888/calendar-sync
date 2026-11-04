# Contributing to calendar-sync

This guide covers setting up a checkout, the tasks and hooks, the commit convention, how pull requests merge, the rules every change follows, and how releases happen. [CLAUDE.md](CLAUDE.md) states the same rules in brief, for people and coding agents alike.

To report a security vulnerability, do not open an issue: follow [SECURITY.md](SECURITY.md).

## Setup

Rust comes from [rustup](https://rustup.rs/), pinned by `rust-toolchain.toml`: the toolchain version, the `rustfmt` and `clippy` components, and the `wasm32-unknown-unknown` target. Every other tool comes from [mise](https://mise.jdx.dev/), pinned to an exact version in `mise.toml`. mise does not manage Rust.

1. Install rustup and mise.
2. In the checkout, install the pinned tools:

   ```sh
   mise install
   ```

3. Install the pinned Rust toolchain and the git hooks:

   ```sh
   mise run setup
   ```

4. Check that everything passes:

   ```sh
   mise run ci
   ```

## Tasks

Every task is defined in `mise.toml` and runs with `mise run <task>`.

| Task | What it does |
|---|---|
| `setup` | Installs the toolchain from `rust-toolchain.toml` and the hk git hooks |
| `fmt` | Formats the Rust sources with `cargo fmt --all` |
| `fix` | Runs every hk linter in fix mode over the whole tree (`hk fix --all`) |
| `lint` | Runs every hk linter in check mode over the whole tree (`hk check --all`): rustfmt, clippy, taplo, typos, actionlint and zizmor |
| `toolchain` | Checks that rustc matches `rust-toolchain.toml` and that `rust-version` matches the MSRV |
| `clippy` | Lints every target and feature, denying warnings |
| `test` | Runs the tests with nextest, then the doctests |
| `doc` | Builds the docs, denying warnings |
| `wasm` | Builds and lints the pure crates for `wasm32-unknown-unknown` |
| `purity` | Checks the pure crates' dependency graphs and sources, and that the purity fixture still fails clippy |
| `deny` | Runs cargo-deny, and checks that the deny fixture's copyleft dependency is still rejected |
| `msrv` | Checks that the workspace builds on the MSRV |
| `semver` | Checks published crates for semver violations. CI does not run it; release pull requests run the same check (see [Releasing](#releasing)) |
| `commits:check` | Checks that every commit on the branch follows the commit convention |
| `ci` | Runs `toolchain`, `lint`, `clippy`, `test`, `doc`, `wasm`, `purity`, `deny`, `msrv`, `semver` and `commits:check`, in that order |

Run `mise run fix` before `mise run lint` when a linter complains: it fixes what the tools can fix.

## Git hooks

`mise run setup` installs the [hk](https://hk.jdx.dev/) hooks configured in `hk.pkl`:

| Hook | What it runs |
|---|---|
| `pre-commit` | The `lint` linters in fix mode over the staged files |
| `commit-msg` | `cog verify` on the commit message |
| `pre-push` | `mise run test`, `mise run deny` and `mise run purity` |

Never bypass a hook with `--no-verify`. If a hook fails because a tool is missing, run `mise install`.

## Commit convention

Commits follow [Conventional Commits](https://www.conventionalcommits.org/), strictly. `cog.toml` is the single source of the convention: the `commit-msg` hook and CI's `commits` job both read it.

```text
<type>[(<scope>)][!]: <description>
```

The types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`, `revert` and `style`.

A scope is optional. When given, it is one of:

| Scope | Covers |
|---|---|
| `facade` | `crates/calendar-sync` |
| `model`, `recurrence`, `engine`, `ical`, `google`, `graph`, `caldav`, `testkit` | `crates/calendar-sync-<scope>` |
| `xtask` | `xtask/` |
| `deps` | Dependency updates |
| `ci` | CI workflows |
| `release` | Release configuration and release pull requests |
| `references` | Vendored specifications |
| `repo` | Repository-wide files: these guides, templates, tool configuration |

Mark a breaking change with `!` after the type or scope, or with a `BREAKING CHANGE:` footer. For example:

```text
feat(recurrence): expand RDATE periods
fix(engine)!: report a conflicting write instead of dropping it
docs(repo): describe the release flow
```

## Pull requests

1. Branch from `master`, and keep each pull request to one change.
2. Fill in the pull request template.
3. Every pull request is squash-merged, and its title becomes the commit on `master`. So the title must follow the commit convention too, and it is the changelog entry release-plz writes (see [What the changelog shows](#what-the-changelog-shows)).
4. CI's `commits` job checks the title and every commit in the pull request. Merge commits from updating the branch are not checked.
5. `ci-success` is the single required check, and it passes only when every other CI job does. A new CI job must be added to its `needs`.

## Rules

### MSRV

The minimum supported Rust version is 1.91, `rust-version` in `Cargo.toml`. [Sunrise](https://github.com/justin13888/Sunrise), the first consumer, pins Rust 1.91.1. `mise run msrv` checks every crate builds on it, so a dependency that needs a newer Rust fails CI. Raise the MSRV only in a pull request of its own that says why, and change all three places it is written together: `rust-version` in `Cargo.toml`, `MSRV` in `mise.toml`, and `MSRV` in `xtask/src/toolchain.rs`. `mise run toolchain` fails while they disagree.

The toolchain in `rust-toolchain.toml` is separate: it is the current stable Rust that development and CI use.

### Purity

The model, recurrence, engine and ical crates are pure. They read no clock, filesystem, environment or ambient randomness, and build for `wasm32-unknown-unknown`. Time, time zones, configuration and randomness are inputs. Three layers enforce this, and [CLAUDE.md](CLAUDE.md) describes each:

1. `clippy.toml` disallows the clock, `std::env`, `std::fs` and ambient randomness everywhere except `xtask`.
2. `cargo xtask purity` rejects a pure crate whose dependency graph or sources reach for I/O.
3. The fixtures under `tests/fixtures/` must keep failing, and CI checks that they do. Do not fix them.

Do not silence the purity lints in library code. A test that reads fixture files may allow them at the narrowest scope.

### Dependencies and licences

- The crates are licensed MIT OR Apache-2.0.
- Dependencies use permissive licences only, as listed in `deny.toml`, come only from crates.io, and never use wildcard versions.
- Copyleft code never enters the repository, as a dependency or as copied code. Copyleft projects may be read as behaviour references or run out of process as test targets.
- `jiff` is the only date and time library.
- Declare a shared dependency in `[workspace.dependencies]`, and use it with `workspace = true`.

### Documentation

`missing_docs` is on: every public item has a doc comment.

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
