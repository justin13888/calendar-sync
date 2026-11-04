//! The engine's [`ProviderSeam`] for Google Calendar, and a blocking client over an injected
//! [`Transport`].

use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::ProviderEvent;
use serde_json::Value;

use crate::event::{self, Item};
use crate::http::{HttpRequest, HttpResponse, Method, Transport, TransportError, encode};

/// The `errors[].reason` values of a `403` that mean "slow down", not "never".
const RATE_LIMITED: [&str; 4] = [
    "rateLimitExceeded",
    "userRateLimitExceeded",
    "quotaExceeded",
    "dailyLimitExceeded",
];

/// Translates the engine's requests into Google Calendar API calls on one calendar, and the
/// responses back.
///
/// | Request | Call |
/// |---|---|
/// | `List` from the start | `GET events?maxResults=N` |
/// | `List` since a cursor | `GET events?maxResults=N&syncToken=T` |
/// | `List` continuing | `GET events?maxResults=N&pageToken=T` |
/// | `Read`, `Lookup` | `GET events/ID`. Ids are client-chosen, so a lookup key is the id |
/// | `Create` | `POST events`, with the id in the body |
/// | `Update` | `PUT events/ID` with `If-Match` |
/// | `Delete` | `DELETE events/ID` with `If-Match` |
///
/// The list never sends `timeMin` or `timeMax`, which Google refuses beside a `syncToken`; filter
/// with [`Window`](crate::Window) instead. A page token continues a pass, and only the last page
/// carries the `nextSyncToken` the next pass starts from.
///
/// | Response | Outcome |
/// |---|---|
/// | `410` to a list | [`Failure::CursorExpired`] |
/// | `404` or `410` to a read | `Read(None)` |
/// | `404` or `410` to an update or a delete | [`Failure::NotFound`] |
/// | `409` to a create | [`Failure::AlreadyExists`] |
/// | `412` | [`Failure::PreconditionFailed`] |
/// | `401`, `408`, `429`, `5xx`, a rate-limit `403`, no response, an unreadable `2xx` | [`Failure::Transient`] |
/// | any other status | [`Failure::Rejected`] with Google's message |
///
/// A `401` is transient because the transport owns credentials: it refreshes them and the engine
/// tries again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoogleSeam {
    calendar_id: String,
    page_size: Option<u32>,
}

impl GoogleSeam {
    /// The seam for the calendar `calendar_id`, such as `primary`.
    #[must_use]
    pub fn new(calendar_id: impl Into<String>) -> Self {
        Self {
            calendar_id: calendar_id.into(),
            page_size: None,
        }
    }

    /// Asks for at most `page_size` events per page (`maxResults`). Google's default is 250.
    #[must_use]
    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// The calendar's id.
    #[must_use]
    pub fn calendar_id(&self) -> &str {
        &self.calendar_id
    }

    fn events(&self) -> String {
        format!(
            "/calendar/v3/calendars/{}/events",
            encode(&self.calendar_id)
        )
    }

    fn event(&self, id: &ResourceId) -> String {
        format!("{}/{}", self.events(), encode(id.as_str()))
    }

    fn list(&self, token: Option<(&str, &Cursor)>) -> String {
        let mut query = Vec::new();
        if let Some(size) = self.page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some((name, cursor)) = token {
            query.push(format!("{name}={}", encode(cursor.as_str())));
        }
        if query.is_empty() {
            self.events()
        } else {
            format!("{}?{}", self.events(), query.join("&"))
        }
    }
}

fn get(path: String) -> HttpRequest {
    HttpRequest {
        method: Method::Get,
        path,
        headers: Vec::new(),
        body: None,
    }
}

fn json_headers(if_match: Option<&calendar_sync_model::ProviderRevision>) -> Vec<(String, String)> {
    let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    if let Some(revision) = if_match {
        headers.push(("If-Match".to_owned(), revision.as_str().to_owned()));
    }
    headers
}

impl ProviderSeam for GoogleSeam {
    type Call = HttpRequest;
    type Response = Result<HttpResponse, TransportError>;

    fn call(&self, request: &Request) -> HttpRequest {
        match request {
            Request::List { from } => get(match from {
                ListFrom::Start => self.list(None),
                ListFrom::Since(cursor) => self.list(Some(("syncToken", cursor))),
                ListFrom::Continue(cursor) => self.list(Some(("pageToken", cursor))),
            }),
            Request::Read { resource } | Request::Lookup { key: resource } => {
                get(self.event(resource))
            }
            Request::Create { resource, event } => {
                let mut body = event::write(event).body;
                body.insert("id".to_owned(), Value::String(resource.as_str().to_owned()));
                HttpRequest {
                    method: Method::Post,
                    path: self.events(),
                    headers: json_headers(None),
                    body: Some(Value::Object(body).to_string()),
                }
            }
            Request::Update {
                resource,
                if_match,
                event,
            } => HttpRequest {
                method: Method::Put,
                path: self.event(resource),
                headers: json_headers(if_match.as_ref()),
                body: Some(Value::Object(event::write(event).body).to_string()),
            },
            Request::Delete { resource, if_match } => HttpRequest {
                method: Method::Delete,
                path: self.event(resource),
                headers: if_match
                    .iter()
                    .map(|revision| ("If-Match".to_owned(), revision.as_str().to_owned()))
                    .collect(),
                body: None,
            },
        }
    }

