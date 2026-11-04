# calendar-sync-testkit

Fake calendar providers with fault injection, and recorded fixtures, for testing calendar-sync.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

## Fake providers

`FakeGoogle`, `FakeGraph` and `FakeCalDav` each hold one calendar in memory and answer the calls a
sync client makes, in their protocol's shape:

| Fake | Change listing | Writes |
|---|---|---|
| `google::FakeGoogle` | `events.list` with `pageToken`, and `nextSyncToken` on the last page only | client-chosen ids; `If-Match` on `etag` |
| `graph::FakeGraph` | `delta` with `@odata.nextLink` and `@odata.deltaLink`; `@removed` | server-assigned ids; `If-Match` on `changeKey`; `PATCH` merges |
| `caldav::FakeCalDav` | `sync-collection` (RFC 6578), truncated with `507` | client-chosen hrefs; `If-Match`, `If-None-Match: *` |

A `FaultPlan` makes them fail. Each fault is drawn from a seed at a rate per mille, or scripted for
a call number, so the same plan and the same calls always fail the same way:

| `Fault` | Google | Graph | CalDAV |
|---|---|---|---|
| `TokenExpired` | `410 Gone` | `410 Gone`, `syncStateNotFound` | `403`, `DAV:valid-sync-token` |
| `PreconditionFailed` | `412`: another client wrote first | `412` | `412` |
| `Replay` | a change already delivered, again | the same | the same |
| `PartialPage` | a short page | a short page | a truncated report |
| `LostResponse` | the call takes effect, the client sees a transport error | the same | the same |

```rust
use calendar_sync_testkit::google::{FakeGoogle, ListRequest};
use calendar_sync_testkit::{Fault, FaultPlan};

let mut google = FakeGoogle::new(FaultPlan::seeded(7).with_rate(Fault::LostResponse, 100));
google.remote_put("evt1", serde_json::json!({ "summary": "Standup" }));
let page = google.list(&ListRequest::default());
```

`faults()` lists what each fake injected, for assertions.

## Recorded fixtures

`fixture::Fixture` is the format for HTTP exchanges recorded from live probes: JSON, versioned,
replayed in order by `Fixture::replay`. `Fixture::audit` flags credentials (authentication headers,
cookies, credential parameters and fields, bearer tokens, Google access tokens, JSON Web Tokens)
and email addresses outside the reserved example domains, and `Fixture::from_json` refuses a
fixture it flags. `Fixture::redact` clears every finding, mapping each address to a stable
`personN@example.com`. The audit cannot recognise names or addresses in free text, so recorded
event content is replaced with synthetic text before it is committed.

`recorder::Recorder` is what a live probe records with. It keeps only the headers a replay needs
(content type, `ETag` and the conditional-request headers, `Location`, `Retry-After`, and the
`CalDAV` ones), so `Authorization` and cookies never enter a fixture. `Recorder::finish` replaces
the free text of events (titles, descriptions, locations, names and comments in JSON; `SUMMARY`,
`DESCRIPTION`, `LOCATION`, `COMMENT`, `CONTACT` and `CN` in iCalendar) with stable synthetic
text, runs `Fixture::redact`, and refuses to return a fixture the audit still flags.

The fixtures in `fixtures/` are synthetic examples of the format. Every file there is audited and
replayed by `tests/fixtures.rs`.

Licensed under MIT or Apache-2.0, at your option.
