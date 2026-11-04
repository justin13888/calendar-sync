# calcard differential

`expected.json` records how calcard 0.3.14 expands each case. The crate test `crates/calendar-sync-recurrence/tests/calcard.rs` expands the same cases and compares.

calcard may not be a dependency of any crate in this repository ([ADR 0001](../../../../docs/adr/0001-icalendar-crate.md)): its later versions are AGPL-3.0, and 0.3.14 is archived. So it runs once, out of tree, and its output is kept as data.

## The cases

- Every case in `../dateutil/cases.json` that `../dateutil/divergences.json` does not name, as a floating event lasting one hour.
- The hand-written `cases.json`: extra and excluded dates, moved instances, `RANGE=THISANDFUTURE`, all-day series, and zoned series in `Asia/Tokyo`, which has no daylight-saving changes.

The cases avoid daylight-saving gaps and folds and custom `VTIMEZONE`s, where ADR 0001 found calcard wrong. Those are covered against RFC 5545 and its errata in `tests/expand.rs` instead.

## What is compared

Each occurrence's start and end in UTC, with UTC as calcard's default zone and as the expander's floating zone. Two things are adjusted, both for reasons RFC 5545 gives:

- Every event has an end. calcard ends a `DATE-TIME` start with no end at 23:59:59, where section 3.6.1 says it ends at its start.
- Where a dateutil rule does not produce its own start, section 3.8.5.3 calls the set undefined. This library shows the start and counts it as the first instance, as section 3.3.10 says of `COUNT`; calcard leaves it out. The test drops the start and compares the rule's own instances.

`divergences.json` names each case where calcard disagrees with RFC 5545, the RFC's reason, and the occurrences the RFC calls for. Every other case must agree exactly.

## Recording again

`oracle.rs` is the whole recorder. Build it in a scratch crate outside this workspace, with `[workspace]` in its manifest and these dependencies:

```toml
calcard = "=0.3.14"
calendar-sync-model = { path = "<this repository>/crates/calendar-sync-model" }
chrono = "0.4"
jiff = { version = "0.2", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Then, from this repository's root:

```sh
oracle tests/data/recurrence/dateutil/cases.json tests/data/recurrence/dateutil/divergences.json \
  tests/data/recurrence/calcard/cases.json > tests/data/recurrence/calcard/expected.json
```

The recording here was made on 2026-11-04 with Rust 1.99.1.
