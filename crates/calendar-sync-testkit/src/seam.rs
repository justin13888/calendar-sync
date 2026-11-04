//! The engine's [`ProviderSeam`] for each fake, so the sync engine can be driven against them.
//!
//! [`GoogleSeam`], [`GraphSeam`] and [`CalDavSeam`] translate each engine [`Request`] into the
//! fake's own call, and the fake's answer into an [`Outcome`], the way a provider crate's seam does
//! for the real service. Each fake gains a `serve` method that performs one call, and each seam a
//! `perform` method that does all three steps at once:
//!
//! ```
//! use calendar_sync_engine::{ListFrom, Next, Outcome, Request};
//! use calendar_sync_testkit::FaultPlan;
//! use calendar_sync_testkit::google::FakeGoogle;
//! use calendar_sync_testkit::seam::GoogleSeam;
//!
//! let mut google = FakeGoogle::new(FaultPlan::none());
//! let seam = GoogleSeam::new(Some(50));
//! let outcome = seam.perform(&mut google, &Request::List { from: ListFrom::Start });
//! let Outcome::Listed(page) = outcome else { panic!() };
//! assert!(page.changes.is_empty());
//! assert!(matches!(page.next, Next::Done(_)));
//! ```
//!
//! The fakes' bodies are opaque, so the seams store each event's [`NormalizedEvent`] as JSON:
//! as the whole body for Google, under an `"event"` key for Graph (so that a `PATCH` replaces it
//! whole), and as JSON text for `CalDAV`. A body that does not decode is
//! [`Failure::Rejected`]. Every event the seams produce carries [`MAPPER_VERSION`].
//!
//! [`GraphSeam`] creates through `POST`, which Graph makes idempotent only through
//! `transactionId`. It sends the create's [`ResourceId`], the idempotency key
//! [`DerivedIds`](calendar_sync_engine::DerivedIds) derives from the edit, as the
//! `transactionId`, and stores it in the extended property [`KEY_PROPERTY`], which
//! [`Request::Lookup`] filters on. The engine looks the key up before every create, so a create
//! is repeated only when nothing was made under it; the `transactionId` covers what the lookup
//! cannot, two attempts in flight at once.
//!
//! # Duplicate risk
//!
//! What a create repeated by the engine (after a lost response, a restart, or on another device)
//! can still do, per provider, as `tests/queue.rs` in the engine asserts against these fakes:
//!
//! | Provider | Repeated create | Create repeated after the event was deleted |
//! |---|---|---|
//! | Google | Refused: the id is in use | Refused: a deleted event keeps its id |
//! | Graph | One event while the `transactionId` is remembered; a duplicate once it is forgotten | A new event |
//! | `CalDAV` | Refused by `If-None-Match: *` | The event comes back: the href is free again |

use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::{MapperVersion, NormalizedEvent, Provider, ProviderEvent, ProviderIds};
use calendar_sync_model::{Passthrough, ProviderRevision};
use serde_json::{Value, json};

use crate::caldav::{self, FakeCalDav, Member, Precondition, SyncCollection, SyncReport};
use crate::google::{self, EventStatus, EventsPage, FakeGoogle, ListRequest};
use crate::graph::{self, DeltaItem, DeltaPage, DeltaRequest, FakeGraph};

/// The id of the Graph single-value extended property that holds a create's idempotency key.
pub const KEY_PROPERTY: &str = "String {0d9b5b4e-6f5c-4c39-9a51-4c3c5d0e7a21} Name calendarSyncKey";

/// The mapper version of every event the seams produce.
pub const MAPPER_VERSION: MapperVersion = MapperVersion::new(1);

fn provider_event(
    provider: Provider,
    id: &str,
    revision: &str,
    fields: NormalizedEvent,
) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(provider, id),
        revision: Some(ProviderRevision::new(revision)),
        mapper_version: MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    }
}

fn decode(body: &Value) -> Result<NormalizedEvent, Failure> {
    serde_json::from_value(body.clone())
        .map_err(|error| Failure::Rejected(format!("undecodable body: {error}")))
}

fn encode(fields: &NormalizedEvent) -> Value {
    serde_json::to_value(fields).unwrap_or(Value::Null)
}

