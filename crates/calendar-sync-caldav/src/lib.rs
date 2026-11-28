//! `CalDAV` mapping over `libdav` for calendar-sync. Native only.
//!
//! HTTP, credentials and the clock are injected by the application. Nothing here performs I/O:
//! the crate turns the engine's requests into HTTP requests, built and parsed by `libdav`'s
//! request types, and HTTP responses into outcomes.
//!
//! # Shape
//!
//! - [`CalDavSeam`]: the engine's [`ProviderSeam`](calendar_sync_engine::ProviderSeam) for one
//!   calendar collection. Listing is the `DAV:sync-collection` report (RFC 6578), with a
//!   truncated report continued from its token and an invalid token reported as
//!   [`Failure::CursorExpired`](calendar_sync_engine::Failure::CursorExpired). A server that
//!   refuses the report is listed by comparing `ETag`s instead ([`Listing::Etags`]). The changed
//!   objects are read with `calendar-multiget` in batches. Creates send `If-None-Match: *`,
//!   updates and deletes `If-Match` on the `ETag`.
//! - [`Exchange`]: one engine request as the one or more HTTP requests it takes.
//! - [`Capabilities`] and [`Discovery`]: what the server offers for the collection, from the
//!   `DAV` header and the collection's properties. [`Capabilities::missing`] names each
//!   [`Feature`] the server lacks; [`Capabilities::listing`] chooses the listing.
//! - [`Transport`] and [`Client`]: the seam over an HTTP client the application supplies.
//! - [`object`]: one calendar object resource as one engine record. The object's text is kept in
//!   the record, so a write changes only the properties whose fields changed and keeps every
//!   other byte; [`object::series`] reads the whole series back out.
//!
//! ```
//! use calendar_sync_caldav::{CalDavSeam, Client, HttpRequest, HttpResponse, Transport, TransportError};
//! use calendar_sync_engine::{ListFrom, Next, Outcome, Request};
//!
//! /// A server whose collection is empty.
//! struct Empty;
//! impl Transport for Empty {
//!     fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
//!         assert_eq!(request.method, "REPORT");
//!         assert!(request.body.as_deref().unwrap().contains("sync-collection"));
//!         Ok(HttpResponse::new(
//!             207,
//!             r#"<multistatus xmlns="DAV:"><sync-token>http://example.com/s/1</sync-token></multistatus>"#,
//!         ))
//!     }
//! }
//!
//! let seam = CalDavSeam::new("https://dav.example.com", "/cal/", "2026-11-20T00:00:00Z".parse().unwrap())
//!     .unwrap();
//! let mut client = Client::new(seam, Empty);
//! let Outcome::Listed(page) = client.perform(&Request::List { from: ListFrom::Start }) else {
//!     panic!()
//! };
//! assert!(page.changes.is_empty());
//! assert!(matches!(page.next, Next::Done(_)));
//! ```
//!
//! # Requests
//!
//! | Engine request | `CalDAV` | Outcome |
//! |---|---|---|
//! | `List` from the start | `REPORT` `sync-collection` with an empty token; `PROPFIND` `Depth: 1` where the server refuses it | every object, read with `calendar-multiget` |
//! | `List` since a cursor | `REPORT` `sync-collection` with the token, or a `PROPFIND` compared with the `ETag`s the cursor holds | changes, and removals; `507` on the collection continues the pass |
//! | `Read` | `GET` | the object, or `None` on `404` |
//! | `Create` | `PUT` with `If-None-Match: *` | `412` is `AlreadyExists` |
//! | `Update` | `PUT` with `If-Match` | `412` is `PreconditionFailed` |
//! | `Delete` | `DELETE` with `If-Match` | `412` is `PreconditionFailed`, `404` `NotFound` |
//!
//! A write's response with no `ETag` (RFC 4791 section 5.3.4: the server changed what it stored)
//! is followed by a `GET`, so [`Outcome::Written`](calendar_sync_engine::Outcome::Written)
//! carries what the server holds. A collection may also hold tasks; an object without a `VEVENT`
//! is not listed.
//!
//! # Scheduling
//!
//! RFC 6638 is used where the server offers it: [`Capabilities::auto_schedule`] and the object's
//! `Schedule-Tag`, kept on a record read with `GET` ([`object::schedule_tag`]), are what the
//! engine's `RsvpTarget::CalDav` needs.
//!
//! # Ground truth
//!
//! `references/rfc/rfc4791.txt` (`CalDAV`), `rfc4918.txt` (`WebDAV`), `rfc6578.txt`
//! (`sync-collection`), `rfc6638.txt` (scheduling), `rfc6764.txt` (discovery) and `rfc9110.txt`
//! (conditional requests).

mod capabilities;
mod http;
pub mod object;
mod seam;

pub use calendar_sync_ical::MAPPER_VERSION;
pub use capabilities::{Capabilities, Discovery, DiscoveryError, Feature};
pub use http::{HttpRequest, HttpResponse, Transport, TransportError};
pub use seam::{CalDavSeam, Client, DEFAULT_BATCH, Exchange, Listing, UrlError};
