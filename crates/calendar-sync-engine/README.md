# calendar-sync-engine

Deterministic two-way calendar sync engine: `step(state, input) -> (state, effects)`, three-way merge, write queue, split planner, instance index, and free/busy.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

- `sync`: the state machine. It lists changes page by page and applies a pass only once it completes, so a truncated or failed listing never deletes; it starts a full listing when a cursor expires, without losing the edits waiting to be pushed; and it pushes each edit by reading, merging and writing conditionally. The state serializes after any step.
- `seam`: the protocol-neutral requests and outcomes, and the `ProviderSeam` trait a provider crate implements to translate them.
- `merge`: the field-by-field three-way merge at push time.

**Status: in development.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
