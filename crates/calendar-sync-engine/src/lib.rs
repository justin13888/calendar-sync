//! Deterministic two-way calendar sync engine: `step(state, input) -> (state, effects)`,
//! three-way merge, write queue, split planner, instance index, and free/busy.
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`.
