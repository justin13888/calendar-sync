//! The provider seam: the protocol-neutral requests the engine makes, the outcomes it accepts,
//! and the trait a provider crate implements to translate both ways.
//!
//! The engine never speaks a protocol. It asks for a [`Request`]: list the changes since a
//! [`Cursor`], read one resource, or create, update or delete one under a precondition. The
//! application hands the request to a [`ProviderSeam`], which turns it into the provider's own
//! [`ProviderSeam::Call`]; the application sends that call with whatever client it has, and hands
//! the response back to the seam, which turns it into an [`Outcome`] for the engine.
//!
//! The seam is a translation, not a client: it performs no I/O, so it stays usable from an async
//! application, a blocking one, or a test that drives a fake.
//!
//! # How each protocol fits
//!
//! | Request | Google | Microsoft Graph | `CalDAV` | JMAP |
//! |---|---|---|---|---|
//! | [`ListFrom::Start`] | `events.list` | `calendarView/delta` | `sync-collection`, no token | `CalendarEvent/get` |
//! | [`ListFrom::Since`] | `syncToken` | `@odata.deltaLink` | `DAV:sync-token` | `CalendarEvent/changes` `sinceState` |
//! | [`ListFrom::Continue`] | `pageToken` | `@odata.nextLink` | a truncated report's token | `sinceState` of the last page |
//! | [`Failure::CursorExpired`] | `410 Gone` | `410` `syncStateNotFound` | `DAV:valid-sync-token` | `cannotCalculateChanges` |
//! | [`Change::Upsert`] without an event | | | a `200` with only an `ETag` | `updated` ids |
//! | [`Request::Update`] precondition | `If-Match` on `etag` | `If-Match` on `changeKey` | `If-Match` on `ETag` | `ifInState`, or a read-compare |
//!
//! A [`Cursor`] is opaque to the engine and owned by the seam, so a seam may encode in it
//! whatever its protocol needs to continue.
//!
//! # What a seam must guarantee
//!
//! - A listing reports, over a complete pass, every resource that changed since the cursor it
//!   started from, and on a pass from [`ListFrom::Start`], every resource that exists. A change
//!   may be reported more than once: the engine ignores a revision it already holds.
//! - [`Failure::Transient`] means the request may or may not have taken effect. The engine reads
//!   before it repeats a write, so a transient failure never applies an edit twice, provided a
//!   create is idempotent for its [`ResourceId`]. A provider that assigns its own ids (Graph)
//!   makes it so with a client-chosen key such as Graph's `transactionId`.
//! - [`Outcome::Written`] carries the resource as the provider now holds it, with its new
//!   revision. Its id may differ from the requested one where the provider assigns ids.

use calendar_sync_model::{ProviderEvent, ProviderRevision};
use serde::{Deserialize, Serialize};

/// Names one request the engine made, so its outcome can be matched to it.
///
/// Ids are unique within one [`SyncState`](crate::SyncState), and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(u64);

impl RequestId {
    /// The id with this number.
    #[must_use]
    pub const fn new(number: u64) -> Self {
        Self(number)
    }

    /// The id's number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A provider's id for one resource: [`ProviderIds::id`](calendar_sync_model::ProviderIds::id).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(String);

impl ResourceId {
    /// Wraps a provider's resource id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the resource `event` describes.
    #[must_use]
    pub fn of(event: &ProviderEvent) -> Self {
        Self(event.ids.id.clone())
    }
}

/// Where a listing continues from: a sync token, a delta link, a page token, a JMAP state.
///
/// Opaque to the engine, which stores it and hands it back. Only the seam that issued it reads
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    /// Wraps a cursor exactly as the seam produced it.
    #[must_use]
    pub fn new(cursor: impl Into<String>) -> Self {
        Self(cursor.into())
    }

