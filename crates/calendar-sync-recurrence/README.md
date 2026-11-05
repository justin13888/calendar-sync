# calendar-sync-recurrence

Recurrence window expansion over the calendar-sync event model.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

Expands a series over a window: `RRULE`, `RDATE`, `EXDATE`, `EXRULE`, and overrides by `RECURRENCE-ID`, including `RANGE=THISANDFUTURE`. Daylight-saving gaps and folds follow RFC 5545 section 3.3.5, as its errata 4271 directs for rule instances. A calendar scale other than Gregorian, a rule RFC 5545 forbids, or an unknown time zone yields no occurrences and a reported `Loss`, never a guess.

Checked against python-dateutil's rrule cases and a recorded calcard 0.3.14 expansion (`tests/data/recurrence/`), and fuzzed (`fuzz/`).

**Status: unreleased.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
