//! Microsoft Graph calendar mapping and a thin REST client for calendar-sync.
//!
//! HTTP, credentials and the clock are injected by the application. Nothing here performs I/O:
//! the crate turns the engine's requests into HTTP requests and HTTP responses into outcomes.
//!
//! # Shape
//!
//! - [`event`]: the pure mapping of one `event` resource to and from a
//!   [`ProviderEvent`](calendar_sync_model::ProviderEvent). Every property it does not map is kept,
//!   as raw JSON, in the record's [`Passthrough`](calendar_sync_model::Passthrough). Writes are
//!   `PATCH`es naming only the properties that changed, so Graph keeps the rest itself.
//! - [`GraphSeam`]: the engine's [`ProviderSeam`](calendar_sync_engine::ProviderSeam).
//!   `calendarView/delta` per calendar with immutable ids; creates under a `transactionId` and an
//!   extended property holding the idempotency key, which [`Request::Lookup`] filters on; updates
//!   and deletes that compare the `changeKey` first ([`Precondition`]); delta removals and sparse
//!   rows confirmed with a read.
//! - [`Exchange`]: one engine request as the one or more HTTP requests it takes.
//! - [`Transport`] and [`Client`]: the seam over an HTTP client the application supplies.
//! - [`group`] and [`instance_key`]: Graph holds each occurrence and exception of a series as its
//!   own event; they are grouped into an [`EventSeries`](calendar_sync_model::EventSeries) keyed
//!   by `originalStart`.
//! - [`iana_zone`] and [`windows_zone`]: Graph's Windows time zone names, from a table generated
//!   from the vendored CLDR `windowsZones.xml` by `cargo xtask gen-windows-zones`.
//!
//! [`Request::Lookup`]: calendar_sync_engine::Request::Lookup
//!
//! ```
//! use calendar_sync_engine::{ListFrom, Outcome, Request};
//! use calendar_sync_graph::{
//!     Client, GraphSeam, HttpRequest, HttpResponse, Transport, TransportError, Window,
//! };
//!
//! /// A transport that answers every delta request with one empty, final page.
//! struct Empty;
//! impl Transport for Empty {
//!     fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
//!         assert!(request.path.starts_with("/v1.0/me/calendars/work/calendarView/delta?"));
//!         assert!(request.header("Prefer").unwrap().contains("IdType=\"ImmutableId\""));
//!         Ok(HttpResponse::new(
//!             200,
//!             r#"{"value": [], "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=t1"}"#,
//!         ))
//!     }
//! }
//!
//! let window = Window {
//!     start: "2026-11-01T00:00:00Z".parse().unwrap(),
//!     end: "2026-12-01T00:00:00Z".parse().unwrap(),
//! };
//! let mut client = Client::new(GraphSeam::new(window).with_calendar("work"), Empty);
//! let outcome = client.perform(&Request::List { from: ListFrom::Start });
//! assert!(matches!(outcome, Outcome::Listed(_)));
//! ```
//!
//! # Mapping
//!
//! | Graph | Model |
//! |---|---|
//! | `id`, `iCalUId`, `seriesMasterId`, `changeKey` | `ids.id`, `ids.ical_uid`, `ids.series_id`, `revision` |
//! | `subject`, `body.content`, `location.displayName` | `title`, `description`, `location`. An empty string is no value |
//! | `start`, `end`, `isAllDay`, `originalStartTimeZone`, `originalEndTimeZone` | `times`, in the event's original zone. A Windows zone name reads as its IANA zone; UTC reads as an instant |
//! | `recurrence` | `recurrence`: six pattern types, three range types |
//! | `showAs`, `isCancelled` | `transparency` (`free` is transparent) and `status` (`tentative` is tentative, `isCancelled` cancelled) |
//! | `sensitivity` | `visibility`. `personal` reads as private |
//! | `organizer`, `attendees` `emailAddress`, `type`, `status.response` | `organizer`, `attendees` |
//! | `isReminderOn`, `reminderMinutesBeforeStart` | `reminders`: one display reminder before the start |
//! | `originalStart`, `type` and every other property | `passthrough`, as JSON text; other location and attendee properties as `location.NAME` and `attendees[ADDRESS].NAME` |
//!
//! What Graph cannot hold (a floating time, a chair, a second reminder) is written as the nearest
//! value and reported as a [`Loss`](calendar_sync_model::Loss) by [`event::write()`]. A recurrence
//! Graph cannot hold is reported too, and the seam refuses to send it: Graph would hold a
//! different series.
//!
//! # Ground truth
//!
//! The vendored Graph documentation in `references/graph/`: `event.md` for every property,
//! `event-delta.md`, `delta-query-events.md` and `delta-query-overview.md` for delta rounds,
//! `patternedrecurrence.md`, `recurrencepattern.md` and `recurrencerange.md` for recurrence, and
//! `outlook-schedule-recurring-events.md` for series. The zone table comes from
//! `references/cldr/release-49/windowsZones.xml`.

pub mod event;
mod http;
mod recurrence;
mod seam;
mod series;
mod time;
#[rustfmt::skip]
mod windows_zones;
mod zones;

pub use event::{MAPPER_VERSION, MapError};
pub use http::{BASE_URL, HttpRequest, HttpResponse, Method, Transport, TransportError, encode};
pub use seam::{Client, Exchange, GraphSeam, IDEMPOTENCY_PROPERTY, Precondition, Window};
pub use series::{group, instance_key};
pub use zones::{iana_zone, windows_zone};
