//! Recurrence window expansion over the calendar-sync event model.
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`. The window, the time zone database and the zone floating
//! times are seen in are all inputs.
//!
//! # Shape
//!
//! - [`Expander`] expands an [`EventSeries`](calendar_sync_model::EventSeries) over a [`Window`]:
//!   `RRULE`, `RDATE`, `EXDATE`, `EXRULE`, and overrides by `RECURRENCE-ID`, including
//!   `RANGE=THISANDFUTURE`. Its [`Expansion`] holds each [`Occurrence`] and every
//!   [`Loss`](calendar_sync_model::Loss).
//! - [`ZoneSource`] supplies time zones: a `jiff` [`TimeZoneDatabase`](jiff::tz::TimeZoneDatabase)
//!   or a closure.
//! - [`CivilRule`] expands one rule in civil time, for a caller that places times itself.
//!
//! # Ground truth
//!
//! RFC 5545 with its errata (`references/rfc/rfc5545-inline-errata.html`) and RFC 7529
//! (`references/rfc/rfc7529.txt`). Two readings matter most:
//!
//! - Errata 4271 replaces section 3.3.10's rule that an instance in a daylight-saving gap is
//!   dropped: it is placed as section 3.3.5 places any `DATE-TIME`, with the offset before the
//!   gap, so it moves later by the gap's length. An instance in a fold is the first of the two.
//!   This is jiff's "compatible" disambiguation.
//! - An `RSCALE` other than Gregorian is not expanded. RFC 7529 section 6, option 2: the series,
//!   overrides included, yields no occurrences, and a [`Loss`](calendar_sync_model::Loss) says
//!   so. A rule this library cannot read does the same. Expansion never guesses.

mod expand;
mod rule;

pub use expand::{EmptyWindow, Expander, Expansion, Occurrence, Source, Window, ZoneSource};
pub use rule::{CivilRule, Instances, PERIOD_BUDGET};
