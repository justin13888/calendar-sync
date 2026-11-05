//! Google Calendar mapping and a thin REST client for calendar-sync.
//!
//! HTTP, credentials and the clock are injected by the application. Nothing here performs I/O:
//! the crate turns the engine's requests into HTTP requests and HTTP responses into outcomes.
//!
//! # Shape
//!
//! - [`event`]: the pure mapping of one `Event` resource to and from a
//!   [`ProviderEvent`](calendar_sync_model::ProviderEvent). Every field it does not map is kept,
//!   as raw JSON, in the record's [`Passthrough`](calendar_sync_model::Passthrough), and written
//!   back unchanged, so a read-modify-write never drops a field Google added.
//! - [`GoogleSeam`]: the engine's [`ProviderSeam`](calendar_sync_engine::ProviderSeam). Sync
//!   tokens with a full resync on `410 Gone`, page tokens within a pass, `If-Match` on every
//!   update and delete, and inserts under the client-chosen id the engine derives from the edit
//!   id ([`DerivedIds::google_id`](calendar_sync_engine::DerivedIds::google_id)).
//! - [`Transport`] and [`Client`]: the seam over an HTTP client the application supplies.
//! - [`group`] and [`instance_key`]: Google holds each changed or cancelled instance of a series
//!   as its own resource; they are grouped into an
//!   [`EventSeries`](calendar_sync_model::EventSeries) keyed by `originalStartTime`.
//! - [`Window`]: the local filter. Google refuses `timeMin` beside a `syncToken`, so the window is
//!   applied here.
//!
//! ```
//! use calendar_sync_engine::{ListFrom, Outcome, Request};
//! use calendar_sync_google::{Client, GoogleSeam, HttpRequest, HttpResponse, Transport, TransportError};
//!
//! /// A transport that answers every list with one empty, final page.
//! struct Empty;
//! impl Transport for Empty {
//!     fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
//!         assert_eq!(request.path, "/calendar/v3/calendars/primary/events?maxResults=50");
//!         Ok(HttpResponse::new(200, r#"{"items": [], "nextSyncToken": "t1"}"#))
//!     }
//! }
//!
//! let mut client = Client::new(GoogleSeam::new("primary").with_page_size(50), Empty);
//! let outcome = client.perform(&Request::List { from: ListFrom::Start });
//! assert!(matches!(outcome, Outcome::Listed(_)));
//! ```
//!
//! # Mapping
//!
//! | Google | Model |
//! |---|---|
//! | `id`, `iCalUID`, `recurringEventId`, `etag` | `ids.id`, `ids.ical_uid`, `ids.series_id`, `revision` |
//! | `summary`, `description`, `location` | `title`, `description`, `location` |
//! | `start`, `end` | `times`. `date` is all-day; `dateTime` with `timeZone` is zoned, in that zone; `dateTime` alone is an instant. A recurring event's instant is written with `timeZone` `UTC`, which Google requires |
//! | `recurrence` | `recurrence`, read and written as iCalendar by `calendar-sync-ical` |
//! | `status`, `transparency`, `visibility` | `status`, `transparency`, `visibility` |
//! | `organizer` `email`, `displayName` | `organizer` |
//! | `attendees` `email`, `displayName`, `optional`, `resource`, `responseStatus` | `attendees` |
//! | `reminders` `useDefault`, `overrides` | `reminders` |
//! | `originalStartTime` and every other field | `passthrough`, as JSON text; other organizer and attendee fields as `organizer.NAME` and `attendees[EMAIL].NAME` |
//!
//! A `cancelled` resource with no `recurringEventId` is a deletion. One with a
//! `recurringEventId` is a cancelled instance, which Google keeps as a resource; it reads as an
//! event whose status is cancelled.
//!
//! What Google cannot hold (a floating time, a chair, a reminder after the start) is written as
//! the nearest value and reported as a [`Loss`](calendar_sync_model::Loss) by [`event::write()`].
//!
//! # Ground truth
//!
//! `references/google/calendar-v3-discovery.json`, the Calendar API's discovery document, for
//! every field name and type, and the incremental sync guide it links.

pub mod event;
mod http;
mod recurrence;
mod seam;
mod series;
mod time;

pub use event::{MAPPER_VERSION, MapError};
pub use http::{BASE_URL, HttpRequest, HttpResponse, Method, Transport, TransportError, encode};
pub use seam::{Client, GoogleSeam};
pub use series::{Window, group, instance_key};
