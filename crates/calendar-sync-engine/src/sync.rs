//! The sync state machine: `step(state, input) -> (state, effects)`.
//!
//! [`SyncState`] mirrors one provider calendar and holds the edits waiting to be pushed to it.
//! [`step`] takes one [`Input`] (a request to sync, a local edit, the [`Outcome`] of a request,
//! the time, or a restart) and returns the next state and the [`Effect`]s the application must
//! perform: [`Request`]s for the provider, through a [`ProviderSeam`](crate::ProviderSeam), and
//! [`Notice`]s for the application. The engine performs no I/O and reads no clock; time arrives
//! only as [`Input::Tick`].
//!
//! # Reading
//!
//! A *pass* lists the provider's changes page by page, from the cursor of the last complete pass,
//! or from the start when there is none. The pass stages what it lists and applies it to the
//! records only when its last page arrives and every resource it must read has been read, so the
//! records always reflect one complete pass:
//!
//! - **A truncated or failed listing never deletes.** A full listing removes the records it did
//!   not report only once it completes. A failed or expired listing discards what it staged.
//! - **An expired cursor** starts a full pass. The records stay as they are until it completes,
//!   and the edits waiting to be pushed are untouched.
//! - **A replayed change** carries a revision the engine already holds, and changes nothing. A
//!   record is replaced only when the provider revision differs, and never by a record from an
//!   older [`MapperVersion`](calendar_sync_model::MapperVersion) than its own.
//! - **A change listed without its body** (a `CalDAV` report) is read, once per pass, when its
//!   revision differs from the record's.
//! - **A partial page** is one more page.
//!
//! # Writing
//!
//! Each [`Edit`] waits in a queue per resource, in the order it was made, and the head of each
//! queue is pushed:
//!
//! 1. An update or delete reads the resource first, then plans from that read: an update merges
//!    its base, its own values and the read record field by field with [`merge`],
//!    and writes the result only if it differs, conditional on the read revision. A create is
//!    written at once, and fails if the resource exists. On a provider that assigns its own ids
//!    (Graph), a create first looks up its idempotency key with [`Request::Lookup`], and writes
//!    only if nothing was created under it.
//! 2. A write that loses a race, or whose response is lost, reads again and plans again. Because
//!    a plan always starts from a fresh read, repeating one never applies an edit twice.
//! 3. A permanent failure, a deleted target, or an edit past its `expires_at` ends the edit with
//!    a [`Notice`], and the next edit for the resource starts.
//!
//! A create's ids derive from its [`EditId`] through [`DerivedIds`](crate::DerivedIds), so a
//! retry from any device names the same resource. Where the provider assigns the id, the create
//! is queued under its idempotency key; once the provider names the event, the edits queued
//! behind the create move to that id, an edit that still names the key is redirected to it, and
//! [`Notice::Remapped`] tells the application.
//!
//! # Coalescing
//!
//! An edit queued behind another for the same resource, neither of them started, is folded into
//! it where the pair has one effect, so a run of offline edits costs one write:
//!
//! | Waiting | New | Becomes |
//! |---|---|---|
//! | update `base -> a` | update `a -> b` | update `base -> b` |
//! | update | delete | delete |
//! | create `a` | update `a -> b` | create `b` |
//! | create | delete | nothing: both end at once |
//!
//! Both edits must have the same [`Role`], an attendee's delete is never folded (it is
//! rejected), and the folded edit expires when the newer one does. Every edit folded in ends
//! with the same notice as the edit it was folded into, so each edit still ends exactly once.
//! An update whose base is not the waiting edit's result stays separate.
//!
//! Every read and write the engine sees updates the record it names, also mid-pass. One an edit
//! makes while a listing is still arriving may be older or newer than the listing's snapshot, so
//! the pass reads that resource again before it applies: a resource deleted just before a full
//! listing's snapshot is not kept alive by an edit's earlier write.
//!
//! # Ordering
//!
//! Several requests may be in flight at once, and their outcomes may arrive in any order. An
//! outcome is trusted only when its request was made after everything since learned about its
//! resource, a full listing's last page counting for every resource. Otherwise it may be older
//! than what the engine already holds: a read for an edit is made again before the edit plans
//! from it, and anything else is read again before it is recorded.
//!
//! # Resuming
//!
//! [`SyncState`] serializes with `serde`, after any step. A request whose outcome was never fed
//! back, because the application stopped, is repeated by [`Input::Resume`]: the engine treats
//! every request in flight as lost, and an outcome that arrives later for one of them is ignored.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use calendar_sync_model::{NormalizedEvent, Provider, ProviderEvent};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::merge::{Conflict, Refused, Role, merge};
use crate::seam::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Request, RequestId, ResourceId,
};

/// Names one local edit. The application mints it, once, and the engine reports it back in each
/// [`Notice`] about the edit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EditId(String);

