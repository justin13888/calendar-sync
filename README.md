# calendar-sync

Two-way calendar sync for Rust: a provider-neutral event model, recurrence, and a deterministic sync engine, over Google Calendar, Microsoft Graph and CalDAV.

**Status: not started.** Nothing is published yet. The plan is in this repository's issues.

Existing crates cover parsing, rule expansion, time zones and the DAV protocol. This library is the layer above them: the model, write semantics across providers, and two-way sync. Its first consumer is [Sunrise](https://github.com/justin13888/Sunrise), but it depends on no Sunrise crate.

## Crates

| Crate | Purpose | I/O |
|---|---|---|
| `calendar-sync` | Facade: re-exports model, recurrence and engine, and `ical`, `google`, `graph`, `caldav` behind features of those names | As enabled |
| `calendar-sync-model` | Series, override, stable instance id, time variants (instant, zoned, floating, all-day) | None |
| `calendar-sync-recurrence` | Window expansion over the model | None |
| `calendar-sync-engine` | `step(state, input) -> (state, effects)`, three-way merge, write queue, split planner, instance index, free/busy | None |
| `calendar-sync-ical` | Pure iCalendar mapping to and from the model, ICS feed parsing, explicit loss reports | None |
| `calendar-sync-google` | Pure mapping and a thin REST client | Injected |
| `calendar-sync-graph` | Pure mapping and a thin REST client | Injected |
| `calendar-sync-caldav` | Pure mapping over `libdav`; native only | Injected |
| `calendar-sync-testkit` | Fake providers with fault injection, recorded fixtures | None |

## Rules

- The model, recurrence, engine and ical crates read no clock, no filesystem and no ambient randomness, and build for wasm. CI enforces this.
- The application supplies storage, HTTP, credentials and scheduling.
- `jiff` is the only date and time library.
- No copyleft dependency, and no code copied from copyleft projects. They may be read as behaviour references or run out of process as test targets.

## Development

Rust comes from rustup, pinned by `rust-toolchain.toml`; every other tool comes from mise.

```sh
mise install && mise run setup && mise run ci
```

The repository rules, gates and commit convention are in [CLAUDE.md](CLAUDE.md).

## Licence

MIT or Apache-2.0, at your option.