fn revision(request: &Request) -> Option<String> {
    match request {
        Request::Update { if_match, .. } | Request::Delete { if_match, .. } => if_match
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        _ => None,
    }
}

fn listed(changes: Result<Vec<Change>, Failure>, next: Option<Next>) -> Outcome {
    match (changes, next) {
        (Ok(changes), Some(next)) => Outcome::Listed(Page { changes, next }),
        (Err(failure), _) => Outcome::Failed(failure),
        (Ok(_), None) => Outcome::Failed(Failure::Rejected("a page with no token".to_owned())),
    }
}

fn mismatched() -> Outcome {
    Outcome::Failed(Failure::Rejected(
        "a response of the wrong kind for the request".to_owned(),
    ))
}

// Google

/// A call to [`FakeGoogle`].
#[derive(Clone, Debug, PartialEq)]
pub enum GoogleCall {
    /// `events.list`.
    List(ListRequest),
    /// `events.get`.
    Get {
        /// The event id.
        id: String,
    },
    /// `events.insert` with a client-chosen id.
    Insert {
        /// The event id.
        id: String,
        /// The body.
        body: Value,
    },
    /// `events.update`.
    Update {
        /// The event id.
        id: String,
        /// `If-Match`.
        if_match: Option<String>,
        /// The body.
        body: Value,
    },
    /// `events.delete`.
    Delete {
        /// The event id.
        id: String,
        /// `If-Match`.
        if_match: Option<String>,
    },
}

/// What [`FakeGoogle`] answered.
#[derive(Clone, Debug, PartialEq)]
pub enum GoogleResponse {
    /// To [`GoogleCall::List`].
    Page(Result<EventsPage, google::Error>),
    /// To a get, insert or update.
    Event(Result<google::Event, google::Error>),
    /// To a delete.
    Deleted(Result<(), google::Error>),
}

impl FakeGoogle {
    /// Performs one call.
    pub fn serve(&mut self, call: GoogleCall) -> GoogleResponse {
        match call {
            GoogleCall::List(request) => GoogleResponse::Page(self.list(&request)),
            GoogleCall::Get { id } => GoogleResponse::Event(self.get(&id)),
            GoogleCall::Insert { id, body } => GoogleResponse::Event(self.insert(&id, body)),
            GoogleCall::Update { id, if_match, body } => {
                GoogleResponse::Event(self.update(&id, if_match.as_deref(), body))
            }
            GoogleCall::Delete { id, if_match } => {
                GoogleResponse::Deleted(self.delete(&id, if_match.as_deref()))
            }
        }
    }
}

/// The seam for [`FakeGoogle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GoogleSeam {
    page_size: Option<usize>,
}

impl GoogleSeam {
    /// A seam that lists `page_size` events a page, or all at once for `None`.
    #[must_use]
    pub fn new(page_size: Option<usize>) -> Self {
        Self { page_size }
    }

    /// The body the seam stores for `fields`, for a test that writes as another client.
    #[must_use]
    pub fn body(fields: &NormalizedEvent) -> Value {
        encode(fields)
    }

    /// Performs `request` on `fake`, returning its outcome.
    pub fn perform(&self, fake: &mut FakeGoogle, request: &Request) -> Outcome {
        self.outcome(request, fake.serve(self.call(request)))
    }

    fn event(event: &google::Event) -> Result<Option<ProviderEvent>, Failure> {
        match (&event.status, &event.body) {
            (EventStatus::Confirmed, Some(body)) => Ok(Some(provider_event(
                Provider::Google,
                &event.id,
                &event.etag,
                decode(body)?,
            ))),
            _ => Ok(None),
        }
    }

    fn failure(error: google::Error) -> Failure {
        match error {
            google::Error::NotFound | google::Error::Gone => Failure::NotFound,
            google::Error::Conflict => Failure::AlreadyExists,
            google::Error::PreconditionFailed => Failure::PreconditionFailed,
            google::Error::LostResponse => Failure::Transient,
        }
    }
}

impl ProviderSeam for GoogleSeam {
    type Call = GoogleCall;
    type Response = GoogleResponse;

