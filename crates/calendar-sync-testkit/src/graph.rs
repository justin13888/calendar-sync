//! A fake Microsoft Graph calendar.
//!
//! It follows `references/graph/delta-query-events.md` and `delta-query-overview.md`: a delta
//! round returns pages joined by `@odata.nextLink` and ends with an `@odata.deltaLink`; the
//! initial round reports only existing events, later rounds report deletions as `@removed`; a
//! replay re-delivers a change already seen; an expired link answers `410 Gone`
//! (`syncStateNotFound`). The server assigns event ids, and each version of an event carries a
//! `changeKey`, which `If-Match` on `PATCH` and `DELETE` compares. Bodies are opaque JSON;
//! `PATCH` replaces the top-level properties it names.
//!
//! A create may carry a `transactionId`. A retry with the same one, while the server remembers
//! it and the event it made is still there, returns that event instead of making another. How
//! long and across which clients Graph remembers one is unverified, so the fake remembers it for
//! [`FakeGraph::with_transaction_lifetime`] calls, or for ever. Single-value extended properties
//! in a body (`singleValueExtendedProperties`) can be filtered on with [`FakeGraph::find`].
//!
//! The date window that `calendarView/delta` requires is not modelled: the fake reports every
//! event.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::fault::{CallKind, Fault, FaultPlan, InjectedFault};
use crate::server::{Resource, Server, SyncFailure};

/// The base of every link the fake hands out.
const DELTA_URL: &str = "https://graph.example.invalid/v1.0/me/calendarView/delta";

/// An event as the fake returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The id the server assigned.
    pub id: String,
    /// The `changeKey`, unique to this version of the event.
    pub change_key: String,
    /// The event's body.
    pub body: Value,
}

/// One entry of a delta page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeltaItem {
    /// The event was created or changed.
    Changed(Event),
    /// The event was deleted (`@removed`).
    Removed {
        /// The deleted event's id.
        id: String,
    },
}

/// A `delta` request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaRequest {
    /// The `@odata.nextLink` or `@odata.deltaLink` to follow; `None` starts a new round.
    pub link: Option<String>,
    /// `Prefer: odata.maxpagesize`; `None` returns the whole round on one page.
    pub max_page_size: Option<usize>,
}

/// One page of a delta round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaPage {
    /// The entries on this page.
    pub value: Vec<DeltaItem>,
    /// `@odata.nextLink`, when the round continues.
    pub next_link: Option<String>,
    /// `@odata.deltaLink`, on the last page of the round.
    pub delta_link: Option<String>,
}

/// A failed call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// `404 Not Found`: no event has this id, or it was deleted.
    NotFound,
    /// `410 Gone` with `syncStateNotFound`: the link expired. The client starts a new round.
    SyncStateNotFound,
    /// `412 Precondition Failed`: `If-Match` did not match the event's current `changeKey`.
    PreconditionFailed,
    /// The response was lost. The call may have taken effect.
    LostResponse,
}

impl Error {
    /// The HTTP status the error stands for, or `None` for a lost response.
    #[must_use]
    pub fn status(self) -> Option<u16> {
        match self {
            Self::NotFound => Some(404),
            Self::SyncStateNotFound => Some(410),
            Self::PreconditionFailed => Some(412),
            Self::LostResponse => None,
        }
    }
}

/// A fake Microsoft Graph calendar.
#[derive(Clone, Debug)]
pub struct FakeGraph {
    server: Server<Value>,
    next_id: u64,
    /// Each `transactionId` seen, to the event it created and the call that created it.
    transactions: BTreeMap<String, (String, u64)>,
    /// For how many calls a `transactionId` is remembered; `None` for ever.
    transaction_lifetime: Option<u64>,
}

fn change_key(seq: u64) -> String {
    format!("ck{seq}")
}

fn event(id: &str, resource: &Resource<Value>) -> Event {
    Event {
        id: id.to_owned(),
        change_key: change_key(resource.seq),
        body: resource.body.clone(),
    }
}

/// Whether `body` holds the single-value extended property `property` with `value`.
fn has_property(body: &Value, property: &str, value: &str) -> bool {
    body.get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .is_some_and(|properties| {
            properties.iter().any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(property)
                    && entry.get("value").and_then(Value::as_str) == Some(value)
            })
        })
}

/// The token in a link the fake issued, or the link itself, which then fails as expired.
fn token(link: &str) -> &str {
    link.rsplit_once("token=").map_or(link, |(_, token)| token)
}

