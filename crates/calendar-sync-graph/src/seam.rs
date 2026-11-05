//! The engine's [`ProviderSeam`] for Microsoft Graph, the [`Exchange`] that performs one request,
//! and a blocking client over an injected [`Transport`].

use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::{ProviderEvent, ProviderRevision};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

use crate::event::{self, Item, ReadEvent};
use crate::http::{BASE_URL, HttpRequest, HttpResponse, Method, Transport, TransportError, encode};

/// The single-value extended property a create stores its idempotency key in, so a later
/// [`Request::Lookup`] can find the event: a named property in the `PS_PUBLIC_STRINGS` set.
pub const IDEMPOTENCY_PROPERTY: &str =
    "String {00020329-0000-0000-C000-000000000046} Name calendarSyncIdempotencyKey";

/// The preferences every request sends: immutable ids, so an id survives a move between
/// folders; times in UTC, which the mapping converts into each event's own zone; and plain-text
/// bodies, which is what the model's description holds.
const PREFER: &str =
    "IdType=\"ImmutableId\", outlook.timezone=\"UTC\", outlook.body-content-type=\"text\"";

/// The error codes that mean a delta link can no longer be followed.
const SYNC_RESET: [&str; 3] = ["syncStateNotFound", "syncStateInvalid", "resyncRequired"];

/// The span of time a delta round covers. Graph's `calendarView/delta` requires one, and keeps
/// it in every link it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// The start, inclusive.
    pub start: Timestamp,
    /// The end, exclusive.
    pub end: Timestamp,
}

/// How an update or a delete makes sure it acts on the revision it was planned from.
///
/// Whether Graph honours `If-Match` on an event is unverified, so the default reads the event and
/// compares its `changeKey` itself. A race remains between that read and the write, limited to
/// the properties the write patches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Precondition {
    /// Read the event, compare its `changeKey`, then send a minimal `PATCH` (or the `DELETE`)
    /// without `If-Match`. The default.
    #[default]
    ReadCompare,
    /// As [`Precondition::ReadCompare`], and send `If-Match` with the `changeKey` as well.
    ReadCompareIfMatch,
    /// Send `If-Match` alone, without a read: one request, but the `PATCH` carries every property
    /// this mapping writes, since there is no read to compare it with.
    IfMatch,
}

/// Translates the engine's requests into Microsoft Graph calls on one calendar, and the
/// responses back.
///
/// | Request | Calls |
/// |---|---|
/// | `List` from the start | `GET calendarView/delta?startDateTime=S&endDateTime=E` |
/// | `List` since or continuing | `GET` the `@odata.deltaLink` or `@odata.nextLink` |
/// | `Read` | `GET events/ID` |
/// | `Lookup` | `GET events?$filter=` on [`IDEMPOTENCY_PROPERTY`] |
/// | `Create` | `POST events` with `transactionId` and [`IDEMPOTENCY_PROPERTY`] set to the key |
/// | `Update` | `GET events/ID`, compare `changeKey`, then `PATCH` the properties that differ; see [`Precondition`] |
/// | `Delete` | `GET events/ID`, compare `changeKey`, then `DELETE`; see [`Precondition`] |
///
/// Every request sends `Prefer: IdType="ImmutableId"`, so the ids the engine stores survive a
/// move. A delta row marked `@removed`, or one that carries too little to map (no `changeKey`,
/// `start` or `end`), is read before the page is returned: a `404` confirms the removal, and an
/// event that still exists (one that left the window, or a sparse row) is reported as it is now.
///
/// | Response | Outcome |
/// |---|---|
/// | `410`, or an error `syncStateNotFound`, `syncStateInvalid` or `resyncRequired`, to a list | [`Failure::CursorExpired`] |
/// | `404` or `410` to a read | `Read(None)` |
/// | an empty collection to a lookup | `Read(None)`, and the first event otherwise |
/// | `404` or `410` to an update or a delete, or its read | [`Failure::NotFound`] |
/// | a `changeKey` other than the planned one, `412`, or `409` to an update or a delete | [`Failure::PreconditionFailed`] |
/// | `409` to a create | [`Failure::AlreadyExists`] |
/// | `401`, `408`, `429`, `5xx`, no response, an unreadable `2xx` | [`Failure::Transient`] |
/// | any other status | [`Failure::Rejected`] with Graph's code and message |
/// | a create or update whose recurrence Graph cannot hold | [`Failure::Rejected`], and nothing is sent |
///
/// A `401` is transient because the transport owns credentials: it refreshes them and the engine
/// tries again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphSeam {
    mailbox: String,
    calendar_id: Option<String>,
    window: Window,
    page_size: Option<u32>,
    precondition: Precondition,
}

