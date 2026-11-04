# calendar-sync-ical

Pure iCalendar mapping to and from the calendar-sync event model, including ICS feed parsing and explicit loss reports.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`. Time zones come from the IANA database bundled into the build.

- `IcalDocument::parse` keeps every byte of a `CalDAV` object or a feed, and `to_bytes` returns them.
- `IcalDocument::read` reads each `UID`'s `VEVENT`s into an `EventSeries`: the master, and its overrides keyed by `RECURRENCE-ID`.
- `IcalDocument::write` writes a series back, rewriting only the properties whose fields changed. Unknown properties and parameters stay as they were.
- `read_feed` reads a subscription.
- Anything the model cannot carry comes back as a `Loss`.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
