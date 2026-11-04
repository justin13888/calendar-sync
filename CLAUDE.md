# calendar-sync: repository rules

Rules for anyone changing this repository, people and coding agents alike. `AGENTS.md` is a symlink to this file.

## Setup

- Rust comes from rustup, pinned by `rust-toolchain.toml`. mise does not manage Rust.
- Every other tool comes from mise, pinned in `mise.toml`.

```sh
mise install        # the pinned tools
mise run setup      # the pinned Rust toolchain, and the hk git hooks
mise run ci         # every gate CI runs, in order
```

## Layout

| Path | What it is |
|---|---|
| `crates/calendar-sync` | Facade. Re-exports model, recurrence and engine; `ical`, `google`, `graph` and `caldav` behind features of those names |
| `crates/calendar-sync-model` | Event model. **Pure** |
| `crates/calendar-sync-recurrence` | Recurrence expansion. **Pure** |
| `crates/calendar-sync-engine` | Sync engine. **Pure** |
| `crates/calendar-sync-ical` | iCalendar mapping. **Pure** |
| `crates/calendar-sync-google`, `-graph`, `-caldav` | Provider mapping and thin clients; I/O is injected |
| `crates/calendar-sync-testkit` | Fake providers and recorded fixtures |
| `xtask/` | Repository automation (`cargo xtask toolchain`, `purity`, `fixture purity`, `fixture deny`). Never published |
| `tests/fixtures/purity`, `tests/fixtures/deny` | Excluded crates that **must fail** clippy and cargo-deny. Do not fix them |

## Rules

- **Purity.** The pure crates read no clock, filesystem, environment or ambient randomness, and build for `wasm32-unknown-unknown`. Take time, time zones, configuration and randomness as inputs. Three layers enforce this:
  1. `clippy.toml` disallows the clock, `std::env`, `std::fs` and ambient RNG workspace-wide, provider crates included. `xtask/clippy.toml` relaxes it for xtask only.
  2. `cargo xtask purity` fails if a pure crate's normal dependency graph contains tokio, hyper, reqwest, libdav, getrandom or rand, or if its sources mention a banned API.
  3. `cargo xtask fixture purity` and `cargo xtask fixture deny` fail if the fixtures stop being rejected.
- **Do not silence the purity lints** with `#[allow(clippy::disallowed_methods)]` in library code. Tests that read fixture files may allow it locally, at the narrowest scope.
- **Dependencies.** `jiff` is the only date and time library. Only permissive licences (see `deny.toml`), only crates.io, and no wildcard versions. No copyleft dependency, and no code copied from copyleft projects. They may be read as behaviour references or run out of process as test targets.
- **Workspace.** Declare shared dependencies in `[workspace.dependencies]` and use `workspace = true` in crates. Every crate sets `[lints] workspace = true`. Keep `rust-version` at 1.91: Sunrise, the first consumer, pins 1.91.1.
- **Publishing.** Every crate is `publish = false` until its release issue flips it.
- **Docs.** `missing_docs` is on. Every public item has a doc comment.

## Commits and pull requests

- Conventional Commits, strictly. `cog.toml` is the single source: types `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`, `revert`, `style`. A scope is optional; when given it is one of `facade`, `model`, `recurrence`, `engine`, `ical`, `google`, `graph`, `caldav`, `testkit`, `xtask`, `deps`, `ci`, `release`, `references`, `repo`.
- The commit-msg hook runs `cog verify`. CI's `commits` job checks every commit in a pull request and its title, because pull requests are squash-merged with the title as the commit message.
- Never bypass hooks with `--no-verify`. If a hook fails because a tool is missing, run `mise install`.
- `ci-success` is the single required check. Add any new CI job to its `needs`.

## Gates

| Task | What it checks |
|---|---|
| `mise run toolchain` | rustc matches `rust-toolchain.toml`; `rust-version` matches the MSRV |
| `mise run lint` / `mise run fix` | `hk check --all` / `hk fix --all`: rustfmt, clippy, taplo, typos, actionlint, zizmor |
| `mise run clippy` | clippy on every target and feature, `-D warnings` |
| `mise run test` | nextest, then doctests |
| `mise run doc` | rustdoc, `-D warnings` |
| `mise run wasm` | pure crates build and lint for `wasm32-unknown-unknown` |
| `mise run purity` | `cargo xtask purity` and the purity fixture |
| `mise run deny` | cargo-deny and the deny fixture |
| `mise run msrv` | `cargo hack check --rust-version` on Rust 1.91 |
| `mise run semver` | cargo-semver-checks on published crates |
| `mise run commits:check` | every commit on the branch follows `cog.toml` |

The hooks: pre-commit runs the `lint` linters in fix mode, commit-msg runs `cog verify`, and pre-push runs `test`, `deny` and `purity`.
