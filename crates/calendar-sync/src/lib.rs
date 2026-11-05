//! Two-way calendar sync for Rust.
//!
//! This facade re-exports the published calendar-sync crates:
//!
//! | Module | Crate |
//! |---|---|
//! | [`model`] | `calendar-sync-model`: the provider-neutral event model |
//! | [`recurrence`] | `calendar-sync-recurrence`: recurrence window expansion |
//!
//! The sync engine, and the iCalendar, Google, Microsoft Graph and CalDAV integrations behind
//! features of those names, join it as their crates publish.

#[doc(inline)]
pub use calendar_sync_model as model;
#[doc(inline)]
pub use calendar_sync_recurrence as recurrence;