impl GraphSeam {
    /// The seam for the signed-in user's default calendar, syncing the events in `window`.
    #[must_use]
    pub fn new(window: Window) -> Self {
        Self {
            mailbox: "/v1.0/me".to_owned(),
            calendar_id: None,
            window,
            page_size: None,
            precondition: Precondition::default(),
        }
    }

    /// Syncs the calendar `calendar_id` instead of the default one.
    #[must_use]
    pub fn with_calendar(mut self, calendar_id: impl Into<String>) -> Self {
        self.calendar_id = Some(calendar_id.into());
        self
    }

    /// Syncs a calendar of the user `user` (an id or a user principal name) instead of the
    /// signed-in user's.
    #[must_use]
    pub fn with_user(mut self, user: &str) -> Self {
        self.mailbox = format!("/v1.0/users/{}", encode(user));
        self
    }

    /// Asks for at most `page_size` events per delta page (`Prefer: odata.maxpagesize`).
    #[must_use]
    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// How updates and deletes check their precondition. [`Precondition::ReadCompare`] by
    /// default.
    #[must_use]
    pub fn with_precondition(mut self, precondition: Precondition) -> Self {
        self.precondition = precondition;
        self
    }

    /// The window delta rounds cover.
    #[must_use]
    pub fn window(&self) -> Window {
        self.window
    }

    /// How updates and deletes check their precondition.
    #[must_use]
    pub fn precondition(&self) -> Precondition {
        self.precondition
    }

    /// The exchange that performs `request`.
    #[must_use]
    pub fn exchange(&self, request: &Request) -> Exchange {
        let state = match request {
            Request::List { from } => State::Await(Pending {
                http: self.list(from),
                step: Step::List,
            }),
            Request::Read { resource } => State::Await(Pending {
                http: self.get(resource),
                step: Step::Final,
            }),
            Request::Lookup { key } => State::Await(Pending {
                http: self.lookup(key),
                step: Step::Final,
            }),
            Request::Create { resource, event } => {
                let written = event::write(event);
                if written.refuses_recurrence() {
                    State::Done(refused(&written))
                } else {
                    State::Await(Pending {
                        http: self.create(resource, written.body),
                        step: Step::Final,
                    })
                }
            }
            Request::Update {
                resource,
                if_match,
                event,
            } => match self.precondition {
                Precondition::IfMatch => {
                    let written = event::write(event);
                    if written.refuses_recurrence() {
                        State::Done(refused(&written))
                    } else {
                        State::Await(Pending {
                            http: self.patch(resource, if_match.as_ref(), written.body),
                            step: Step::Final,
                        })
                    }
                }
                _ => State::Await(Pending {
                    http: self.get(resource),
                    step: Step::Check,
                }),
            },
            Request::Delete { resource, if_match } => match self.precondition {
                Precondition::IfMatch => State::Await(Pending {
                    http: self.delete(resource, if_match.as_ref()),
                    step: Step::Final,
                }),
                _ => State::Await(Pending {
                    http: self.get(resource),
                    step: Step::Check,
                }),
            },
        };
        Exchange {
            seam: self.clone(),
            request: request.clone(),
            state,
        }
    }

    fn calendar(&self) -> String {
        match &self.calendar_id {
            Some(id) => format!("{}/calendars/{}", self.mailbox, encode(id)),
            None => self.mailbox.clone(),
        }
    }

