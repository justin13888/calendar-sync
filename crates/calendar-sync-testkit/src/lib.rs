//! Fake calendar providers with fault injection, and recorded fixtures, for testing calendar-sync.
//!
//! # Fake providers
//!
//! [`google::FakeGoogle`], [`graph::FakeGraph`] and [`caldav::FakeCalDav`] each hold one calendar
//! in memory and answer the calls a sync client makes, in their protocol's shape: Google
//! `events.list` with page and sync tokens, Graph `delta` with `@odata.nextLink` and
//! `@odata.deltaLink`, and `CalDAV` `sync-collection` (RFC 6578) with `ETag` preconditions. They
//! are synchronous and perform no I/O, so a test drives them call by call.
//!
//! Each fake injects the [`Fault`]s a [`FaultPlan`] asks for: an expired token (`410`), a
//! conditional write that loses a race (`412`), a replayed change, a short page, and a response
//! lost after the call took effect. Faults are drawn from a seed, or scripted for a call number,
//! so the same plan and the same calls always fail the same way. Each fake logs the faults it
//! injected, for assertions.
//!
//! ```
//! use calendar_sync_testkit::google::{Error, FakeGoogle, ListRequest};
//! use calendar_sync_testkit::{Fault, FaultPlan};
//!
//! // The second call loses its sync token.
//! let mut google = FakeGoogle::new(FaultPlan::seeded(7).at_call(1, Fault::TokenExpired));
//! google.remote_put("evt1", serde_json::json!({ "summary": "Standup" }));
//!
//! let first = google.list(&ListRequest::default()).unwrap();
//! assert_eq!(first.items.len(), 1);
//! let incremental = ListRequest {
//!     sync_token: first.next_sync_token,
//!     ..ListRequest::default()
//! };
//! assert_eq!(google.list(&incremental), Err(Error::Gone));
//! assert_eq!(google.faults()[0].fault, Fault::TokenExpired);
//! ```
//!
//! The fakes model what a sync client sees, not every rule of each service. Bodies are opaque
//! (JSON for Google and Graph, iCalendar text for `CalDAV`), and Graph's delta window is not
//! modelled.
//!
//! # Driving the sync engine
//!
//! The [`seam`] module implements the engine's
//! [`ProviderSeam`](calendar_sync_engine::ProviderSeam) for each fake, so the engine's requests
//! can be performed against them.
//!
//! # Recorded fixtures
//!
//! [`fixture::Fixture`] is the format for HTTP exchanges recorded from live probes. A fixture
//! that carries a credential or an email address outside the example domains cannot be loaded;
//! see the [`fixture`] module. A live probe records its exchanges with a
//! [`recorder::Recorder`], which keeps only the headers a replay needs, replaces the free text of
//! events with synthetic text, and redacts what remains before it returns the fixture.

pub mod caldav;
mod fault;
pub mod fixture;
pub mod google;
pub mod graph;
pub mod recorder;
mod rng;
pub mod seam;
mod server;

pub use fault::{CallKind, Fault, FaultPlan, InjectedFault};