impl EditId {
    /// Wraps an edit id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A local change to push to the provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    /// The edit's id.
    pub id: EditId,
    /// The resource it changes, or creates. A create names the id
    /// [`DerivedIds`](crate::DerivedIds) derives from its `id`: the Google id, the `CalDAV` href,
    /// or, where the provider assigns ids, the idempotency key.
    pub resource: ResourceId,
    /// What it does.
    pub operation: Operation,
    /// Which parts of the event the writer owns. An [`Role::Attendee`] cannot delete.
    pub role: Role,
    /// When the edit stops being worth pushing; `None` never expires. Compared with the time of
    /// the latest [`Input::Tick`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// What an [`Edit`] does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::large_enum_variant,
    reason = "an operation is moved once into the queue; boxing would only add an allocation"
)]
pub enum Operation {
    /// Create the resource with this record.
    Create(ProviderEvent),
    /// Change the resource from `base`, the fields the edit was made against, to `ours`.
    Update {
        /// The fields the edit was made against.
        base: NormalizedEvent,
        /// The edited fields.
        ours: NormalizedEvent,
    },
    /// Delete the resource.
    Delete,
}

/// One input to [`step`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::large_enum_variant,
    reason = "an input is consumed by the step it is passed to, never stored"
)]
pub enum Input {
    /// Start a pass, unless one is running.
    Sync,
    /// Queue a local edit.
    Edit(Edit),
    /// A request's outcome. An outcome for a request not in flight is ignored.
    Outcome {
        /// The request.
        request: RequestId,
        /// Its outcome, from the seam.
        outcome: Outcome,
    },
    /// The application's clock reads `now`. Expires the edits past their `expires_at`.
    Tick {
        /// The time.
        now: Timestamp,
    },
    /// The application restarted, and every request in flight is lost. They are made again.
    Resume,
}

/// One thing [`step`] asks the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "an effect is handed to the application once, never stored"
)]
pub enum Effect {
    /// Perform `request` through the provider's seam, and feed its outcome back as
    /// [`Input::Outcome`] with `id`.
    Request {
        /// The request's id.
        id: RequestId,
        /// The request.
        request: Request,
    },
    /// Tell the application something happened.
    Notify(Notice),
}

/// Something the application is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A pass completed. The records reflect the provider as of its cursor.
    Synced,
    /// A listing failed for good, for the reason given. Nothing it listed was applied.
    ListingFailed {
        /// The provider's reason.
        reason: String,
    },
    /// An edit is on the provider, or the provider already held it.
    Applied {
        /// The edit.
        edit: EditId,
        /// The units on which the provider's value was kept over the edit's.
        conflicts: Vec<Conflict>,
        /// The units the writer changed without owning them, which kept the provider's value.
        refused: Vec<Refused>,
    },
    /// An edit cannot be applied, and was dropped.
    Rejected {
        /// The edit.
        edit: EditId,
        /// Why.
        reason: Rejection,
    },
    /// An edit passed its `expires_at` before it was written, and was dropped.
    Expired {
        /// The edit.
        edit: EditId,
    },
    /// A create on a provider that assigns ids was queued under its idempotency key, and the
    /// provider named the event `to`. Edits waiting behind it moved to `to`, and an edit queued
    /// later under `from` is redirected while the engine still holds `to`.
    Remapped {
        /// The idempotency key the create was queued under.
        from: ResourceId,
        /// The id the provider gave the event.
        to: ResourceId,
    },
}

/// Why an edit was rejected.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rejection {
    /// The resource to update no longer exists.
    Gone,
    /// The resource to create exists already, and differs from the edit.
    Exists,
    /// The writer may not delete an event it does not own.
    NotOwner,
    /// The provider refused the request, or answered it in a way that cannot apply.
    Provider(String),
}

/// One provider calendar's records, the pass in progress, the edits waiting, and the requests in
/// flight. Serializable after any step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    records: BTreeMap<ResourceId, ProviderEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pass: Option<Pass>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    edits: BTreeMap<ResourceId, VecDeque<Pending>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    in_flight: BTreeMap<RequestId, Purpose>,
    #[serde(default)]
    next_request: u64,
    /// For each resource something was learned about while a request was in flight, the number
    /// of the first request made after it was learned. An outcome of an earlier request may have
    /// been performed before or after what was learned, so it is read again rather than trusted.
    /// Entries no request in flight predates are dropped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    learned: BTreeMap<ResourceId, u64>,
    /// The number of the first request made after the last page of a full listing arrived,
    /// while an earlier request is in flight. A full listing tells about every resource, those
    /// it leaves out too, so it is learned for all of them.
    #[serde(default, skip_serializing_if = "is_zero")]
    learned_all: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    now: Option<Timestamp>,
    /// Idempotency keys of creates, to the id the provider gave each event. Kept while the
    /// engine holds the event, waits to edit it, or has a request about it in flight.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    aliases: BTreeMap<ResourceId, ResourceId>,
}