    fn event_path(&self, id: &ResourceId) -> String {
        format!("{}/events/{}", self.mailbox, encode(id.as_str()))
    }

    fn headers(&self, list: bool) -> Vec<(String, String)> {
        let prefer = match self.page_size {
            Some(size) if list => format!("{PREFER}, odata.maxpagesize={size}"),
            _ => PREFER.to_owned(),
        };
        vec![("Prefer".to_owned(), prefer)]
    }

    fn list(&self, from: &ListFrom) -> HttpRequest {
        let path = match from {
            ListFrom::Start => format!(
                "{}/calendarView/delta?startDateTime={}&endDateTime={}",
                self.calendar(),
                encode(&self.window.start.to_string()),
                encode(&self.window.end.to_string())
            ),
            ListFrom::Since(link) | ListFrom::Continue(link) => link
                .as_str()
                .strip_prefix(BASE_URL)
                .unwrap_or(link.as_str())
                .to_owned(),
        };
        HttpRequest {
            method: Method::Get,
            path,
            headers: self.headers(true),
            body: None,
        }
    }

    fn get(&self, id: &ResourceId) -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            path: self.event_path(id),
            headers: self.headers(false),
            body: None,
        }
    }

    fn lookup(&self, key: &ResourceId) -> HttpRequest {
        let filter = format!(
            "singleValueExtendedProperties/any(ep: ep/id eq '{IDEMPOTENCY_PROPERTY}' and ep/value eq '{}')",
            key.as_str().replace('\'', "''")
        );
        HttpRequest {
            method: Method::Get,
            path: format!("{}/events?$filter={}", self.calendar(), encode(&filter)),
            headers: self.headers(false),
            body: None,
        }
    }

    fn json_headers(&self, if_match: Option<&ProviderRevision>) -> Vec<(String, String)> {
        let mut headers = self.headers(false);
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        if let Some(revision) = if_match {
            headers.push(("If-Match".to_owned(), revision.as_str().to_owned()));
        }
        headers
    }

    fn create(&self, key: &ResourceId, mut body: Map<String, Value>) -> HttpRequest {
        body.insert("transactionId".to_owned(), json!(key.as_str()));
        body.insert(
            "singleValueExtendedProperties".to_owned(),
            json!([{ "id": IDEMPOTENCY_PROPERTY, "value": key.as_str() }]),
        );
        HttpRequest {
            method: Method::Post,
            path: format!("{}/events", self.calendar()),
            headers: self.json_headers(None),
            body: Some(Value::Object(body).to_string()),
        }
    }

    fn patch(
        &self,
        id: &ResourceId,
        if_match: Option<&ProviderRevision>,
        body: Map<String, Value>,
    ) -> HttpRequest {
        HttpRequest {
            method: Method::Patch,
            path: self.event_path(id),
            headers: self.json_headers(if_match),
            body: Some(Value::Object(body).to_string()),
        }
    }

    fn delete(&self, id: &ResourceId, if_match: Option<&ProviderRevision>) -> HttpRequest {
        let mut headers = self.headers(false);
        if let Some(revision) = if_match {
            headers.push(("If-Match".to_owned(), revision.as_str().to_owned()));
        }
        HttpRequest {
            method: Method::Delete,
            path: self.event_path(id),
            headers,
            body: None,
        }
    }
}

fn refused(written: &event::WrittenEvent) -> Outcome {
    let reasons: Vec<String> = written
        .losses
        .iter()
        .filter(|loss| loss.field == calendar_sync_model::Field::Recurrence)
        .map(|loss| loss.detail.clone())
        .collect();
    Outcome::Failed(Failure::Rejected(format!(
        "Graph cannot hold this recurrence: {}",
        reasons.join("; ")
    )))
}

impl ProviderSeam for GraphSeam {
    type Call = Exchange;
    type Response = Exchange;

    fn call(&self, request: &Request) -> Exchange {
        self.exchange(request)
    }

