# calendar-sync-graph

Microsoft Graph calendar mapping and a thin REST client for calendar-sync. HTTP, credentials and the clock are injected by the application.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

- `event::read` and `event::write` map one `event` resource to and from the event model. Every
  property the mapping does not read is kept as raw JSON; writes are `PATCH`es naming only the
  properties that changed, so Graph keeps the rest.
- `GraphSeam` implements the sync engine's provider seam: `calendarView/delta` per calendar with
  `Prefer: IdType="ImmutableId"`, creates under a `transactionId` and an extended property
  holding the idempotency key, updates and deletes that compare the `changeKey` first, and delta
  removals and sparse rows confirmed with a read. A recurrence Graph cannot hold is reported and
  never sent.
- `Precondition` chooses how a write checks its revision. The default reads and compares the
  `changeKey`, because whether Graph honours `If-Match` on events is unverified; `IfMatch` trusts
  the header alone.
- `Client` drives the seam over a `Transport` the application implements with its own HTTP
  client and credentials.
- `group` collects a series master and its exceptions, keyed by `originalStart`.
- `iana_zone` and `windows_zone` translate Graph's Windows time zone names. The table is
  generated from the vendored CLDR `windowsZones.xml` by `cargo xtask gen-windows-zones`, and CI
  fails if the checked-in table differs.

Property names and types come from the vendored Graph documentation in `references/graph/`.

## Tests

`tests/fixtures.rs` replays the recorded exchanges in `tests/fixtures/` through the seam: an
initial two-page delta round, an incremental round with a removal confirmed and one refuted by a
read, an expired delta link, a create found by its idempotency key, conditional updates and
deletes, and a series with an exception. The fixtures are synthesized from the documented
formats until the live suite records real ones.

`tests/live.rs` probes a real mailbox. It is `#[ignore]`d and does nothing unless
`CALENDAR_SYNC_GRAPH_LIVE=1`; the weekly `scheduled.yml` workflow runs it:

| Variable | Meaning |
|---|---|
| `CALENDAR_SYNC_GRAPH_LIVE` | `1` runs the suite |
| `GRAPH_CALENDAR_ID` | A calendar the probe may fill and empty. Unset uses the default calendar, which should then belong to a test account |
| `GRAPH_TENANT`, `GRAPH_CLIENT_ID`, `GRAPH_CLIENT_SECRET`, `GRAPH_REFRESH_TOKEN` | Exchanged for an access token. `GRAPH_TENANT` defaults to `common`; `GRAPH_CLIENT_SECRET` is omitted for a public client |
| `GRAPH_ACCESS_TOKEN` | An access token, instead of the four above |
| `CALENDAR_SYNC_GRAPH_RECORD` | A directory to write each probe's exchanges to, as scrubbed fixtures |

The suite sends requests with the `curl` binary, so it adds no HTTP client to the build. It also
reports, without failing, what Graph does with a stale `If-Match`, which decides whether
`Precondition::IfMatch` can become the default.

```sh
CALENDAR_SYNC_GRAPH_LIVE=1 cargo test -p calendar-sync-graph --test live -- --ignored --nocapture
```

The Windows time zone table is derived from the Unicode CLDR, under the Unicode License v3
reproduced in `src/windows_zones.rs`.

Licensed under MIT or Apache-2.0, at your option.