/// A listing in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pass {
    /// Whether the pass lists everything, so that what it does not list is gone.
    full: bool,
    /// Where the page being listed starts, to ask again after a transient failure.
    from: ListFrom,
    /// What the pass has listed, to apply when it completes.
    staged: BTreeMap<ResourceId, Staged>,
    /// Every resource the listing reported, or a read after the listing ended found, to exist.
    seen: BTreeSet<ResourceId>,
    /// The resources an edit's read or write established while the listing was still arriving.
    /// The listing's snapshot may predate or follow each of them, so each is read again before
    /// the pass applies.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    observed: BTreeSet<ResourceId>,
    /// The cursor the last page ended with, once it arrived.
    done: Option<Cursor>,
}

/// What a pass listed for one resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Staged {
    Present(Box<ProviderEvent>),
    /// Listed at a revision the records do not hold, without its body: to be read.
    Unread,
    Removed,
}

/// An edit and what its latest plan found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    edit: Edit,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conflicts: Vec<Conflict>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refused: Vec<Refused>,
    /// A create was answered [`Failure::AlreadyExists`]. If the read that follows finds nothing,
    /// the id is held by a deleted resource (Google keeps a cancelled event's id), and creating
    /// again would fail forever.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    collided: bool,
    /// The later edits folded into this one, oldest first. Each ends with this edit's notice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    absorbed: Vec<EditId>,
}

/// What a request in flight is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Purpose {
    List,
    PassRead(ResourceId),
    EditRead(ResourceId),
    EditWrite(ResourceId),
    /// A read outside any pass, of a resource an outcome left in doubt.
    Refresh(ResourceId),
}

/// Takes one input, and returns the next state and the effects to perform, in order.
#[must_use]
pub fn step(mut state: SyncState, input: Input) -> (SyncState, Vec<Effect>) {
    let effects = state.apply(input);
    (state, effects)
}

impl SyncState {
    /// A state with no records, no cursor and nothing pending.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one input in place, and returns the effects to perform, in order. [`step`] by
    /// reference.
    pub fn apply(&mut self, input: Input) -> Vec<Effect> {
        let mut out = Vec::new();
        match input {
            Input::Sync => {
                if self.pass.is_none() {
                    self.start_pass(&mut out);
                }
            }
            Input::Edit(edit) => self.enqueue(edit, &mut out),
            Input::Outcome { request, outcome } => {
                if let Some(purpose) = self.in_flight.remove(&request) {
                    match purpose {
                        Purpose::List => self.on_list(request, outcome, &mut out),
                        Purpose::PassRead(resource) => {
                            self.on_pass_read(request, &resource, outcome, &mut out);
                        }
                        Purpose::EditRead(resource) => {
                            self.on_edit_read(request, resource, outcome, &mut out);
                        }
                        Purpose::EditWrite(resource) => {
                            self.on_edit_write(request, &resource, outcome, &mut out);
                        }
                        Purpose::Refresh(resource) => {
                            self.on_refresh(request, &resource, outcome, &mut out);
                        }
                    }
                    self.try_commit(&mut out);
                }
            }
            Input::Tick { now } => self.tick(now, &mut out),
            Input::Resume => self.resume(&mut out),
        }
        self.forget_settled();
        self.forget_aliases();
        out
    }

    /// The records, by resource id, as of the last complete pass and every read and write since.
    #[must_use]
    pub fn records(&self) -> &BTreeMap<ResourceId, ProviderEvent> {
        &self.records
    }

    /// The cursor the next incremental pass starts from, once a pass has completed.
    #[must_use]
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    /// Whether a pass is in progress.
    #[must_use]
    pub fn is_syncing(&self) -> bool {
        self.pass.is_some()
    }

    /// The edits not yet applied, rejected or expired, by resource, oldest first.
    pub fn pending_edits(&self) -> impl Iterator<Item = &Edit> {
        self.edits.values().flatten().map(|pending| &pending.edit)
    }