    /// The outcome of `response`, the exchange [`GraphSeam::call`] returned, driven to its end.
    /// An exchange abandoned part-way may have taken effect, so it is
    /// [`Failure::Transient`].
    fn outcome(&self, _request: &Request, response: Exchange) -> Outcome {
        response.into_outcome()
    }
}

/// One engine request performed as one or more HTTP requests.
///
/// Send [`Exchange::request`] and hand its response to [`Exchange::receive`] until there is no
/// request left; then [`Exchange::into_outcome`] (or [`ProviderSeam::outcome`]) is the outcome.
/// A read before a conditional write, and the read that confirms a delta removal, are why one
/// request can take more than one call. Each exchange makes at most two calls, except a delta
/// page, which makes one more per row it confirms.
///
/// ```
/// use calendar_sync_engine::{Outcome, ProviderSeam, Request, ResourceId};
/// use calendar_sync_graph::{GraphSeam, HttpResponse, Window};
///
/// let seam = GraphSeam::new(Window {
///     start: "2026-11-01T00:00:00Z".parse().unwrap(),
///     end: "2026-12-01T00:00:00Z".parse().unwrap(),
/// });
/// let request = Request::Read { resource: ResourceId::new("AAMkAGI2") };
/// let mut exchange = seam.call(&request);
/// let call = exchange.request().expect("one call").clone();
/// assert_eq!(call.path, "/v1.0/me/events/AAMkAGI2");
/// exchange.receive(Ok(HttpResponse::new(404, "")));
/// assert!(exchange.request().is_none());
/// assert_eq!(seam.outcome(&request, exchange), Outcome::Read(None));
/// ```
#[derive(Clone, Debug)]
pub struct Exchange {
    seam: GraphSeam,
    request: Request,
    state: State,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one exchange per request, moved into its outcome once"
)]
enum State {
    Await(Pending),
    Done(Outcome),
}

#[derive(Clone, Debug)]
struct Pending {
    http: HttpRequest,
    step: Step,
}

#[derive(Clone, Debug)]
enum Step {
    /// The response is the request's outcome.
    Final,
    /// A delta page.
    List,
    /// The read before a conditional update or delete.
    Check,
    /// The read that confirms row `index` of a delta page.
    Confirm {
        rows: Vec<Row>,
        next: Next,
        index: usize,
    },
}

#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "a row is moved once into its page"
)]
enum Row {
    Ready(Change),
    Confirm(ResourceId),
}

impl Exchange {
    /// The HTTP request to send next, or `None` once the outcome is known.
    #[must_use]
    pub fn request(&self) -> Option<&HttpRequest> {
        match &self.state {
            State::Await(pending) => Some(&pending.http),
            State::Done(_) => None,
        }
    }

    /// The outcome, once there is no request left to send.
    #[must_use]
    pub fn outcome(&self) -> Option<&Outcome> {
        match &self.state {
            State::Await(_) => None,
            State::Done(outcome) => Some(outcome),
        }
    }

    /// The outcome: [`Failure::Transient`] if a request is still to be sent, since what was sent
    /// may have taken effect.
    #[must_use]
    pub fn into_outcome(self) -> Outcome {
        match self.state {
            State::Await(_) => Outcome::Failed(Failure::Transient),
            State::Done(outcome) => outcome,
        }
    }

    /// Hands over what sending [`Exchange::request`] produced. Ignored once the outcome is known.
    pub fn receive(&mut self, response: Result<HttpResponse, TransportError>) {
        let State::Await(pending) = &mut self.state else {
            return;
        };
        let step = core::mem::replace(&mut pending.step, Step::Final);
        self.state = match step {
            Step::Final => State::Done(self.finish(response)),
            Step::List => self.page(response),
            Step::Check => self.check(response),
            Step::Confirm { rows, next, index } => self.confirm(response, rows, next, index),
        };
    }

