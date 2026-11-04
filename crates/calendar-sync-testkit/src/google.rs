//! A fake Google Calendar, one calendar's `events` collection.
//!
//! It follows the [incremental sync guide](https://developers.google.com/workspace/calendar/api/guides/sync)
//! and `events.list` in `references/google/calendar-v3-discovery.json`: pages carry a
//! `nextPageToken`, and only the last page carries a `nextSyncToken`; an incremental list
//! includes cancelled events; an expired sync token answers `410 Gone`. Event ids are chosen by
//! the client, and writes take `If-Match` on the event's `etag`. Bodies are opaque JSON.

use serde_json::Value;

use crate::fault::{CallKind, Fault, FaultPlan, InjectedFault};
use crate::server::{Resource, Server, SyncFailure};

/// An event's status as `events.list` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventStatus {
    /// The event exists (`confirmed`; the fake does not model `tentative`).
    Confirmed,
    /// The event was deleted (`cancelled`).
    Cancelled,
}

/// An event as the fake returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The event id.
    pub id: String,
    /// The `etag`, a quoted string unique to this version of the event.
    pub etag: String,
    /// Whether the event exists or was deleted.
    pub status: EventStatus,
    /// The event's body. A cancelled event carries none.
    pub body: Option<Value>,
}

/// An `events.list` request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListRequest {
    /// The `nextSyncToken` of an earlier pass, for an incremental list.
    pub sync_token: Option<String>,
    /// The `nextPageToken` of the previous page. It carries the pass's sync token itself.
    pub page_token: Option<String>,
    /// `maxResults`; `None` returns the whole pass on one page.
    pub max_results: Option<usize>,
}

/// One page of `events.list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsPage {
    /// The events on this page. A cancelled event appears only in an incremental list.
    pub items: Vec<Event>,
    /// The token for the next page, when there is one.
    pub next_page_token: Option<String>,
    /// The token for the next incremental list, on the last page only.
    pub next_sync_token: Option<String>,
}

/// A failed call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// `404 Not Found`: no event has this id.
    NotFound,
    /// `409 Conflict`: an event with this id exists or existed.
    Conflict,
    /// `410 Gone`: the sync or page token expired, or the event was deleted. After an expired
    /// token the client clears its store and lists again without one.
    Gone,
    /// `412 Precondition Failed`: `If-Match` did not match the event's current `etag`.
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
            Self::Conflict => Some(409),
            Self::Gone => Some(410),
            Self::PreconditionFailed => Some(412),
            Self::LostResponse => None,
        }
    }
}

/// A fake Google Calendar with one calendar.
#[derive(Clone, Debug)]
pub struct FakeGoogle {
    server: Server<Value>,
}

fn etag(seq: u64) -> String {
    format!("\"{seq}\"")
}

fn event(id: &str, resource: &Resource<Value>) -> Event {
    Event {
        id: id.to_owned(),
        etag: etag(resource.seq),
        status: if resource.deleted {
            EventStatus::Cancelled
        } else {
            EventStatus::Confirmed
        },
        body: (!resource.deleted).then(|| resource.body.clone()),
    }
}

impl FakeGoogle {
    /// An empty calendar that injects faults by `plan`.
    #[must_use]
    pub fn new(plan: FaultPlan) -> Self {
        Self {
            server: Server::new(plan),
        }
    }

    /// `events.list`.
    ///
    /// # Errors
    ///
    /// [`Error::Gone`] when the token expired, and [`Error::LostResponse`].
    pub fn list(&mut self, request: &ListRequest) -> Result<EventsPage, Error> {
        let token = request
            .page_token
            .as_deref()
            .or(request.sync_token.as_deref());
        let page =
            self.server
                .sync(token, request.max_results)
                .map_err(|failure| match failure {
                    SyncFailure::TokenExpired => Error::Gone,
                    SyncFailure::LostResponse => Error::LostResponse,
                })?;
        Ok(EventsPage {
            items: page
                .items
                .iter()
                .map(|(id, resource)| event(id, resource))
                .collect(),
            next_page_token: page.next,
            next_sync_token: page.sync,
        })
    }