    /// The requests whose outcomes the engine is waiting for.
    pub fn in_flight(&self) -> impl Iterator<Item = RequestId> + '_ {
        self.in_flight.keys().copied()
    }

    /// Whether nothing is in flight: no pass, no edit and no request.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.pass.is_none() && self.edits.is_empty() && self.in_flight.is_empty()
    }

    fn request(&mut self, purpose: Purpose, request: Request, out: &mut Vec<Effect>) {
        let id = RequestId::new(self.next_request);
        self.next_request += 1;
        self.in_flight.insert(id, purpose);
        out.push(Effect::Request { id, request });
    }

    // Ordering. Outcomes may arrive in any order, and a request may be performed at any time
    // between being made and its outcome arriving. An outcome is therefore newer than what was
    // learned about its resource only if its request was made after that was learned.

    /// Notes that something was learned about `resource` now.
    fn learn(&mut self, resource: &ResourceId) {
        self.learned.insert(resource.clone(), self.next_request);
    }

    /// Whether an outcome of `request` about `resource` may be older than what was learned about
    /// it since the request was made.
    fn doubtful(&self, request: RequestId, resource: &ResourceId) -> bool {
        request.get() < self.learned_all
            || self
                .learned
                .get(resource)
                .is_some_and(|&since| request.get() < since)
    }

    /// Reads `resource` again, because an outcome about it could not be ordered against what was
    /// already learned. A pass in progress reads it before it applies. Otherwise a read of its
    /// own does, unless one is in flight.
    fn doubt(&mut self, resource: &ResourceId, out: &mut Vec<Effect>) {
        if let Some(pass) = &mut self.pass {
            if pass.done.is_some() {
                pass.staged.insert(resource.clone(), Staged::Unread);
            } else {
                pass.observed.insert(resource.clone());
            }
            self.read_unread(out);
        } else if !self
            .in_flight
            .values()
            .any(|purpose| *purpose == Purpose::Refresh(resource.clone()))
        {
            let request = Request::Read {
                resource: resource.clone(),
            };
            self.request(Purpose::Refresh(resource.clone()), request, out);
        }
    }

    fn on_refresh(
        &mut self,
        request: RequestId,
        resource: &ResourceId,
        outcome: Outcome,
        out: &mut Vec<Effect>,
    ) {
        let event = match outcome {
            Outcome::Read(event) => event,
            Outcome::Failed(Failure::NotFound) => None,
            Outcome::Failed(Failure::Transient) => {
                self.doubt(resource, out);
                return;
            }
            // The next pass lists it.
            _ => return,
        };
        if self.doubtful(request, resource) {
            self.doubt(resource, out);
        } else {
            self.observe(resource, event);
        }
    }

    /// Drops what was learned before every request in flight was made: no outcome can be
    /// doubted against it.
    fn forget_settled(&mut self) {
        match self.in_flight.keys().next().copied() {
            None => {
                self.learned.clear();
                self.learned_all = 0;
            }
            Some(oldest) => {
                self.learned.retain(|_, since| oldest.get() < *since);
                if self.learned_all <= oldest.get() {
                    self.learned_all = 0;
                }
            }
        }
    }

    // Reading.

    fn start_pass(&mut self, out: &mut Vec<Effect>) {
        let from = self.cursor.clone().map_or(ListFrom::Start, ListFrom::Since);
        self.pass = Some(Pass {
            full: from == ListFrom::Start,
            from: from.clone(),
            staged: BTreeMap::new(),
            seen: BTreeSet::new(),
            observed: BTreeSet::new(),
            done: None,
        });
        self.request(Purpose::List, Request::List { from }, out);
    }

    fn on_list(&mut self, request: RequestId, outcome: Outcome, out: &mut Vec<Effect>) {
        if self.pass.is_none() {
            return;
        }
        match outcome {
            Outcome::Listed(page) => {
                for change in page.changes {
                    let resource = match &change {
                        Change::Upsert { resource, .. } | Change::Remove { resource } => {
                            resource.clone()
                        }
                    };
                    let doubtful = self.doubtful(request, &resource);
                    self.learn(&resource);
                    let Some(pass) = self.pass.as_mut() else {
                        return;
                    };
                    match change {
                        // What was learned since this page was asked for may be older or newer
                        // than it: read it.
                        _ if doubtful => {
                            if matches!(change, Change::Upsert { .. }) {
                                pass.seen.insert(resource.clone());
                            }
                            pass.staged.insert(resource, Staged::Unread);
                        }
                        Change::Upsert {
                            resource,
                            revision,
                            event,
                        } => {
                            pass.seen.insert(resource.clone());
                            let held = self
                                .records
                                .get(&resource)
                                .is_some_and(|record| record.revision.as_ref() == Some(&revision));
                            if held {
                                pass.staged.remove(&resource);
                            } else {
                                let staged = event.map_or(Staged::Unread, |event| {
                                    Staged::Present(Box::new(event))
                                });
                                pass.staged.insert(resource, staged);
                            }
                        }
                        Change::Remove { resource } => {
                            pass.staged.insert(resource, Staged::Removed);
                        }
                    }
                }
                let Some(pass) = self.pass.as_mut() else {
                    return;
                };
                match page.next {
                    Next::More(cursor) => {
                        let from = ListFrom::Continue(cursor);
                        pass.from = from.clone();
                        self.request(Purpose::List, Request::List { from }, out);
                    }
                    Next::Done(cursor) => {
                        pass.done = Some(cursor);
                        if pass.full {
                            self.learned_all = self.next_request;
                        }
                    }
                }
                self.read_unread(out);
            }
            Outcome::Failed(Failure::CursorExpired) => {
                // The reads the expired pass asked for are not this pass's: their outcomes must
                // not stand in for reads after the new listing.
                self.in_flight
                    .retain(|_, purpose| !matches!(purpose, Purpose::PassRead(_)));
                self.cursor = None;
                self.start_pass(out);
            }
            Outcome::Failed(Failure::Transient) => {
                if let Some(pass) = &self.pass {
                    let from = pass.from.clone();
                    self.request(Purpose::List, Request::List { from }, out);
                }
            }
            other => self.abandon_pass(&other, out),
        }
    }

    fn on_pass_read(
        &mut self,
        request: RequestId,
        resource: &ResourceId,
        outcome: Outcome,
        out: &mut Vec<Effect>,
    ) {
        let event = match outcome {
            Outcome::Read(event) => event,
            Outcome::Failed(Failure::NotFound) => None,
            Outcome::Failed(Failure::Transient) => {
                self.read_unread(out);
                return;
            }
            other => {
                if self.pass.is_some() {
                    self.abandon_pass(&other, out);
                }
                return;
            }
        };
        if self.doubtful(request, resource) {
            self.doubt(resource, out);
            return;
        }
        // A read this pass asked for, after its listing reported the resource, is the newest word
        // on it. A read an earlier, abandoned pass asked for is like an edit's: it may predate
        // this pass's snapshot.
        let listed = self
            .pass
            .as_ref()
            .is_some_and(|pass| pass.staged.get(resource) == Some(&Staged::Unread));
        self.observe(resource, event);
        if listed && let Some(pass) = &mut self.pass {
            pass.observed.remove(resource);
        }
    }

    /// Reads every resource the pass listed without a body, that is not being read already.
    fn read_unread(&mut self, out: &mut Vec<Effect>) {
        let Some(pass) = &self.pass else {
            return;
        };
        let unread: Vec<ResourceId> = pass
            .staged
            .iter()
            .filter(|(resource, staged)| {
                **staged == Staged::Unread
                    && !self
                        .in_flight
                        .values()
                        .any(|purpose| *purpose == Purpose::PassRead((*resource).clone()))
            })
            .map(|(resource, _)| resource.clone())
            .collect();
        for resource in unread {
            self.request(
                Purpose::PassRead(resource.clone()),
                Request::Read { resource },
                out,
            );
        }
    }

    fn abandon_pass(&mut self, outcome: &Outcome, out: &mut Vec<Effect>) {
        self.pass = None;
        self.in_flight
            .retain(|_, purpose| !matches!(purpose, Purpose::List | Purpose::PassRead(_)));
        out.push(Effect::Notify(Notice::ListingFailed {
            reason: describe(outcome),
        }));
    }

    /// Applies the pass once its last page has arrived and nothing it listed is left to read.
    fn try_commit(&mut self, out: &mut Vec<Effect>) {
        let ready = self.pass.as_ref().is_some_and(|pass| {
            pass.done.is_some() && !pass.staged.values().any(|staged| *staged == Staged::Unread)
        });
        if !ready {
            return;
        }
        if let Some(pass) = &mut self.pass
            && !pass.observed.is_empty()
        {
            // What an edit saw while the listing arrived may be older or newer than the listing.
            // A read now is newer than both.
            for resource in core::mem::take(&mut pass.observed) {
                pass.staged.insert(resource, Staged::Unread);
            }
            self.read_unread(out);
            return;
        }
        let Some(pass) = self.pass.take() else {
            return;
        };
        for (resource, staged) in pass.staged {
            match staged {
                Staged::Present(event) => self.put_record(resource, *event),
                Staged::Removed => {
                    self.records.remove(&resource);
                }
                Staged::Unread => {}
            }
        }
        if pass.full {
            self.records
                .retain(|resource, _| pass.seen.contains(resource));
        }
        self.cursor = pass.done;
        out.push(Effect::Notify(Notice::Synced));
    }

    /// Records what a read or a write established about one resource: it is `event`, or it does
    /// not exist. A pass in progress takes it over what it listed.
    fn observe(&mut self, resource: &ResourceId, event: Option<ProviderEvent>) {
        self.learn(resource);
        match event {
            Some(event) => {
                let id = ResourceId::of(&event);
                self.learn(&id);
                if let Some(pass) = &mut self.pass {
                    pass.staged.remove(resource);
                    pass.staged.remove(&id);
                    if pass.done.is_some() {
                        pass.seen.insert(id.clone());
                    } else {
                        pass.observed.insert(id.clone());
                    }
                }
                self.put_record(id, event);
            }
            None => {
                if let Some(pass) = &mut self.pass {
                    pass.staged.remove(resource);
                    pass.seen.remove(resource);
                    if pass.done.is_none() {
                        pass.observed.insert(resource.clone());
                    }
                }
                self.records.remove(resource);
            }
        }
    }

    /// Replaces a record when the provider revision differs, unless the record came from a newer
    /// mapper.
    fn put_record(&mut self, resource: ResourceId, event: ProviderEvent) {
        let replace = self.records.get(&resource).is_none_or(|held| {
            event.mapper_version >= held.mapper_version
                && (event.revision != held.revision || event.mapper_version > held.mapper_version)
        });
        if replace {
            self.records.insert(resource, event);
        }
    }

    // Writing.

    fn enqueue(&mut self, mut edit: Edit, out: &mut Vec<Effect>) {
        if self.expired(&edit) {
            out.push(Effect::Notify(Notice::Expired { edit: edit.id }));
            return;
        }
        if let Some(to) = self.aliases.get(&edit.resource) {
            edit.resource = to.clone();
        }
        let resource = edit.resource.clone();
        let queue = self.edits.entry(resource.clone()).or_default();
        // Only an edit behind the head has never been started, so only it may be folded into.
        if queue.len() >= 2
            && let Some(waiting) = queue.back_mut()
        {
            match coalesce(&waiting.edit, &edit) {
                Some(Coalesced::Into(operation)) => {
                    waiting.edit.operation = operation;
                    waiting.edit.expires_at = edit.expires_at;
                    waiting.absorbed.push(edit.id);
                    return;
                }
                Some(Coalesced::Cancelled) => {
                    if let Some(waiting) = queue.pop_back() {
                        Self::notify(waiting, None, out);
                    }
                    out.push(Effect::Notify(Notice::Applied {
                        edit: edit.id,
                        conflicts: Vec::new(),
                        refused: Vec::new(),
                    }));
                    return;
                }
                None => {}
            }
        }
        queue.push_back(Pending {
            edit,
            conflicts: Vec::new(),
            refused: Vec::new(),
            collided: false,
            absorbed: Vec::new(),
        });
        if queue.len() == 1 {
            self.start_edit(&resource, out);
        }
    }

    /// Moves the edits waiting behind a create queued under its idempotency key `from` to `to`,
    /// the id the provider gave the event, and redirects later edits for `from` to it.
    fn remap(&mut self, from: &ResourceId, to: &ResourceId, out: &mut Vec<Effect>) {
        if from == to {
            return;
        }
        self.aliases.insert(from.clone(), to.clone());
        out.push(Effect::Notify(Notice::Remapped {
            from: from.clone(),
            to: to.clone(),
        }));
        let Some(queue) = self.edits.get_mut(from) else {
            return;
        };
        if queue.len() < 2 {
            return;
        }
        let behind: Vec<Pending> = queue.drain(1..).collect();
        let target = self.edits.entry(to.clone()).or_default();
        let idle = target.is_empty();
        for mut pending in behind {
            pending.edit.resource = to.clone();
            target.push_back(pending);
        }
        if idle {
            self.start_edit(to, out);
        }
    }

    /// Drops the aliases of events the engine no longer holds, waits to edit, or asks about.
    fn forget_aliases(&mut self) {
        if self.aliases.is_empty() {
            return;
        }
        let asked: BTreeSet<&ResourceId> = self
            .in_flight
            .values()
            .filter_map(|purpose| match purpose {
                Purpose::List => None,
                Purpose::PassRead(resource)
                | Purpose::EditRead(resource)
                | Purpose::EditWrite(resource)
                | Purpose::Refresh(resource) => Some(resource),
            })
            .collect();
        let keep: BTreeSet<ResourceId> = self
            .aliases
            .values()
            .filter(|to| {
                self.records.contains_key(*to)
                    || self.edits.contains_key(*to)
                    || asked.contains(to)
                    || self.pass.as_ref().is_some_and(|pass| {
                        pass.staged.contains_key(*to) || pass.observed.contains(*to)
                    })
            })
            .cloned()
            .collect();
        self.aliases.retain(|_, to| keep.contains(to));
    }

    fn expired(&self, edit: &Edit) -> bool {
        matches!((self.now, edit.expires_at), (Some(now), Some(at)) if at <= now)
    }

    fn head(&self, resource: &ResourceId) -> Option<&Pending> {
        self.edits.get(resource).and_then(VecDeque::front)
    }

    fn start_edit(&mut self, resource: &ResourceId, out: &mut Vec<Effect>) {
        let Some(head) = self.head(resource) else {
            return;
        };
        match &head.edit.operation {
            // Look before creating where the provider names the event, since only the lookup can
            // tell that an earlier attempt made it.
            Operation::Create(event) if assigns_ids(event) => self.read_for_edit(resource, out),
            Operation::Create(event) => {
                let request = Request::Create {
                    resource: resource.clone(),
                    event: event.clone(),
                };
                self.request(Purpose::EditWrite(resource.clone()), request, out);
            }
            Operation::Delete if matches!(head.edit.role, Role::Attendee { .. }) => {
                self.finish(resource, Some(&Rejection::NotOwner), out);
            }
            Operation::Update { .. } | Operation::Delete => self.read_for_edit(resource, out),
        }
    }

    fn read_for_edit(&mut self, resource: &ResourceId, out: &mut Vec<Effect>) {
        let lookup = self.head(resource).is_some_and(
            |head| matches!(&head.edit.operation, Operation::Create(event) if assigns_ids(event)),
        );
        let request = if lookup {
            Request::Lookup {
                key: resource.clone(),
            }
        } else {
            Request::Read {
                resource: resource.clone(),
            }
        };
        self.request(Purpose::EditRead(resource.clone()), request, out);
    }

    fn on_edit_read(
        &mut self,
        request: RequestId,
        resource: ResourceId,
        outcome: Outcome,
        out: &mut Vec<Effect>,
    ) {
        let theirs = match outcome {
            Outcome::Read(theirs) => theirs,
            Outcome::Failed(Failure::NotFound) => None,
            Outcome::Failed(Failure::Transient) => {
                if self.head(&resource).is_some() {
                    self.read_for_edit(&resource, out);
                }
                return;
            }
            other => {
                if self.head(&resource).is_some() {
                    self.finish(&resource, Some(&Rejection::Provider(describe(&other))), out);
                }
                return;
            }
        };
        if self.doubtful(request, &resource) {
            // The read may be older than what was learned since it was asked for, such as a
            // pass that applied a newer revision. Neither record it nor plan from it.
            if self.head(&resource).is_some() {
                self.read_for_edit(&resource, out);
            } else {
                self.doubt(&resource, out);
            }
            return;
        }
        self.observe(&resource, theirs.clone());
        let Some(head) = self.head(&resource) else {
            return;
        };
        let (operation, role) = (head.edit.operation.clone(), head.edit.role.clone());
        let collided = head.collided;
        match (&operation, theirs) {
            // Found under its idempotency key: an earlier attempt made it, whatever has been
            // done to it since.
            (Operation::Create(ours), Some(theirs)) if assigns_ids(ours) => {
                self.remap(&resource, &ResourceId::of(&theirs), out);
                self.finish(&resource, None, out);
            }
            (Operation::Create(ours), Some(theirs)) => {
                let rejection = (theirs.fields != ours.fields).then_some(Rejection::Exists);
                self.finish(&resource, rejection.as_ref(), out);
            }
            (Operation::Create(_), None) if collided => {
                self.finish(&resource, Some(&Rejection::Exists), out);
            }
            (Operation::Create(ours), None) => {
                let request = Request::Create {
                    resource: resource.clone(),
                    event: ours.clone(),
                };
                self.request(Purpose::EditWrite(resource), request, out);
            }
            (Operation::Update { .. }, None) => self.finish(&resource, Some(&Rejection::Gone), out),
            (Operation::Update { base, ours }, Some(theirs)) => {
                let merged = merge(base, ours, &theirs, &role);
                if let Some(head) = self.edits.get_mut(&resource).and_then(VecDeque::front_mut) {
                    head.conflicts = merged.conflicts;
                    head.refused = merged.refused;
                }
                if merged.changed {
                    let request = Request::Update {
                        resource: resource.clone(),
                        if_match: theirs.revision,
                        event: merged.event,
                    };
                    self.request(Purpose::EditWrite(resource), request, out);
                } else {
                    self.finish(&resource, None, out);
                }
            }
            (Operation::Delete, None) => self.finish(&resource, None, out),
            (Operation::Delete, Some(theirs)) => {
                let request = Request::Delete {
                    resource: resource.clone(),
                    if_match: theirs.revision,
                };
                self.request(Purpose::EditWrite(resource), request, out);
            }
        }
    }

    fn on_edit_write(
        &mut self,
        request: RequestId,
        resource: &ResourceId,
        outcome: Outcome,
        out: &mut Vec<Effect>,
    ) {
        let is_delete = self
            .head(resource)
            .is_some_and(|head| head.edit.operation == Operation::Delete);
        match outcome {
            Outcome::Written(event) => {
                let created = self
                    .head(resource)
                    .is_some_and(|head| matches!(head.edit.operation, Operation::Create(_)));
                let named = ResourceId::of(&event);
                self.observe_write(request, resource, Some(event), out);
                if created {
                    self.remap(resource, &named, out);
                }
                self.finish(resource, None, out);
            }
            Outcome::Deleted => {
                self.observe_write(request, resource, None, out);
                self.finish(resource, None, out);
            }
            Outcome::Failed(Failure::NotFound) if is_delete => {
                self.observe_write(request, resource, None, out);
                self.finish(resource, None, out);
            }
            Outcome::Failed(Failure::Rejected(reason)) => {
                self.finish(resource, Some(&Rejection::Provider(reason)), out);
            }
            Outcome::Failed(Failure::AlreadyExists) => {
                if let Some(head) = self.edits.get_mut(resource).and_then(VecDeque::front_mut) {
                    head.collided = true;
                }
                if self.head(resource).is_some() {
                    self.read_for_edit(resource, out);
                }
            }
            // A lost race, a lost response, or a create or update that found the resource in a
            // state it did not expect: plan again from a fresh read.
            _ => {
                if self.head(resource).is_some() {
                    self.read_for_edit(resource, out);
                }
            }
        }
    }

    /// Records what a write established, unless what was learned since it was asked for may be
    /// newer: then the resource is read again. The write landed either way.
    fn observe_write(
        &mut self,
        request: RequestId,
        resource: &ResourceId,
        event: Option<ProviderEvent>,
        out: &mut Vec<Effect>,
    ) {
        if self.doubtful(request, resource) {
            self.doubt(resource, out);
        } else {
            self.observe(resource, event);
        }
    }

    /// Ends the head edit for `resource`, applied or rejected, and starts the next.
    fn finish(
        &mut self,
        resource: &ResourceId,
        rejection: Option<&Rejection>,
        out: &mut Vec<Effect>,
    ) {
        let Some(queue) = self.edits.get_mut(resource) else {
            return;
        };
        let Some(done) = queue.pop_front() else {
            return;
        };
        if queue.is_empty() {
            self.edits.remove(resource);
        }
        Self::notify(done, rejection, out);
        self.start_edit(resource, out);
    }

    /// Ends `done` and every edit folded into it, applied or rejected.
    fn notify(done: Pending, rejection: Option<&Rejection>, out: &mut Vec<Effect>) {
        for edit in core::iter::once(done.edit.id).chain(done.absorbed) {
            out.push(Effect::Notify(match rejection {
                None => Notice::Applied {
                    edit,
                    conflicts: done.conflicts.clone(),
                    refused: done.refused.clone(),
                },
                Some(reason) => Notice::Rejected {
                    edit,
                    reason: reason.clone(),
                },
            }));
        }
    }

    // Time and restarts.

    fn tick(&mut self, now: Timestamp, out: &mut Vec<Effect>) {
        self.now = Some(now);
        let resources: Vec<ResourceId> = self.edits.keys().cloned().collect();
        for resource in resources {
            let writing = self
                .in_flight
                .values()
                .any(|purpose| *purpose == Purpose::EditWrite(resource.clone()));
            let Some(queue) = self.edits.get_mut(&resource) else {
                continue;
            };
            let mut kept = VecDeque::with_capacity(queue.len());
            let mut head_dropped = false;
            for (index, pending) in queue.drain(..).enumerate() {
                let expired = matches!(pending.edit.expires_at, Some(at) if at <= now)
                    && !(index == 0 && writing);
                if expired {
                    head_dropped |= index == 0;
                    for edit in core::iter::once(pending.edit.id).chain(pending.absorbed) {
                        out.push(Effect::Notify(Notice::Expired { edit }));
                    }
                } else {
                    kept.push_back(pending);
                }
            }
            if head_dropped {
                // The read planned for the dropped edit must not plan the next one too.
                self.in_flight
                    .retain(|_, purpose| *purpose != Purpose::EditRead(resource.clone()));
            }
            if kept.is_empty() {
                self.edits.remove(&resource);
            } else {
                *queue = kept;
                if head_dropped {
                    self.start_edit(&resource, out);
                }
            }
        }
    }

    fn resume(&mut self, out: &mut Vec<Effect>) {
        let refreshes: Vec<ResourceId> = self
            .in_flight
            .values()
            .filter_map(|purpose| match purpose {
                Purpose::Refresh(resource) => Some(resource.clone()),
                _ => None,
            })
            .collect();
        self.in_flight.clear();
        for resource in refreshes {
            self.doubt(&resource, out);
        }
        if let Some(pass) = &self.pass {
            if pass.done.is_none() {
                let from = pass.from.clone();
                self.request(Purpose::List, Request::List { from }, out);
            }
            self.read_unread(out);
        }
        let resources: Vec<ResourceId> = self.edits.keys().cloned().collect();
        for resource in resources {
            if let Some(head) = self.edits.get_mut(&resource).and_then(VecDeque::front_mut) {
                head.conflicts.clear();
                head.refused.clear();
            }
            self.start_edit(&resource, out);
        }
    }
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes fields by reference; the lint fires on x86_64 but not on wasm32"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Whether the provider of `event` assigns the ids of the events created on it, so that a create
/// is queued under its idempotency key and looked up before it is sent.
fn assigns_ids(event: &ProviderEvent) -> bool {
    event.ids.provider == Provider::Graph
}