    /// The outcome of the request's own call.
    fn finish(&self, response: Result<HttpResponse, TransportError>) -> Outcome {
        let response = match success(response) {
            Ok(response) => response,
            Err(failed) => return self.failure(&failed),
        };
        if matches!(self.request, Request::Delete { .. }) {
            return Outcome::Deleted;
        }
        // A 2xx whose body cannot be read may still have taken effect: the engine reads again.
        let Ok(body) = serde_json::from_slice::<Value>(&response.body) else {
            return Outcome::Failed(Failure::Transient);
        };
        match &self.request {
            Request::Read { .. } => match event::read(&body) {
                Ok(read) => Outcome::Read(match read.item {
                    Item::Event(event) => Some(event),
                    Item::Removed { .. } => None,
                }),
                Err(error) => Outcome::Failed(Failure::Rejected(error.to_string())),
            },
            Request::Lookup { .. } => {
                let found = body
                    .get("value")
                    .and_then(Value::as_array)
                    .and_then(|events| events.first());
                match found.map(event::read) {
                    Some(Ok(ReadEvent {
                        item: Item::Event(event),
                        ..
                    })) => Outcome::Read(Some(event)),
                    Some(Err(error)) => Outcome::Failed(Failure::Rejected(error.to_string())),
                    None | Some(Ok(_)) => Outcome::Read(None),
                }
            }
            _ => match event::read(&body) {
                Ok(ReadEvent {
                    item: Item::Event(event),
                    ..
                }) => Outcome::Written(event),
                _ => Outcome::Failed(Failure::Transient),
            },
        }
    }

    /// What a failed response means for this request.
    fn failure(&self, failed: &Failed) -> Outcome {
        let Failed::Status(response) = failed else {
            return Outcome::Failed(Failure::Transient);
        };
        let error = ErrorBody::of(response);
        let failure = match (response.status, &self.request) {
            (_, Request::List { .. })
                if response.status == 410
                    || error
                        .code
                        .as_deref()
                        .is_some_and(|code| SYNC_RESET.contains(&code)) =>
            {
                Failure::CursorExpired
            }
            (404 | 410, Request::Read { .. }) => return Outcome::Read(None),
            (404 | 410, Request::Update { .. } | Request::Delete { .. }) => Failure::NotFound,
            (409, Request::Create { .. }) => Failure::AlreadyExists,
            (409, Request::Update { .. } | Request::Delete { .. }) | (412, _) => {
                Failure::PreconditionFailed
            }
            (401 | 408 | 429 | 500..=599, _) => Failure::Transient,
            (status, _) => Failure::Rejected(error.describe(status)),
        };
        Outcome::Failed(failure)
    }