    /// `events.get`. A deleted event is returned with status `cancelled`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] and [`Error::LostResponse`].
    pub fn get(&mut self, id: &str) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Read);
        let result = self
            .server
            .get(id)
            .map(|resource| event(id, resource))
            .ok_or(Error::NotFound);
        self.lose(fault, result)
    }

    /// `events.insert` with a client-chosen id.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when the id is or was in use, and [`Error::LostResponse`].
    pub fn insert(&mut self, id: &str, body: Value) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Write);
        let result = if self.server.get(id).is_some() {
            Err(Error::Conflict)
        } else {
            Ok(self.put(id, body))
        };
        self.lose(fault, result)
    }

    /// `events.update`, replacing the body, conditional on `if_match` when given.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::Gone`] for a deleted event,
    /// [`Error::PreconditionFailed`], and [`Error::LostResponse`].
    pub fn update(
        &mut self,
        id: &str,
        if_match: Option<&str>,
        body: Value,
    ) -> Result<Event, Error> {
        let fault = self.server.begin(CallKind::Write);
        self.race(fault, id, if_match);
        let result = self.check(id, if_match).map(|()| self.put(id, body));
        self.lose(fault, result)
    }

    /// `events.delete`, conditional on `if_match` when given.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::Gone`] when already deleted,
    /// [`Error::PreconditionFailed`], and [`Error::LostResponse`].
    pub fn delete(&mut self, id: &str, if_match: Option<&str>) -> Result<(), Error> {
        let fault = self.server.begin(CallKind::Write);
        self.race(fault, id, if_match);
        let result = self.check(id, if_match).map(|()| {
            self.server.remove(id);
        });
        self.lose(fault, result)
    }

    /// Another client creates or replaces an event. Never faulted.
    pub fn remote_put(&mut self, id: &str, body: Value) {
        self.server.upsert(id, body);
    }

    /// Another client deletes an event, if it exists. Never faulted.
    pub fn remote_delete(&mut self, id: &str) {
        self.server.remove(id);
    }

    /// Invalidate every sync and page token issued so far, as a server does when it drops its
    /// change history.
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

    fn put(&mut self, id: &str, body: Value) -> Event {
        let seq = self.server.upsert(id, body.clone());
        Event {
            id: id.to_owned(),
            etag: etag(seq),
            status: EventStatus::Confirmed,
            body: Some(body),
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

    fn check(&self, id: &str, if_match: Option<&str>) -> Result<(), Error> {
        let resource = self.server.get(id).ok_or(Error::NotFound)?;
        if resource.deleted {
            return Err(Error::Gone);
        }
        match if_match {
            Some(tag) if tag != etag(resource.seq) => Err(Error::PreconditionFailed),
            _ => Ok(()),
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

    use super::{Error, EventStatus, FakeGoogle, ListRequest};
    use crate::fault::{Fault, FaultPlan};

    #[test]
    fn next_sync_token_only_on_the_last_page() {
        let mut google = FakeGoogle::new(FaultPlan::none());
        for id in ["a", "b", "c"] {
            google.remote_put(id, json!({ "summary": id }));
        }
        let request = ListRequest {
            max_results: Some(2),
            ..ListRequest::default()
        };
        let first = google.list(&request).unwrap();
        assert_eq!(first.items.len(), 2);
        assert!(first.next_page_token.is_some() && first.next_sync_token.is_none());
        let last = google
            .list(&ListRequest {
                page_token: first.next_page_token,
                ..request
            })
            .unwrap();
        assert_eq!(last.items.len(), 1);
        assert!(last.next_page_token.is_none() && last.next_sync_token.is_some());
    }

    #[test]
    fn incremental_list_reports_cancelled_events() {
        let mut google = FakeGoogle::new(FaultPlan::none());
        google.remote_put("a", json!({}));
        let token = google
            .list(&ListRequest::default())
            .unwrap()
            .next_sync_token;
        google.remote_delete("a");
        let page = google
            .list(&ListRequest {
                sync_token: token,
                ..ListRequest::default()
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, EventStatus::Cancelled);
        assert_eq!(page.items[0].body, None);
    }

    #[test]
    fn expired_token_answers_410() {
        let mut google = FakeGoogle::new(FaultPlan::none());
        let token = google
            .list(&ListRequest::default())
            .unwrap()
            .next_sync_token;
        google.expire_tokens();
        let error = google
            .list(&ListRequest {
                sync_token: token,
                ..ListRequest::default()
            })
            .unwrap_err();
        assert_eq!(error, Error::Gone);
        assert_eq!(error.status(), Some(410));
    }

    #[test]
    fn injected_412_comes_from_a_concurrent_edit() {
        let plan = FaultPlan::none().at_call(1, Fault::PreconditionFailed);
        let mut google = FakeGoogle::new(plan);
        let created = google.insert("a", json!({ "summary": "x" })).unwrap();
        let error = google
            .update("a", Some(&created.etag), json!({ "summary": "y" }))
            .unwrap_err();
        assert_eq!(error.status(), Some(412));
        let current = google.get("a").unwrap();
        assert_ne!(current.etag, created.etag);
        assert_eq!(current.body, Some(json!({ "summary": "x" })));
        assert!(
            google
                .update("a", Some(&current.etag), json!({ "summary": "y" }))
                .is_ok()
        );
    }

    #[test]
    fn lost_response_still_applies_the_write() {
        let plan = FaultPlan::none().at_call(0, Fault::LostResponse);
        let mut google = FakeGoogle::new(plan);
        assert_eq!(
            google.insert("a", json!({})).unwrap_err(),
            Error::LostResponse
        );
        assert_eq!(google.insert("a", json!({})).unwrap_err(), Error::Conflict);
        assert_eq!(google.events().len(), 1);
        assert_eq!(google.faults().len(), 1);
    }

    #[test]
    fn deleted_event_is_gone() {
        let mut google = FakeGoogle::new(FaultPlan::none());
        let created = google.insert("a", json!({})).unwrap();
        google.delete("a", Some(&created.etag)).unwrap();
        assert_eq!(google.delete("a", None).unwrap_err(), Error::Gone);
        assert_eq!(google.get("a").unwrap().status, EventStatus::Cancelled);
        assert_eq!(google.delete("b", None).unwrap_err(), Error::NotFound);
    }
}