    /// The cursor.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A protocol-neutral request the engine asks the application to perform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Request {
    /// List one page of changes.
    List {
        /// Where the page starts.
        from: ListFrom,
    },
    /// Read one resource as the provider holds it now.
    Read {
        /// The resource.
        resource: ResourceId,
    },
    /// Create a resource that must not exist yet.
    Create {
        /// The id the resource is created under. A provider that assigns ids uses it as its
        /// idempotency key instead.
        resource: ResourceId,
        /// The event to create.
        event: ProviderEvent,
    },
    /// Replace a resource, only if it is still at `if_match`.
    Update {
        /// The resource.
        resource: ResourceId,
        /// The revision the replacement was planned from; `None` writes unconditionally, which
        /// the engine does only for a resource the provider gave no revision.
        if_match: Option<ProviderRevision>,
        /// The whole new record.
        event: ProviderEvent,
    },
    /// Delete a resource, only if it is still at `if_match`.
    Delete {
        /// The resource.
        resource: ResourceId,
        /// The revision the deletion was planned from; `None` deletes unconditionally.
        if_match: Option<ProviderRevision>,
    },
}

/// Where a [`Request::List`] page starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListFrom {
    /// A full listing of every resource that exists.
    Start,
    /// The changes since a cursor from the end of an earlier complete pass.
    Since(Cursor),
    /// The next page of the pass in progress.
    Continue(Cursor),
}

/// What became of a [`Request`], as the seam translated the provider's response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// A page of a listing.
    Listed(Page),
    /// The resource as it is now, or `None` where it does not exist or was deleted.
    Read(Option<ProviderEvent>),
    /// A create or update took effect. The record as the provider now holds it.
    Written(ProviderEvent),
    /// A delete took effect.
    Deleted,
    /// The request failed.
    Failed(Failure),
}

/// One page of a listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// The changes on the page, in the provider's order.
    pub changes: Vec<Change>,
    /// Whether the pass continues.
    pub next: Next,
}

/// One change in a [`Page`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::large_enum_variant,
    reason = "a change is moved once into the state; boxing would allocate per change for nothing"
)]
pub enum Change {
    /// The resource exists at `revision`.
    Upsert {
        /// The resource.
        resource: ResourceId,
        /// Its current revision.
        revision: ProviderRevision,
        /// The resource, where the listing carries it. Without it, the engine reads the
        /// resource when its revision differs from the one it holds.
        event: Option<ProviderEvent>,
    },
    /// The resource was deleted.
    Remove {
        /// The resource.
        resource: ResourceId,
    },
}

/// What follows a [`Page`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Next {
    /// More of this pass follows from this cursor, through [`ListFrom::Continue`].
    More(Cursor),
    /// The pass is complete. The next pass starts from this cursor, through [`ListFrom::Since`].
    Done(Cursor),
}

/// Why a request failed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Failure {
    /// A listing's cursor is no longer valid. The engine starts a full listing again.
    CursorExpired,
    /// A conditional write lost a race: the resource is no longer at the revision given.
    PreconditionFailed,
    /// An update or delete named a resource that does not exist.
    NotFound,
    /// A create named a resource that already exists.
    AlreadyExists,
    /// The request may or may not have taken effect: a lost response, a timeout, a server
    /// error. The engine tries again.
    Transient,
    /// The provider refused the request for good, for the reason given.
    Rejected(String),
}

/// Translates the engine's [`Request`]s into one provider's calls, and its responses into
/// [`Outcome`]s.
///
/// A provider crate implements it once. The application sends each [`ProviderSeam::Call`] with
/// its own client, and passes what it got back, a response or a transport error, as a
/// [`ProviderSeam::Response`]. The engine depends on nothing else about the provider, so a new
/// protocol needs a new seam and no change to the engine.
pub trait ProviderSeam {
    /// The provider's own request, ready to send.
    type Call;
    /// What sending a call produced, a response or a transport failure.
    type Response;

    /// The call that performs `request`.
    fn call(&self, request: &Request) -> Self::Call;

    /// What `response`, the result of sending the call for `request`, means to the engine.
    fn outcome(&self, request: &Request, response: Self::Response) -> Outcome;
}
