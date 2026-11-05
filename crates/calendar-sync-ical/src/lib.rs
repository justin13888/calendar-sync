//! Pure iCalendar mapping to and from the calendar-sync event model, including ICS feed parsing
//! and explicit loss reports.
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`. Time zones come from the IANA database bundled into the
//! build; the current time, where a new `VEVENT` needs a `DTSTAMP`, is an input.
//!
//! # Shape
//!
//! - [`IcalDocument`] holds a `CalDAV` object or a feed, every byte of it.
//!   [`IcalDocument::read`] reads its `VEVENT`s into [`EventSeries`](calendar_sync_model::EventSeries),
//!   one per `UID`; [`IcalDocument::write`] writes a series back, rewriting only the properties
//!   whose fields changed; [`IcalDocument::to_bytes`] returns the bytes.
//! - [`read_feed`] reads a subscription in one call.
//! - Every read and write returns the [`Loss`](calendar_sync_model::Loss)es it could not carry.
//!
//! # Mapping
//!
//! | iCalendar | Model |
//! |---|---|
//! | `SUMMARY`, `LOCATION`, `DESCRIPTION` | `title`, `location`, `description` |
//! | `DTSTART`, and `DTEND` or `DURATION` | `times`. `VALUE=DATE` is all-day, a `Z` an instant, a `TZID` a zoned time, and anything else floating. A `DATE` start with no end lasts one day (RFC 5545 section 3.6.1) |
//! | `RRULE`, `EXRULE`, `RDATE`, `EXDATE` | `recurrence`. An `RDATE` period has no place and is a loss |
//! | `RECURRENCE-ID` and its `RANGE` | the override's `InstanceKey` and `InstanceRange` |
//! | An override whose `STATUS` is `CANCELLED` | `Override::Cancelled` |
//! | `STATUS`, `TRANSP`, `CLASS` | `status`, `transparency`, `visibility`. No `CLASS` is `Visibility::Default` |
//! | `ORGANIZER`, `ATTENDEE` with `CN`, `ROLE`, `PARTSTAT`, `CUTYPE` | `organizer`, `attendees`. `mailto:` is dropped from an address |
//! | `VALARM` with `ACTION` `DISPLAY` or `EMAIL` | `reminders.alerts` |
//! | Every other property and subcomponent | the event's `Passthrough`, as written |
//!
//! A `TZID` reads as the IANA zone it names, directly, as the last segments of a prefixed name
//! (`/mozilla.org/20050126_1/America/New_York`), or through its `VTIMEZONE`'s `X-LIC-LOCATION`.
//! One that names no IANA zone is kept as written, and reported.
//!
//! # Byte-faithful round trip
//!
//! Parsing keeps every byte: folds, line endings, blank lines, the order of properties, unknown
//! properties and parameters, and lines that do not parse at all. A document written with the
//! series it read returns its bytes unchanged. An edit rewrites only the properties of the fields
//! that changed; a rewritten line keeps the parameters the model does not map, and is folded at 75
//! octets (RFC 5545 section 3.1).
//!
//! # Ground truth
//!
//! RFC 5545 with its errata (`references/rfc/rfc5545.txt`,
//! `references/rfc/rfc5545-inline-errata.html`), RFC 7986, RFC 6868 parameter encoding, and
//! RFC 7529 `RSCALE` and `SKIP`. The iCalendar syntax tree is `ical-rs`
//! (`docs/adr/0001-icalendar-crate.md`); none of its types appear in this crate's API.

mod document;
mod line;
mod read;
mod value;
mod write;
mod zone;

pub use document::{IcalDocument, MAPPER_VERSION, Read, ReadSeries, Source, WriteError};

/// Reads a subscription feed: every `VEVENT` of every `VCALENDAR` in `bytes`, identified by its
/// `UID`.
///
/// A feed is read-only, so nothing is kept to write back. A line the parser cannot structure is
/// skipped and reported in [`Read::losses`], and the rest of the feed is still read.
#[must_use]
pub fn read_feed(bytes: &[u8]) -> Read {
    IcalDocument::parse(bytes).read(&Source::feed())
}