    fn outcome(&self, request: &Request, response: Self::Response) -> Outcome {
        let Ok(response) = response else {
            return Outcome::Failed(Failure::Transient);
        };
        if (200..300).contains(&response.status) {
            return success(request, &response);
        }
        let reason = ErrorBody::of(&response);
        let failure = match (response.status, request) {
            (410, Request::List { .. }) => Failure::CursorExpired,
            (404 | 410, Request::Read { .. } | Request::Lookup { .. }) => {
                return Outcome::Read(None);
            }
            (404 | 410, Request::Update { .. } | Request::Delete { .. }) => Failure::NotFound,
            (409, Request::Create { .. }) => Failure::AlreadyExists,
            (412, _) => Failure::PreconditionFailed,
            (401 | 408 | 429 | 500..=599, _) => Failure::Transient,
            (403, _) if reason.rate_limited() => Failure::Transient,
            (status, _) => Failure::Rejected(reason.describe(status)),
        };
        Outcome::Failed(failure)
    }
}

fn success(request: &Request, response: &HttpResponse) -> Outcome {
    if matches!(request, Request::Delete { .. }) {
        return Outcome::Deleted;
    }
    // A 2xx whose body cannot be read may still have taken effect: the engine reads again.
    let Ok(body) = serde_json::from_slice::<Value>(&response.body) else {
        return Outcome::Failed(Failure::Transient);
    };
    match request {
        Request::List { .. } => page(&body),
        Request::Read { .. } | Request::Lookup { .. } => match event::read(&body) {
            Ok(read) => Outcome::Read(match read.item {
                Item::Event(event) => Some(event),
                Item::Deleted { .. } => None,
            }),
            Err(error) => Outcome::Failed(Failure::Rejected(error.to_string())),
        },
        Request::Create { .. } | Request::Update { .. } => match event::read(&body) {
            Ok(read) => match read.item {
                Item::Event(event) => Outcome::Written(event),
                Item::Deleted { .. } => Outcome::Failed(Failure::Transient),
            },
            Err(_) => Outcome::Failed(Failure::Transient),
        },
        Request::Delete { .. } => Outcome::Deleted,
    }
}

/// Reads an `Events` page.
fn page(body: &Value) -> Outcome {
    let Some(object) = body.as_object() else {
        return Outcome::Failed(Failure::Transient);
    };
    let mut changes = Vec::new();
    for item in object
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match event::read(item) {
            Ok(read) => changes.push(match read.item {
                Item::Deleted { id } => Change::Remove {
                    resource: ResourceId::new(id),
                },
                Item::Event(event) => match upsert(event) {
                    Ok(change) => change,
                    Err(failure) => return Outcome::Failed(failure),
                },
            }),
            Err(error) => return Outcome::Failed(Failure::Rejected(error.to_string())),
        }
    }
    let token = |name: &str| object.get(name).and_then(Value::as_str).map(Cursor::new);
    let next = if let Some(page) = token("nextPageToken") {
        Next::More(page)
    } else if let Some(sync) = token("nextSyncToken") {
        Next::Done(sync)
    } else {
        return Outcome::Failed(Failure::Rejected(
            "the last page carries no nextSyncToken".to_owned(),
        ));
    };
    Outcome::Listed(Page { changes, next })
}

fn upsert(event: ProviderEvent) -> Result<Change, Failure> {
    let revision = event
        .revision
        .clone()
        .ok_or_else(|| Failure::Rejected(format!("event {} has no etag", event.ids.id)))?;
    Ok(Change::Upsert {
        resource: ResourceId::of(&event),
        revision,
        event: Some(event),
    })
}

/// Google's error body: `{"error": {"code", "message", "errors": [{"reason"}]}}`.
struct ErrorBody {
    message: Option<String>,
    reasons: Vec<String>,
}

impl ErrorBody {
    fn of(response: &HttpResponse) -> Self {
        let error = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|body| body.get("error").cloned())
            .and_then(|error| error.as_object().cloned())
            .unwrap_or_default();
        Self {
            message: error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
            reasons: error
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get("reason").and_then(Value::as_str))
                .map(str::to_owned)
                .collect(),
        }
    }

    fn rate_limited(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| RATE_LIMITED.contains(&reason.as_str()))
    }

    fn describe(&self, status: u16) -> String {
        match (&self.message, self.reasons.first()) {
            (Some(message), Some(reason)) => format!("{status} {reason}: {message}"),
            (Some(message), None) => format!("{status}: {message}"),
            (None, Some(reason)) => format!("{status} {reason}"),
            (None, None) => format!("{status}"),
        }
    }
}

/// A blocking client: the seam over an injected [`Transport`].
///
/// An async application uses [`GoogleSeam`] directly, sending each call with its own client.
#[derive(Debug)]
pub struct Client<T> {
    seam: GoogleSeam,
    transport: T,
}

impl<T: Transport> Client<T> {
    /// A client for `seam`'s calendar that sends through `transport`.
    #[must_use]
    pub fn new(seam: GoogleSeam, transport: T) -> Self {
        Self { seam, transport }
    }

    /// Performs `request` and returns its outcome.
    pub fn perform(&mut self, request: &Request) -> Outcome {
        let call = self.seam.call(request);
        let response = self.transport.send(&call);
        self.seam.outcome(request, response)
    }

    /// The seam.
    #[must_use]
    pub fn seam(&self) -> &GoogleSeam {
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