    fn call(&self, request: &Request) -> GoogleCall {
        match request {
            Request::List { from } => {
                let mut list = ListRequest {
                    max_results: self.page_size,
                    ..ListRequest::default()
                };
                match from {
                    ListFrom::Start => {}
                    ListFrom::Since(cursor) => list.sync_token = Some(cursor.as_str().to_owned()),
                    ListFrom::Continue(cursor) => {
                        list.page_token = Some(cursor.as_str().to_owned());
                    }
                }
                GoogleCall::List(list)
            }
            // The key is the event id, so a lookup is a read.
            Request::Read { resource } | Request::Lookup { key: resource } => GoogleCall::Get {
                id: resource.as_str().to_owned(),
            },
            Request::Create { resource, event } => GoogleCall::Insert {
                id: resource.as_str().to_owned(),
                body: encode(&event.fields),
            },
            Request::Update {
                resource, event, ..
            } => GoogleCall::Update {
                id: resource.as_str().to_owned(),
                if_match: revision(request),
                body: encode(&event.fields),
            },
            Request::Delete { resource, .. } => GoogleCall::Delete {
                id: resource.as_str().to_owned(),
                if_match: revision(request),
            },
        }
    }

    fn outcome(&self, request: &Request, response: GoogleResponse) -> Outcome {
        match (request, response) {
            (Request::List { .. }, GoogleResponse::Page(Ok(page))) => {
                let changes = page
                    .items
                    .iter()
                    .map(|event| {
                        let resource = ResourceId::new(event.id.clone());
                        Ok(match Self::event(event)? {
                            Some(event) => Change::Upsert {
                                resource,
                                revision: ProviderRevision::new(event_etag(&event)),
                                event: Some(event),
                            },
                            None => Change::Remove { resource },
                        })
                    })
                    .collect();
                let next = page
                    .next_page_token
                    .map(|token| Next::More(Cursor::new(token)))
                    .or_else(|| {
                        page.next_sync_token
                            .map(|token| Next::Done(Cursor::new(token)))
                    });
                listed(changes, next)
            }
            (Request::List { .. }, GoogleResponse::Page(Err(google::Error::Gone))) => {
                Outcome::Failed(Failure::CursorExpired)
            }
            (Request::Read { .. } | Request::Lookup { .. }, GoogleResponse::Event(Ok(event))) => {
                Self::event(&event).map_or_else(Outcome::Failed, Outcome::Read)
            }
            (
                Request::Read { .. } | Request::Lookup { .. },
                GoogleResponse::Event(Err(google::Error::NotFound)),
            ) => Outcome::Read(None),
            (Request::Create { .. } | Request::Update { .. }, GoogleResponse::Event(Ok(event))) => {
                match Self::event(&event) {
                    Ok(Some(event)) => Outcome::Written(event),
                    Ok(None) => mismatched(),
                    Err(failure) => Outcome::Failed(failure),
                }
            }
            (Request::Delete { .. }, GoogleResponse::Deleted(Ok(()))) => Outcome::Deleted,
            (
                _,
                GoogleResponse::Page(Err(error))
                | GoogleResponse::Event(Err(error))
                | GoogleResponse::Deleted(Err(error)),
            ) => Outcome::Failed(Self::failure(error)),
            _ => mismatched(),
        }
    }
}

fn event_etag(event: &ProviderEvent) -> &str {
    event.revision.as_ref().map_or("", ProviderRevision::as_str)
}

// Graph

/// A call to [`FakeGraph`].
#[derive(Clone, Debug, PartialEq)]
pub enum GraphCall {
    /// `GET /me/calendarView/delta`, or a link it returned.
    Delta(DeltaRequest),
    /// `GET /me/events/{id}`.
    Get {
        /// The event id.
        id: String,
    },
    /// `POST /me/events`.
    Create {
        /// The body.
        body: Value,
        /// `transactionId`.
        transaction_id: Option<String>,
    },
    /// `GET /me/events?$filter=` on one single-value extended property.
    Find {
        /// The property's id.
        property: String,
        /// The value to match.
        value: String,
    },
    /// `PATCH /me/events/{id}`.
    Patch {
        /// The event id.
        id: String,
        /// `If-Match`.
        if_match: Option<String>,
        /// The properties to replace.
        body: Value,
    },
    /// `DELETE /me/events/{id}`.
    Delete {
        /// The event id.
        id: String,
        /// `If-Match`.
        if_match: Option<String>,
    },
}