/// What folding a new edit into the edit waiting before it gives.
#[allow(
    clippy::large_enum_variant,
    reason = "a result is matched once, right after it is made; boxing would only add an allocation"
)]
enum Coalesced {
    /// One edit with this operation.
    Into(Operation),
    /// Nothing: the two edits undo each other.
    Cancelled,
}

/// Folds `next` into `waiting`, two edits for one resource that have not started, where the pair
/// has the effect of one edit.
fn coalesce(waiting: &Edit, next: &Edit) -> Option<Coalesced> {
    if waiting.role != next.role {
        return None;
    }
    let deletes = matches!(next.role, Role::Owner);
    match (&waiting.operation, &next.operation) {
        (
            Operation::Update { base, ours },
            Operation::Update {
                base: from,
                ours: to,
            },
        ) if from == ours => Some(Coalesced::Into(Operation::Update {
            base: base.clone(),
            ours: to.clone(),
        })),
        (Operation::Update { .. }, Operation::Delete) if deletes => {
            Some(Coalesced::Into(Operation::Delete))
        }
        (Operation::Create(event), Operation::Update { base, ours }) if *base == event.fields => {
            let mut event = event.clone();
            event.fields = ours.clone();
            Some(Coalesced::Into(Operation::Create(event)))
        }
        (Operation::Create(_), Operation::Delete) if deletes => Some(Coalesced::Cancelled),
        _ => None,
    }
}

/// A short description of an outcome the engine could not use, for a notice.
fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Failed(Failure::Rejected(reason)) => reason.clone(),
        Outcome::Failed(failure) => format!("unexpected failure: {failure:?}"),
        Outcome::Listed(_) => "unexpected listing".to_owned(),
        Outcome::Read(_) => "unexpected read".to_owned(),
        Outcome::Written(_) => "unexpected write".to_owned(),
        Outcome::Deleted => "unexpected delete".to_owned(),
    }
}
