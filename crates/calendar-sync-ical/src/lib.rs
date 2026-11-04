//! Pure iCalendar mapping to and from the calendar-sync event model, including ICS feed parsing
//! and explicit loss reports.
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`.