/// What [`FakeGraph`] answered.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphResponse {
    /// To [`GraphCall::Delta`].
    Page(Result<DeltaPage, graph::Error>),
    /// To a get, create or patch.
    Event(Result<graph::Event, graph::Error>),
    /// To a find.
    Found(Result<Vec<graph::Event>, graph::Error>),
    /// To a delete.
    Deleted(Result<(), graph::Error>),
}

impl FakeGraph {
    /// Performs one call.
    pub fn serve(&mut self, call: GraphCall) -> GraphResponse {
        match call {
            GraphCall::Delta(request) => GraphResponse::Page(self.delta(&request)),
            GraphCall::Get { id } => GraphResponse::Event(self.get(&id)),
            GraphCall::Create {
                body,
                transaction_id,
            } => GraphResponse::Event(self.create_with(body, transaction_id.as_deref())),
            GraphCall::Find { property, value } => {
                GraphResponse::Found(self.find(&property, &value))
            }
            GraphCall::Patch { id, if_match, body } => {
                GraphResponse::Event(self.patch(&id, if_match.as_deref(), body))
            }
            GraphCall::Delete { id, if_match } => {
                GraphResponse::Deleted(self.delete(&id, if_match.as_deref()))
            }
        }
    }
}

/// The seam for [`FakeGraph`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphSeam {
    page_size: Option<usize>,
}

impl GraphSeam {
    /// A seam that asks for `page_size` events a page, or all at once for `None`.
    #[must_use]
    pub fn new(page_size: Option<usize>) -> Self {
        Self { page_size }
    }

    /// The body the seam stores for `fields`, for a test that writes as another client.
    #[must_use]
    pub fn body(fields: &NormalizedEvent) -> Value {
        json!({ "event": encode(fields) })
    }

    /// Performs `request` on `fake`, returning its outcome.
    pub fn perform(&self, fake: &mut FakeGraph, request: &Request) -> Outcome {
        self.outcome(request, fake.serve(self.call(request)))
    }

    fn event(event: &graph::Event) -> Result<ProviderEvent, Failure> {
        let body = event.body.get("event").unwrap_or(&Value::Null);
        Ok(provider_event(
            Provider::Graph,
            &event.id,
            &event.change_key,
            decode(body)?,
        ))
    }

    fn failure(error: graph::Error) -> Failure {
        match error {
            graph::Error::NotFound => Failure::NotFound,
            graph::Error::SyncStateNotFound => Failure::CursorExpired,
            graph::Error::PreconditionFailed => Failure::PreconditionFailed,
            graph::Error::LostResponse => Failure::Transient,
        }
    }
}

impl ProviderSeam for GraphSeam {
    type Call = GraphCall;
    type Response = GraphResponse;

    fn call(&self, request: &Request) -> GraphCall {
        match request {
            Request::List { from } => GraphCall::Delta(DeltaRequest {
                link: match from {
                    ListFrom::Start => None,
                    ListFrom::Since(cursor) | ListFrom::Continue(cursor) => {
                        Some(cursor.as_str().to_owned())
                    }
                },
                max_page_size: self.page_size,
            }),
            Request::Read { resource } => GraphCall::Get {
                id: resource.as_str().to_owned(),
            },
            // The resource is the idempotency key: sent as the `transactionId`, and kept in an
            // extended property for a later lookup.
            Request::Create { resource, event } => {
                let mut body = Self::body(&event.fields);
                body["singleValueExtendedProperties"] =
                    json!([{ "id": KEY_PROPERTY, "value": resource.as_str() }]);
                GraphCall::Create {
                    body,
                    transaction_id: Some(resource.as_str().to_owned()),
                }
            }
            Request::Lookup { key } => GraphCall::Find {
                property: KEY_PROPERTY.to_owned(),
                value: key.as_str().to_owned(),
            },
            Request::Update {
                resource, event, ..
            } => GraphCall::Patch {
                id: resource.as_str().to_owned(),
                if_match: revision(request),
                body: Self::body(&event.fields),
            },
            Request::Delete { resource, .. } => GraphCall::Delete {
                id: resource.as_str().to_owned(),
                if_match: revision(request),
            },
        }
    }

