//! Two-way calendar sync for Rust.
//!
//! This facade re-exports the provider-neutral event model, recurrence expansion and the
//! deterministic sync engine. Provider integrations are re-exported behind features:
//!
//! | Feature | Module |
//! |---|---|
//! | (always) | [`model`], [`recurrence`], [`engine`] |
//! | `ical` | `ical` |
//! | `google` | `google` |
//! | `graph` | `graph` |
//! | `caldav` | `caldav` (native only) |

#[doc(inline)]
pub use calendar_sync_engine as engine;
#[doc(inline)]
pub use calendar_sync_model as model;
#[doc(inline)]
pub use calendar_sync_recurrence as recurrence;

#[cfg(feature = "ical")]
#[doc(inline)]
pub use calendar_sync_ical as ical;

#[cfg(feature = "google")]
#[doc(inline)]
pub use calendar_sync_google as google;

#[cfg(feature = "graph")]
#[doc(inline)]
pub use calendar_sync_graph as graph;

#[cfg(feature = "caldav")]
#[doc(inline)]
pub use calendar_sync_caldav as caldav;