impl FakeGraph {
    /// An empty calendar that injects faults by `plan`.
    #[must_use]
    pub fn new(plan: FaultPlan) -> Self {
        Self {
            server: Server::new(plan),
            next_id: 0,
            transactions: BTreeMap::new(),
            transaction_lifetime: None,
        }
    }

    /// Remember each `transactionId` for `calls` calls after the create that carried it.
    #[must_use]
    pub fn with_transaction_lifetime(mut self, calls: u64) -> Self {
        self.transaction_lifetime = Some(calls);
        self
    }

    /// `GET /me/calendarView/delta`, or a link it returned.
    ///
    /// # Errors
    ///
    /// [`Error::SyncStateNotFound`] when the link expired, and [`Error::LostResponse`].
    pub fn delta(&mut self, request: &DeltaRequest) -> Result<DeltaPage, Error> {
        let page = self
            .server
            .sync(request.link.as_deref().map(token), request.max_page_size)
            .map_err(|failure| match failure {
                SyncFailure::TokenExpired => Error::SyncStateNotFound,
                SyncFailure::LostResponse => Error::LostResponse,
            })?;
        Ok(DeltaPage {
            value: page
                .items
                .iter()
                .map(|(id, resource)| {
                    if resource.deleted {
                        DeltaItem::Removed { id: id.clone() }
                    } else {
                        DeltaItem::Changed(event(id, resource))
                    }
                })
                .collect(),
            next_link: page
                .next
                .map(|token| format!("{DELTA_URL}?$skiptoken={token}")),
            delta_link: page
                .sync
                .map(|token| format!("{DELTA_URL}?$deltatoken={token}")),
        })
    }

