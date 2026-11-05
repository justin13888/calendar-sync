# calendar-sync-recurrence

Recurrence window expansion over the calendar-sync event model.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

Expands a series over a window: `RRULE`, `RDATE`, `EXDATE`, `EXRULE`, and overrides by `RECURRENCE-ID`, including `RANGE=THISANDFUTURE`. Daylight-saving gaps and folds follow RFC 5545 section 3.3.5, as its errata 4271 directs for rule instances. A calendar scale other than Gregorian, a rule RFC 5545 forbids, or an unknown time zone yields no occurrences and a reported `Loss`, never a guess.

Checked against python-dateutil's rrule cases and a recorded calcard 0.3.14 expansion (`tests/data/recurrence/`), and fuzzed (`fuzz/`).

The crate reads no time zone database itself: the caller supplies zones through a `ZoneSource`.

```toml
[dependencies]
calendar-sync-model = "0.1"
calendar-sync-recurrence = "0.1"
```

Part of [calendar-sync](https://github.com/justin13888/calendar-sync). API docs are on [docs.rs](https://docs.rs/calendar-sync-recurrence).

## Versioning

0.x: a minor release (0.1 to 0.2) may break the API, and a patch release never does. The minimum supported Rust version is 1.91, raised only in a minor release. The full policy is in [docs/semver.md](https://github.com/justin13888/calendar-sync/blob/master/docs/semver.md).

Licensed under MIT or Apache-2.0, at your option.
