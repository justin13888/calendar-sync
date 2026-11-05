# calendar-sync

Two-way calendar sync for Rust. This facade re-exports the published calendar-sync crates:

| Module | Crate |
|---|---|
| `model` | [`calendar-sync-model`](https://crates.io/crates/calendar-sync-model): provider-neutral event model |
| `recurrence` | [`calendar-sync-recurrence`](https://crates.io/crates/calendar-sync-recurrence): recurrence window expansion |

The sync engine, and the iCalendar, Google Calendar, Microsoft Graph and CalDAV integrations behind features named `ical`, `google`, `graph` and `caldav`, join it as their crates publish.

```toml
[dependencies]
calendar-sync = "0.1"
```

Part of [calendar-sync](https://github.com/justin13888/calendar-sync). API docs are on [docs.rs](https://docs.rs/calendar-sync).

## Versioning

0.x: a minor release (0.1 to 0.2) may break the API, and a patch release never does. The minimum supported Rust version is 1.91, raised only in a minor release. The full policy is in [docs/semver.md](https://github.com/justin13888/calendar-sync/blob/master/docs/semver.md).

Licensed under MIT or Apache-2.0, at your option.
