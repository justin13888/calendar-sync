# calendar-sync-caldav

CalDAV mapping over `libdav` for calendar-sync. Native only. HTTP, credentials and the clock are injected by the application.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

- `CalDavSeam` implements the sync engine's provider seam for one calendar collection. It lists
  with the `sync-collection` report (RFC 6578), continuing a truncated report (`507`) from its
  token and expiring the cursor on `DAV:valid-sync-token`. A server that refuses the report is
  listed by comparing every member's `ETag` with the last pass instead (`Listing::Etags`).
  Changed objects are read with `calendar-multiget`, in batches. Creates send
  `If-None-Match: *`; updates and deletes send `If-Match` on the `ETag`.
- `libdav`'s request types build and parse every report, `PROPFIND`, `PUT` and `DELETE`; the
  crate never performs I/O itself.
- `Capabilities`, from `Client::discover` or the `Discovery` exchange, report what a server
  lacks: `calendar-access`, `sync-collection`, `VEVENT` support, `calendar-auto-schedule`
  (RFC 6638). `Capabilities::listing` picks the listing.
- `object` maps one calendar object resource to one engine record. The object's text is kept in
  the record, so a write rewrites only the properties whose fields changed and keeps overrides,
  unknown properties and other components byte for byte; `object::series` reads the whole series
  back out. The `Schedule-Tag` a `GET` returns is kept for the engine's RSVP plan.
- `Client` drives the seam over a `Transport` the application implements with its own HTTP
  client and credentials.

Ground truth: `references/rfc/rfc4791.txt`, `rfc4918.txt`, `rfc6578.txt`, `rfc6638.txt`,
`rfc6764.txt` and `rfc9110.txt`.

## Tests

`tests/probes/mod.rs` holds the probes: discovery, a create, update and delete lifecycle with each
precondition refused once, the `ETag` listing, a series whose override survives an update of its
master, and a sync token the server never issued. `tests/live.rs` runs them against a live server;
`tests/fixtures.rs` replays them from the scrubbed fixtures they recorded from RustiCal and
Stalwart, in `tests/fixtures/SERVER/PROBE.json`, the testkit's fixture format.

The live suite is `#[ignore]`d and does nothing unless `CALENDAR_SYNC_CALDAV_LIVE=1`:

| Variable | Meaning |
|---|---|
| `CALENDAR_SYNC_CALDAV_LIVE` | `1` runs the suite |
| `CALDAV_URL` | The server, such as `https://caldav.fastmail.com` |
| `CALDAV_COLLECTION` | The calendar collection's path. The probes fill and empty it, so use a calendar made for them |
| `CALDAV_USERNAME`, `CALDAV_PASSWORD` | HTTP Basic credentials: an app password for iCloud and Fastmail |
| `CALENDAR_SYNC_CALDAV_RECORD` | A directory to write each probe's exchanges to, as scrubbed fixtures |

It sends requests with the `curl` binary, so it adds no HTTP client to the build. CI's `caldav`
job runs it against RustiCal and Stalwart, each started by `tests/fixtures/caldav/serve.sh` from
an image pinned by digest. Both servers are AGPL-licensed and run only as separate processes.

```sh
sh tests/fixtures/caldav/serve.sh rustical \
  cargo test -p calendar-sync-caldav --test live -- --ignored --nocapture
```

To record an iCloud or Fastmail fixture set, run the suite with the variables above and
`CALENDAR_SYNC_CALDAV_RECORD=crates/calendar-sync-caldav/tests/fixtures/SERVER`, review the
files, and add `SERVER` to the list in `tests/fixtures.rs`.

Licensed under MIT or Apache-2.0, at your option.