    fn outcome(&self, request: &Request, response: GraphResponse) -> Outcome {
        match (request, response) {
            (Request::List { .. }, GraphResponse::Page(Ok(page))) => {
                let changes = page
                    .value
                    .iter()
                    .map(|item| {
                        Ok(match item {
                            DeltaItem::Changed(event) => Change::Upsert {
                                resource: ResourceId::new(event.id.clone()),
                                revision: ProviderRevision::new(event.change_key.clone()),
                                event: Some(Self::event(event)?),
                            },
                            DeltaItem::Removed { id } => Change::Remove {
                                resource: ResourceId::new(id.clone()),
                            },
                        })
                    })
                    .collect();
                let next = page
                    .next_link
                    .map(|link| Next::More(Cursor::new(link)))
                    .or_else(|| page.delta_link.map(|link| Next::Done(Cursor::new(link))));
                listed(changes, next)
            }
            (Request::Read { .. }, GraphResponse::Event(Ok(event))) => {
                Self::event(&event).map_or_else(Outcome::Failed, |event| Outcome::Read(Some(event)))
            }
            (Request::Read { .. }, GraphResponse::Event(Err(graph::Error::NotFound))) => {
                Outcome::Read(None)
            }
            (Request::Lookup { .. }, GraphResponse::Found(Ok(events))) => events
                .first()
                .map(Self::event)
                .transpose()
                .map_or_else(Outcome::Failed, Outcome::Read),
            (Request::Create { .. } | Request::Update { .. }, GraphResponse::Event(Ok(event))) => {
                Self::event(&event).map_or_else(Outcome::Failed, Outcome::Written)
            }
            (Request::Delete { .. }, GraphResponse::Deleted(Ok(()))) => Outcome::Deleted,
            (
                _,
                GraphResponse::Page(Err(error))
                | GraphResponse::Event(Err(error))
                | GraphResponse::Found(Err(error))
                | GraphResponse::Deleted(Err(error)),
            ) => Outcome::Failed(Self::failure(error)),
            _ => mismatched(),
        }
    }
}

// CalDAV

/// A call to [`FakeCalDav`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalDavCall {
    /// `REPORT` `DAV:sync-collection`.
    SyncCollection(SyncCollection),
    /// `GET` a member.
    Get {
        /// The member's href.
        href: String,
    },
    /// `PUT` a member.
    Put {
        /// The member's href.
        href: String,
        /// `If-Match` or `If-None-Match`.
        precondition: Precondition,
        /// The body.
        body: String,
    },
    /// `DELETE` a member.
    Delete {
        /// The member's href.
        href: String,
        /// `If-Match`.
        if_match: Option<String>,
    },
}

/// What [`FakeCalDav`] answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalDavResponse {
    /// To [`CalDavCall::SyncCollection`].
    Report(Result<SyncReport, caldav::Error>),
    /// To a `GET`.
    Member(Result<Member, caldav::Error>),
    /// To a `PUT`: the new `ETag`.
    Put(Result<String, caldav::Error>),
    /// To a `DELETE`.
    Deleted(Result<(), caldav::Error>),
}

impl FakeCalDav {
    /// Performs one call.
    pub fn serve(&mut self, call: CalDavCall) -> CalDavResponse {
        match call {
            CalDavCall::SyncCollection(request) => {
                CalDavResponse::Report(self.sync_collection(&request))
            }
            CalDavCall::Get { href } => CalDavResponse::Member(self.get(&href)),
            CalDavCall::Put {
                href,
                precondition,
                body,
            } => CalDavResponse::Put(self.put(&href, &precondition, body)),
            CalDavCall::Delete { href, if_match } => {
                CalDavResponse::Deleted(self.delete(&href, if_match.as_deref()))
            }
        }
    }
}

/// The seam for [`FakeCalDav`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CalDavSeam {
    limit: Option<usize>,
}

impl CalDavSeam {
    /// A seam that asks for at most `limit` members a report, or all at once for `None`.
    #[must_use]
    pub fn new(limit: Option<usize>) -> Self {
        Self { limit }
    }

    /// The body the seam stores for `fields`, for a test that writes as another client.
    #[must_use]
    pub fn body(fields: &NormalizedEvent) -> String {
        encode(fields).to_string()
    }

