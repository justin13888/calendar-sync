# calendar-sync-engine

Deterministic two-way calendar sync engine: `step(state, input) -> (state, effects)`, three-way merge, write queue, split planner, instance index, and free/busy.

Pure: no clock, filesystem, environment or ambient randomness. Builds for `wasm32-unknown-unknown`.

**Status: not started.** Part of [calendar-sync](https://github.com/justin13888/calendar-sync).

Licensed under MIT or Apache-2.0, at your option.
