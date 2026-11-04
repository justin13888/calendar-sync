# calendar-sync-model

Provider-neutral calendar event model: series, overrides, stable instance ids, and time variants (instant, zoned, floating, all-day).

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

It holds:

- `NormalizedEvent`: title, times, recurrence, organizer, attendees, location, description, status, transparency, visibility and reminders.
- `ProviderEvent`: one resource as a provider holds it, with its `ProviderIds`, an opaque `ProviderRevision` compared only for equality, the `MapperVersion` that read it, and a `Passthrough` of fields the model has no place for.
- `EventSeries`: a master and its overrides, keyed by `InstanceKey`, the provider's own identity for an instance.
- `TimeValue`: instant, zoned, floating (never coerced to a zone) and all-day.
- `Loss`: what a mapping could not carry, by field and reason. Nothing is dropped silently.
- `graph::recurrence_losses`: the parts of a recurrence that Microsoft Graph cannot hold.

Everything serializes with `serde`. The model stays close to JSCalendar (RFC 8984); the crate docs list each deliberate divergence.

**Status: unreleased.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