    /// Reads a delta page, and starts confirming the rows that need a read.
    fn page(&self, response: Result<HttpResponse, TransportError>) -> State {
        let response = match success(response) {
            Ok(response) => response,
            Err(failed) => return State::Done(self.failure(&failed)),
        };
        let Some(body) = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|body| body.as_object().cloned())
        else {
            return State::Done(Outcome::Failed(Failure::Transient));
        };
        let mut rows = Vec::new();
        for row in body
            .get("value")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let complete = ["changeKey", "start", "end"]
                .iter()
                .all(|name| row.get(name).is_some_and(|value| !value.is_null()));
            match event::read(row) {
                Ok(read) => rows.push(match read.item {
                    Item::Event(event) if complete => match upsert(event) {
                        Ok(change) => Row::Ready(change),
                        Err(failure) => return State::Done(Outcome::Failed(failure)),
                    },
                    Item::Event(event) => Row::Confirm(ResourceId::of(&event)),
                    Item::Removed { id } => Row::Confirm(ResourceId::new(id)),
                }),
                // A sparse row with no `start` cannot be mapped, but it can be read.
                Err(_) if !complete => match row.get("id").and_then(Value::as_str) {
                    Some(id) => rows.push(Row::Confirm(ResourceId::new(id))),
                    None => {
                        return State::Done(Outcome::Failed(Failure::Rejected(
                            "a delta row has no id".to_owned(),
                        )));
                    }
                },
                Err(error) => {
                    return State::Done(Outcome::Failed(Failure::Rejected(error.to_string())));
                }
            }
        }
        let link = |name: &str| body.get(name).and_then(Value::as_str).map(Cursor::new);
        let next = if let Some(page) = link("@odata.nextLink") {
            Next::More(page)
        } else if let Some(delta) = link("@odata.deltaLink") {
            Next::Done(delta)
        } else {
            return State::Done(Outcome::Failed(Failure::Rejected(
                "a delta page carries neither @odata.nextLink nor @odata.deltaLink".to_owned(),
            )));
        };
        self.next_confirmation(rows, next, 0)
    }

    /// Asks for the next row that needs a read, or returns the page when none is left.
    fn next_confirmation(&self, rows: Vec<Row>, next: Next, from: usize) -> State {
        let found = rows[from..]
            .iter()
            .position(|row| matches!(row, Row::Confirm(_)))
            .map(|offset| from + offset);
        match found {
            Some(index) => {
                let Row::Confirm(resource) = &rows[index] else {
                    unreachable!("found a row to confirm")
                };
                State::Await(Pending {
                    http: self.seam.get(resource),
                    step: Step::Confirm { rows, next, index },
                })
            }
            None => State::Done(Outcome::Listed(Page {
                changes: rows
                    .into_iter()
                    .filter_map(|row| match row {
                        Row::Ready(change) => Some(change),
                        Row::Confirm(_) => None,
                    })
                    .collect(),
                next,
            })),
        }
    }

    /// Reads row `index` of a delta page: gone means removed, present means changed.
    fn confirm(
        &self,
        response: Result<HttpResponse, TransportError>,
        mut rows: Vec<Row>,
        next: Next,
        index: usize,
    ) -> State {
        let Row::Confirm(resource) = &rows[index] else {
            return State::Done(Outcome::Failed(Failure::Transient));
        };
        let change = match response {
            Ok(response) if matches!(response.status, 404 | 410) => Change::Remove {
                resource: resource.clone(),
            },
            other => match success(other) {
                Ok(response) => match serde_json::from_slice::<Value>(&response.body) {
                    Ok(body) => match event::read(&body) {
                        Ok(ReadEvent {
                            item: Item::Event(event),
                            ..
                        }) => match upsert(event) {
                            Ok(change) => change,
                            Err(failure) => return State::Done(Outcome::Failed(failure)),
                        },
                        Ok(_) => Change::Remove {
                            resource: resource.clone(),
                        },
                        Err(error) => {
                            return State::Done(Outcome::Failed(Failure::Rejected(
                                error.to_string(),
                            )));
                        }
                    },
                    Err(_) => return State::Done(Outcome::Failed(Failure::Transient)),
                },
                Err(Failed::Status(response))
                    if !matches!(response.status, 401 | 408 | 429 | 500..=599) =>
                {
                    return State::Done(Outcome::Failed(Failure::Rejected(
                        ErrorBody::of(&response).describe(response.status),
                    )));
                }
                Err(_) => return State::Done(Outcome::Failed(Failure::Transient)),
            },
        };
        rows[index] = Row::Ready(change);
        self.next_confirmation(rows, next, index + 1)
    }

    /// The read before a conditional update or delete: compare the `changeKey`, then write.
    fn check(&self, response: Result<HttpResponse, TransportError>) -> State {
        let response = match success(response) {
            Ok(response) => response,
            Err(failed) => return State::Done(self.failure(&failed)),
        };
        let current =
            match serde_json::from_slice::<Value>(&response.body).map(|body| event::read(&body)) {
                Ok(Ok(ReadEvent {
                    item: Item::Event(event),
                    ..
                })) => event,
                Ok(Ok(_)) => return State::Done(Outcome::Failed(Failure::NotFound)),
                Ok(Err(error)) => {
                    return State::Done(Outcome::Failed(Failure::Rejected(error.to_string())));
                }
                Err(_) => return State::Done(Outcome::Failed(Failure::Transient)),
            };
        let (Request::Update {
            resource, if_match, ..
        }
        | Request::Delete { resource, if_match }) = &self.request
        else {
            return State::Done(Outcome::Failed(Failure::Transient));
        };
        if let Some(planned) = if_match
            && current.revision.as_ref() != Some(planned)
        {
            return State::Done(Outcome::Failed(Failure::PreconditionFailed));
        }
        let header = match self.seam.precondition {
            Precondition::ReadCompareIfMatch => current.revision.as_ref(),
            Precondition::ReadCompare | Precondition::IfMatch => None,
        };
        let http = match &self.request {
            Request::Update { event, .. } => {
                let written = event::write(event);
                // A recurrence Graph cannot hold is written as `null`, so the comparison is of
                // the recurrences themselves: refuse only a change Graph would not keep.
                if written.refuses_recurrence()
                    && event.fields.recurrence != current.fields.recurrence
                {
                    return State::Done(refused(&written));
                }
                let patch = minimal(&written.body, &event::write(&current).body);
                if patch.is_empty() {
                    return State::Done(Outcome::Written(current));
                }
                self.seam.patch(resource, header, patch)
            }
            _ => self.seam.delete(resource, header),
        };
        State::Await(Pending {
            http,
            step: Step::Final,
        })
    }
}

