# calendar-sync

Two-way calendar sync for Rust. This facade re-exports the event model, recurrence and sync engine, and the provider integrations behind features:

| Feature | Re-exports |
|---|---|
| (always) | `model`, `recurrence`, `engine` |
| `ical` | `ical` |
| `google` | `google` |
| `graph` | `graph` |
| `caldav` | `caldav` (native only) |

**Status: not started.** See [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
