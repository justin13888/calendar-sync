//! Deterministic two-way calendar sync engine: `step(state, input) -> (state, effects)`,
//! three-way merge, write queue, split planner, instance index, and free/busy.
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`.
//!
//! - [`sync`]: the state machine. [`step`] takes a [`SyncState`] and an [`Input`], and returns the
//!   next state and the [`Effect`]s the application performs. The state serializes after any
//!   step.
//! - [`seam`]: the provider seam. The protocol-neutral [`Request`]s the engine makes, the
//!   [`Outcome`]s it accepts, and the [`ProviderSeam`] trait a provider crate implements to
//!   translate both ways.
//! - [`ids`]: the provider ids a create derives from its [`EditId`], so a retry from any
//!   device names the same resource.
//! - [`merge`]: the three-way merge of one event at push time, field by field, with
//!   organizer and attendee ownership.
//! - [`split`]: "this and following" as a series split: the new series created first, the old
//!   one trimmed after, every step recoverable from what the provider holds.
//!
//! ```
//! use calendar_sync_engine::{Effect, Input, ListFrom, Request, SyncState, step};
//!
//! let (state, effects) = step(SyncState::new(), Input::Sync);
//! // With no cursor yet, the first pass lists everything.
//! assert!(matches!(
//!     &effects[..],
//!     [Effect::Request { request: Request::List { from: ListFrom::Start }, .. }]
//! ));
//! assert!(state.is_syncing());
//! ```

pub mod ids;
pub mod merge;
pub mod seam;
pub mod split;
pub mod sync;

pub use ids::DerivedIds;
pub use merge::{Conflict, FieldValue, MergeOutcome, Refused, Role};
pub use seam::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, RequestId,
    ResourceId,
};
pub use split::{Split, SplitError, SplitStep, Splitter};
pub use sync::{Edit, EditId, Effect, Input, Notice, Operation, Rejection, SyncState, step};
