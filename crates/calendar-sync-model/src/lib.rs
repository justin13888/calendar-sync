//! Provider-neutral calendar event model: series, overrides, stable instance ids, and time
//! variants (instant, zoned, floating, all-day).
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`.