/// The properties of `new` whose values differ from `current`'s: the minimal `PATCH`.
fn minimal(new: &Map<String, Value>, current: &Map<String, Value>) -> Map<String, Value> {
    new.iter()
        .filter(|(name, value)| current.get(*name) != Some(value))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn upsert(event: ProviderEvent) -> Result<Change, Failure> {
    let revision = event
        .revision
        .clone()
        .ok_or_else(|| Failure::Rejected(format!("event {} has no changeKey", event.ids.id)))?;
    Ok(Change::Upsert {
        resource: ResourceId::of(&event),
        revision,
        event: Some(event),
    })
}

/// A response that was not a success.
enum Failed {
    /// No response.
    Transport,
    /// A non-2xx response.
    Status(HttpResponse),
}

fn success(response: Result<HttpResponse, TransportError>) -> Result<HttpResponse, Failed> {
    match response {
        Ok(response) if (200..300).contains(&response.status) => Ok(response),
        Ok(response) => Err(Failed::Status(response)),
        Err(_) => Err(Failed::Transport),
    }
}

/// Graph's error body: `{"error": {"code", "message"}}`.
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

impl ErrorBody {
    fn of(response: &HttpResponse) -> Self {
        let error = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|body| body.get("error").cloned());
        let field = |name: &str| {
            error
                .as_ref()
                .and_then(|error| error.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Self {
            code: field("code"),
            message: field("message"),
        }
    }

    fn describe(&self, status: u16) -> String {
        match (&self.code, &self.message) {
            (Some(code), Some(message)) => format!("{status} {code}: {message}"),
            (Some(code), None) => format!("{status} {code}"),
            (None, Some(message)) => format!("{status}: {message}"),
            (None, None) => format!("{status}"),
        }
    }
}

/// A blocking client: the seam over an injected [`Transport`].
///
/// An async application drives each [`Exchange`] itself, sending its requests with its own
/// client.
#[derive(Debug)]
pub struct Client<T> {
    seam: GraphSeam,
    transport: T,
}

impl<T: Transport> Client<T> {
    /// A client for `seam`'s calendar that sends through `transport`.
    #[must_use]
    pub fn new(seam: GraphSeam, transport: T) -> Self {
        Self { seam, transport }
    }

    /// Performs `request`, sending every call its exchange needs, and returns its outcome.
    pub fn perform(&mut self, request: &Request) -> Outcome {
        let mut exchange = self.seam.call(request);
        while let Some(call) = exchange.request().cloned() {
            let response = self.transport.send(&call);
            exchange.receive(response);
        }
        self.seam.outcome(request, exchange)
    }

    /// The seam.
    #[must_use]
    pub fn seam(&self) -> &GraphSeam {
        &self.seam
    }

    /// The transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// The transport, giving up the client.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }
}

#[cfg(test)]
mod tests;