    /// `GET /me/events/{id}`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] and [`Error::LostResponse`].
    pub fn get(&mut self, id: &str) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Read);
        let result = self
            .server
            .live(id)
            .map(|resource| event(id, resource))
            .ok_or(Error::NotFound);
        self.lose(fault, result)
    }

    /// `POST /me/events`. The server assigns the id; a retry creates a second event.
    ///
    /// # Errors
    ///
    /// [`Error::LostResponse`], after which the event exists.
    pub fn create(&mut self, body: Value) -> Result<Event, Error> {
        self.create_with(body, None)
    }

    /// `POST /me/events` with an optional `transactionId`. A create carrying one the server
    /// still remembers, whose event still exists, returns that event and makes nothing.
    ///
    /// # Errors
    ///
    /// [`Error::LostResponse`], after which the event exists.
    pub fn create_with(
        &mut self,
        body: Value,
        transaction_id: Option<&str>,
    ) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Write);
        let call = self.server.calls();
        let remembered = transaction_id
            .and_then(|key| self.transactions.get(key))
            .filter(|(_, made)| {
                self.transaction_lifetime
                    .is_none_or(|lifetime| call <= made + lifetime)
            })
            .and_then(|(id, _)| self.server.live(id).map(|resource| event(id, resource)));
        let result = Ok(match remembered {
            Some(existing) => existing,
            None => {
                let id = self.mint_id();
                if let Some(key) = transaction_id {
                    self.transactions.insert(key.to_owned(), (id.clone(), call));
                }
                self.put(&id, body)
            }
        });
        self.lose(fault, result)
    }

    /// `GET /me/events?$filter=singleValueExtendedProperties/any(ep: ep/id eq '{property}' and
    /// ep/value eq '{value}')`: the live events whose body carries that extended property, in id
    /// order.
    ///
    /// # Errors
    ///
    /// [`Error::LostResponse`].
    pub fn find(&mut self, property: &str, value: &str) -> Result<Vec<Event>, Error> {
        let fault = self.server.begin(CallKind::Read);
        let found = self
            .server
            .live_resources()
            .filter(|(_, resource)| has_property(&resource.body, property, value))
            .map(|(id, resource)| event(id, resource))
            .collect();
        self.lose(fault, Ok(found))
    }

    /// `PATCH /me/events/{id}`, replacing the top-level properties `patch` names, conditional on
    /// `if_match` when given.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::PreconditionFailed`], and [`Error::LostResponse`].
    pub fn patch(
        &mut self,
        id: &str,
        if_match: Option<&str>,
        patch: Value,
    ) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Write);
        self.race(fault, id, if_match);
        let result = self.check(id, if_match).map(|mut body| {
            match (&mut body, patch) {
                (Value::Object(body), Value::Object(patch)) => body.extend(patch),
                (body, patch) => *body = patch,
            }
            self.put(id, body)
        });
        self.lose(fault, result)
    }

    /// `DELETE /me/events/{id}`, conditional on `if_match` when given.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], also when already deleted, [`Error::PreconditionFailed`], and
    /// [`Error::LostResponse`].
    pub fn delete(&mut self, id: &str, if_match: Option<&str>) -> Result<(), Error> {
        let fault = self.server.begin(CallKind::Write);
        self.race(fault, id, if_match);
        let result = self.check(id, if_match).map(|_| {
            self.server.remove(id);
        });
        self.lose(fault, result)
    }

    /// Another client creates an event, returning its id. Never faulted.
    pub fn remote_create(&mut self, body: Value) -> String {
        let id = self.mint_id();
        self.server.upsert(&id, body);
        id
    }

    /// Another client replaces an event's body, if it exists. Never faulted.
    pub fn remote_update(&mut self, id: &str, body: Value) {
        if self.server.live(id).is_some() {
            self.server.upsert(id, body);
        }
    }

    /// Another client deletes an event, if it exists. Never faulted.
    pub fn remote_delete(&mut self, id: &str) {
        self.server.remove(id);
    }

    /// Invalidate every link issued so far.
    pub fn expire_tokens(&mut self) {
        self.server.expire_tokens();
    }

    /// The events that exist now, in id order, for assertions. Never faulted.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.server
            .live_resources()
            .map(|(id, resource)| event(id, resource))
            .collect()
    }

    /// The faults injected so far.
    #[must_use]
    pub fn faults(&self) -> &[InjectedFault] {
        self.server.faults()
    }

    /// The number of calls served so far.
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.server.calls()
    }

    fn mint_id(&mut self) -> String {
        self.next_id += 1;
        format!("AAMkAG{:08}", self.next_id)
    }

    fn put(&mut self, id: &str, body: Value) -> Event {
        let seq = self.server.upsert(id, body.clone());
        Event {
            id: id.to_owned(),
            change_key: change_key(seq),
            body,
        }
    }

    /// Under [`Fault::PreconditionFailed`], another client changes the event first.
    fn race(&mut self, fault: Option<Fault>, id: &str, if_match: Option<&str>) {
        if fault == Some(Fault::PreconditionFailed)
            && if_match.is_some()
            && self.server.touch(id).is_some()
        {
            self.server.record(Fault::PreconditionFailed);
        }
    }

    /// The event's current body, if `if_match` allows the write.
    fn check(&self, id: &str, if_match: Option<&str>) -> Result<Value, Error> {
        let resource = self.server.live(id).ok_or(Error::NotFound)?;
        match if_match {
            Some(key) if key != change_key(resource.seq) => Err(Error::PreconditionFailed),
            _ => Ok(resource.body.clone()),
        }
    }

    fn lose<T>(&mut self, fault: Option<Fault>, result: Result<T, Error>) -> Result<T, Error> {
        if fault == Some(Fault::LostResponse) {
            self.server.record(Fault::LostResponse);
            return Err(Error::LostResponse);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DeltaItem, DeltaRequest, Error, FakeGraph};
    use crate::fault::{CallKind, Fault, FaultPlan};

    #[test]
    fn rounds_end_with_a_delta_link_and_report_removals() {
        let mut graph = FakeGraph::new(FaultPlan::none());
        let a = graph.remote_create(json!({ "subject": "a" }));
        let b = graph.remote_create(json!({ "subject": "b" }));
        let first = graph
            .delta(&DeltaRequest {
                link: None,
                max_page_size: Some(1),
            })
            .unwrap();
        assert!(first.next_link.is_some() && first.delta_link.is_none());
        let last = graph
            .delta(&DeltaRequest {
                link: first.next_link,
                max_page_size: Some(1),
            })
            .unwrap();
        assert!(last.delta_link.is_some());
        graph.remote_delete(&a);
        graph.remote_update(&b, json!({ "subject": "b2" }));
        let next = graph
            .delta(&DeltaRequest {
                link: last.delta_link,
                max_page_size: None,
            })
            .unwrap();
        assert_eq!(next.value.len(), 2);
        assert_eq!(next.value[0], DeltaItem::Removed { id: a });
        assert!(
            matches!(&next.value[1], DeltaItem::Changed(event) if event.body["subject"] == "b2")
        );
    }

    #[test]
    fn replay_redelivers_a_change_already_seen() {
        let plan = FaultPlan::none().at_call(1, Fault::Replay);
        let mut graph = FakeGraph::new(plan);
        let a = graph.remote_create(json!({}));
        let round = graph.delta(&DeltaRequest::default()).unwrap();
        let replayed = graph
            .delta(&DeltaRequest {
                link: round.delta_link,
                max_page_size: None,
            })
            .unwrap();
        assert!(matches!(&replayed.value[..], [DeltaItem::Changed(event)] if event.id == a));
        assert_eq!(graph.faults()[0].fault, Fault::Replay);
        assert_eq!(graph.faults()[0].kind, CallKind::Sync);
    }

    #[test]
    fn expired_link_answers_410() {
        let plan = FaultPlan::none().at_call(1, Fault::TokenExpired);
        let mut graph = FakeGraph::new(plan);
        let round = graph.delta(&DeltaRequest::default()).unwrap();
        let error = graph
            .delta(&DeltaRequest {
                link: round.delta_link,
                max_page_size: None,
            })
            .unwrap_err();
        assert_eq!(error, Error::SyncStateNotFound);
        assert_eq!(error.status(), Some(410));
        assert_eq!(
            graph
                .delta(&DeltaRequest {
                    link: Some("not a link".to_owned()),
                    max_page_size: None,
                })
                .unwrap_err(),
            Error::SyncStateNotFound
        );
    }

    #[test]
    fn patch_merges_and_honours_if_match() {
        let mut graph = FakeGraph::new(FaultPlan::none());
        let created = graph
            .create(json!({ "subject": "a", "body": "x" }))
            .unwrap();
        let patched = graph
            .patch(
                &created.id,
                Some(&created.change_key),
                json!({ "subject": "b" }),
            )
            .unwrap();
        assert_eq!(patched.body, json!({ "subject": "b", "body": "x" }));
        assert_eq!(
            graph
                .patch(&created.id, Some(&created.change_key), json!({}))
                .unwrap_err(),
            Error::PreconditionFailed
        );
        graph.delete(&created.id, None).unwrap();
        assert_eq!(
            graph.delete(&created.id, None).unwrap_err(),
            Error::NotFound
        );
    }

    #[test]
    fn injected_412_comes_from_a_concurrent_edit() {
        let plan = FaultPlan::none().at_call(1, Fault::PreconditionFailed);
        let mut graph = FakeGraph::new(plan);
        let created = graph.create(json!({ "subject": "a" })).unwrap();
        let error = graph
            .patch(
                &created.id,
                Some(&created.change_key),
                json!({ "subject": "b" }),
            )
            .unwrap_err();
        assert_eq!(error.status(), Some(412));
        let current = graph.get(&created.id).unwrap();
        assert_ne!(current.change_key, created.change_key);
        assert_eq!(current.body, json!({ "subject": "a" }));
        assert_eq!(graph.faults()[0].fault, Fault::PreconditionFailed);
        assert!(
            graph
                .patch(
                    &created.id,
                    Some(&current.change_key),
                    json!({ "subject": "b" })
                )
                .is_ok()
        );
    }

    #[test]
    fn partial_page_continues_from_its_next_link() {
        let plan = FaultPlan::none().at_call(0, Fault::PartialPage);
        let mut graph = FakeGraph::new(plan);
        let created: Vec<String> = (0..4).map(|_| graph.remote_create(json!({}))).collect();
        let first = graph.delta(&DeltaRequest::default()).unwrap();
        assert!(first.value.len() < 4);
        assert!(first.next_link.is_some() && first.delta_link.is_none());
        assert_eq!(graph.faults()[0].fault, Fault::PartialPage);
        let mut seen = first.value;
        let mut link = first.next_link;
        while let Some(next) = link {
            let page = graph
                .delta(&DeltaRequest {
                    link: Some(next),
                    max_page_size: None,
                })
                .unwrap();
            seen.extend(page.value);
            link = page.next_link;
        }
        let ids: Vec<String> = seen
            .into_iter()
            .map(|item| match item {
                DeltaItem::Changed(event) => event.id,
                DeltaItem::Removed { id } => id,
            })
            .collect();
        assert_eq!(ids, created);
    }

    #[test]
    fn lost_create_duplicates_on_retry() {
        let plan = FaultPlan::none().at_call(0, Fault::LostResponse);
        let mut graph = FakeGraph::new(plan);
        assert_eq!(graph.create(json!({})).unwrap_err(), Error::LostResponse);
        graph.create(json!({})).unwrap();
        assert_eq!(graph.events().len(), 2);
    }
}
