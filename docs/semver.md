# Versioning policy

This policy covers every crate published from this repository: today `calendar-sync-model`, `calendar-sync-recurrence` and the `calendar-sync` facade. A crate with `publish = false` in its `Cargo.toml` makes no promise at all.

## 0.x releases

The crates follow [Cargo's semver rules](https://doc.rust-lang.org/cargo/reference/semver.html), which treat the first non-zero component as the major version. Before 1.0:

| Release | May contain |
|---|---|
| Minor (0.1.x to 0.2.0) | Breaking changes, and anything else |
| Patch (0.1.0 to 0.1.1) | Additions, fixes and performance work that do not break a caller |

A dependency written `calendar-sync-model = "0.1"` therefore never picks up a breaking change on its own.

A breaking change is one of these:

- A change to the public Rust API that Cargo's semver reference classes as major. `cargo-semver-checks` runs on every release pull request against the last published version, and a break it finds forces the minor bump (`release-plz.toml`, `semver_check`).
- A change to the serialized form of a `calendar-sync-model` type that stops a value written by an earlier release of the same minor series from deserializing, or makes it deserialize to a different value. Applications store the model, so its `serde` form is part of the API.
- A change to recurrence expansion that makes the same input yield different occurrences, unless the earlier output contradicted RFC 5545 or its errata. Fixing such a contradiction is a bug fix, released as a patch and listed under Fixed in the changelog.
- Raising the MSRV (below).

A type marked `#[non_exhaustive]` may gain a variant or field in a patch release; matching on it needs a wildcard arm.

The crates share one version (`[workspace.package] version` in `Cargo.toml`), so a release bumps them together, and a breaking change in one crate is a minor release of all of them.

## MSRV

The minimum supported Rust version is **1.91**, declared as `rust-version` in each crate and checked in CI by `mise run msrv` (`cargo hack check --rust-version`).

- It is raised only together with [Sunrise](https://github.com/justin13888/Sunrise), the first consumer, and never past the Rust that Sunrise pins.
- Raising it is a breaking change, released in a minor version before 1.0 and called out in the changelog.
- It is raised in a pull request of its own that says why (CONTRIBUTING.md, "MSRV").

The toolchain in `rust-toolchain.toml` is separate: it is the stable Rust that development and CI use, and it moves freely.

## Facade

`calendar-sync` re-exports the published crates as modules. crates.io must resolve every dependency of a package, optional ones included, so the facade depends only on crates already published. When another crate publishes, the facade gains it in a patch release: the sync engine as the `engine` module, and the iCalendar, Google, Microsoft Graph and CalDAV integrations as `ical`, `google`, `graph` and `caldav`, each behind a feature of the same name that is off by default.
