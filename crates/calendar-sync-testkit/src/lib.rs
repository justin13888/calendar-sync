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
//! The fakes model what a sync client sees, not every rule of each service. Bodies are opaque
//! (JSON for Google and Graph, iCalendar text for `CalDAV`), and Graph's delta window is not
//! modelled.

pub mod caldav;
mod fault;
pub mod google;
pub mod graph;
mod rng;
mod server;

pub use fault::{CallKind, Fault, FaultPlan, InjectedFault};