    /// Performs `request` on `fake`, returning its outcome.
    pub fn perform(&self, fake: &mut FakeCalDav, request: &Request) -> Outcome {
        self.outcome(request, fake.serve(self.call(request)))
    }

    fn failure(error: caldav::Error) -> Failure {
        match error {
            caldav::Error::InvalidSyncToken => Failure::CursorExpired,
            caldav::Error::NotFound => Failure::NotFound,
            caldav::Error::PreconditionFailed => Failure::PreconditionFailed,
            caldav::Error::LostResponse => Failure::Transient,
        }
    }
}

impl ProviderSeam for CalDavSeam {
    type Call = CalDavCall;
    type Response = CalDavResponse;

    fn call(&self, request: &Request) -> CalDavCall {
        match request {
            Request::List { from } => CalDavCall::SyncCollection(SyncCollection {
                sync_token: match from {
                    ListFrom::Start => None,
                    ListFrom::Since(cursor) | ListFrom::Continue(cursor) => {
                        Some(cursor.as_str().to_owned())
                    }
                },
                limit: self.limit,
            }),
            // The key is the href, so a lookup is a read.
            Request::Read { resource } | Request::Lookup { key: resource } => CalDavCall::Get {
                href: resource.as_str().to_owned(),
            },
            Request::Create { resource, event } => CalDavCall::Put {
                href: resource.as_str().to_owned(),
                precondition: Precondition::IfNoneMatch,
                body: Self::body(&event.fields),
            },
            Request::Update {
                resource, event, ..
            } => CalDavCall::Put {
                href: resource.as_str().to_owned(),
                precondition: revision(request).map_or(Precondition::None, Precondition::IfMatch),
                body: Self::body(&event.fields),
            },
            Request::Delete { resource, .. } => CalDavCall::Delete {
                href: resource.as_str().to_owned(),
                if_match: revision(request),
            },
        }
    }

    fn outcome(&self, request: &Request, response: CalDavResponse) -> Outcome {
        match (request, response) {
            (Request::List { .. }, CalDavResponse::Report(Ok(report))) => {
                let changes = report
                    .responses
                    .into_iter()
                    .map(|response| match response {
                        caldav::SyncResponse::Changed { href, etag } => Change::Upsert {
                            resource: ResourceId::new(href),
                            revision: ProviderRevision::new(etag),
                            event: None,
                        },
                        caldav::SyncResponse::Removed { href } => Change::Remove {
                            resource: ResourceId::new(href),
                        },
                    })
                    .collect();
                let cursor = Cursor::new(report.sync_token);
                let next = if report.truncated {
                    Next::More(cursor)
                } else {
                    Next::Done(cursor)
                };
                listed(Ok(changes), Some(next))
            }
            (Request::Read { .. } | Request::Lookup { .. }, CalDavResponse::Member(Ok(member))) => {
                let body = serde_json::from_str(&member.body).unwrap_or(Value::Null);
                match decode(&body) {
                    Ok(fields) => Outcome::Read(Some(provider_event(
                        Provider::CalDav,
                        &member.href,
                        &member.etag,
                        fields,
                    ))),
                    Err(failure) => Outcome::Failed(failure),
                }
            }
            (
                Request::Read { .. } | Request::Lookup { .. },
                CalDavResponse::Member(Err(caldav::Error::NotFound)),
            ) => Outcome::Read(None),
            (
                Request::Create {
                    resource, event, ..
                }
                | Request::Update {
                    resource, event, ..
                },
                CalDavResponse::Put(Ok(etag)),
            ) => Outcome::Written(provider_event(
                Provider::CalDav,
                resource.as_str(),
                &etag,
                event.fields.clone(),
            )),
            // `If-None-Match: *` failed: the member exists.
            (
                Request::Create { .. },
                CalDavResponse::Put(Err(caldav::Error::PreconditionFailed)),
            ) => Outcome::Failed(Failure::AlreadyExists),
            (Request::Delete { .. }, CalDavResponse::Deleted(Ok(()))) => Outcome::Deleted,
            (
                _,
                CalDavResponse::Report(Err(error))
                | CalDavResponse::Member(Err(error))
                | CalDavResponse::Put(Err(error))
                | CalDavResponse::Deleted(Err(error)),
            ) => Outcome::Failed(Self::failure(error)),
            _ => mismatched(),
        }
    }
}
