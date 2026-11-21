# calendar-sync-engine

Deterministic two-way calendar sync engine: `step(state, input) -> (state, effects)`, three-way merge, write queue, split planner, instance index, and free/busy.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

- `sync`: the state machine. It lists changes page by page and applies a pass only once it completes, so a truncated or failed listing never deletes; it starts a full listing when a cursor expires, without losing the edits waiting to be pushed; and it pushes each edit by reading, merging and writing conditionally. The state serializes after any step.
- `seam`: the protocol-neutral requests and outcomes, and the `ProviderSeam` trait a provider crate implements to translate them.
- `merge`: the field-by-field three-way merge at push time.
- `index`: every series' occurrences over a rolling horizon, in a discardable store the application supplies through the `IndexStore` trait. It is invalidated when a series or the time zone data changes, and a rebuild always equals the incremental result.
- `freebusy`: occurrences merged into the disjoint intervals in which time is taken, busy over tentative.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
