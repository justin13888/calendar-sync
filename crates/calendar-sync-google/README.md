# calendar-sync-google

Google Calendar mapping and a thin REST client for calendar-sync. HTTP, credentials and the clock are injected by the application.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

- `event::read` and `event::write` map one `Event` resource to and from the event model. Every
  field the mapping does not read is kept as raw JSON and written back unchanged.
- `GoogleSeam` implements the sync engine's provider seam: sync tokens with a full resync on
  `410 Gone`, `If-Match` on every update and delete, and inserts under client-chosen ids.
- `Client` drives the seam over a `Transport` the application implements with its own HTTP
  client and credentials.
- `group` collects a series and its changed or cancelled instances, keyed by
  `originalStartTime`; `Window` filters events locally, since Google refuses `timeMin` beside a
  sync token.

Field names and types come from `references/google/calendar-v3-discovery.json`.

## Tests

`tests/fixtures.rs` replays the recorded exchanges in `tests/fixtures/` through the seam:
an initial two-page sync, an incremental change, a deletion, an expired sync token, and a
recurring series with a changed and a cancelled instance. The fixtures are synthesized from the
documented formats until the live suite records real ones.

`tests/live.rs` probes a real calendar. It is `#[ignore]`d and does nothing unless
`CALENDAR_SYNC_GOOGLE_LIVE=1`; the weekly `scheduled.yml` workflow runs it:

| Variable | Meaning |
|---|---|
| `CALENDAR_SYNC_GOOGLE_LIVE` | `1` runs the suite |
| `GOOGLE_CALENDAR_ID` | A calendar the probe may fill and empty. Never a personal calendar |
| `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET`, `GOOGLE_OAUTH_REFRESH_TOKEN` | Exchanged for an access token |
| `GOOGLE_CALENDAR_ACCESS_TOKEN` | An access token, instead of the three above |
| `CALENDAR_SYNC_GOOGLE_RECORD` | A directory to write each probe's exchanges to, as scrubbed fixtures |

The suite sends requests with the `curl` binary, so it adds no HTTP client to the build.

```sh
CALENDAR_SYNC_GOOGLE_LIVE=1 cargo test -p calendar-sync-google --test live -- --ignored
```

Licensed under MIT or Apache-2.0, at your option.
