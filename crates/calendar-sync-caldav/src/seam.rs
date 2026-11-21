//! The engine's provider seam for one `CalDAV` calendar collection.

use std::collections::BTreeMap;

use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::{ProviderEvent, ProviderRevision};
use http::Uri;
use jiff::Timestamp;
use libdav::caldav::GetCalendarResources;
use libdav::dav::CheckSupport;
use libdav::dav::{
    Delete, ListResources, PutResource, SyncCollection, SyncCollectionParseError, make_relative_url,
};
use libdav::names;
use libdav::requests::{DavRequest, ParseResponseError};
use serde_json::{Map, Value};

use crate::capabilities::{Capabilities, Discovery, DiscoveryError};
use crate::http::{HttpRequest, HttpResponse, Transport, TransportError};
use crate::object;

/// The most objects one `calendar-multiget` report asks for.
pub const DEFAULT_BATCH: usize = 100;

/// The `Content-Type` of an object the seam writes.
const CALENDAR_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

/// What a sync-collection cursor starts with.
const SYNC_PREFIX: &str = "sync:";
/// What an `ETag` snapshot cursor starts with.
const ETAGS_PREFIX: &str = "etags:";

/// How the seam lists a collection's changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Listing {
    /// The `DAV:sync-collection` report (RFC 6578): only what changed since the last token is
    /// listed, removals included. A server that refuses the report on a full listing is listed
    /// by [`Listing::Etags`] instead, in the same request.
    SyncCollection,
    /// A `PROPFIND` of every member's `ETag`, compared with the `ETag`s of the last pass, which
    /// the cursor holds. For a server without `sync-collection`. Every pass reads the whole
    /// collection's `ETag`s, and the cursor grows with the collection.
    Etags,
}

impl Listing {
    /// The listing that issued `cursor`, or `None` where this seam did not issue it.
    #[must_use]
    pub fn of(cursor: &Cursor) -> Option<Self> {
        let text = cursor.as_str();
        if text.starts_with(SYNC_PREFIX) {
            Some(Self::SyncCollection)
        } else if text.starts_with(ETAGS_PREFIX) {
            Some(Self::Etags)
        } else {
            None
        }
    }
}

/// Why a seam could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlError(String);

impl core::fmt::Display for UrlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "not a server URL: {}", self.0)
    }
}

impl core::error::Error for UrlError {}

/// The seam for one calendar collection: the engine's
/// [`ProviderSeam`] over `CalDAV`.
///
/// Resource ids are hrefs, the percent-encoded path of each calendar object resource, as the
/// server lists them. A create names the href it creates
/// ([`DerivedIds::caldav_href`](calendar_sync_engine::DerivedIds::caldav_href)), and is sent with
/// `If-None-Match: *`, so a repeat is refused rather than duplicated.
#[derive(Clone, Debug)]
pub struct CalDavSeam {
    base: Uri,
    collection: String,
    listing: Listing,
    batch: usize,
    stamp: Timestamp,
}

impl CalDavSeam {
    /// A seam for the collection at `collection`, a path such as `/dav/calendars/user/work/`, on
    /// the server at `base_url`, such as `https://dav.example.com`. `stamp` is the `DTSTAMP` of a
    /// `VEVENT` a write adds; the application advances it with [`CalDavSeam::set_stamp`].
    ///
    /// # Errors
    ///
    /// [`UrlError`] where `base_url` has no scheme and host.
    pub fn new(
        base_url: &str,
        collection: impl Into<String>,
        stamp: Timestamp,
    ) -> Result<Self, UrlError> {
        let base: Uri = base_url
            .parse()
            .map_err(|_| UrlError(base_url.to_owned()))?;
        if base.scheme().is_none() || base.authority().is_none() {
            return Err(UrlError(base_url.to_owned()));
        }
        let mut collection = collection.into();
        if !collection.ends_with('/') {
            collection.push('/');
        }
        // libdav compares each listed href, normalised, with the collection's own href.
        let collection = libdav::encoding::normalise_percent_encoded(&collection)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or(collection);
        Ok(Self {
            base,
            collection,
            listing: Listing::SyncCollection,
            batch: DEFAULT_BATCH,
            stamp,
        })
    }

    /// The seam listing by `listing`, as [`Capabilities::listing`] chose it.
    #[must_use]
    pub fn with_listing(mut self, listing: Listing) -> Self {
        self.listing = listing;
        self
    }

    /// The seam asking for at most `batch` objects per `calendar-multiget` report.
    #[must_use]
    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Sets how a later full listing starts, as [`Capabilities::listing`] chose it. A cursor
    /// already issued keeps the listing that issued it.
    pub fn set_listing(&mut self, listing: Listing) {
        self.listing = listing;
    }

    /// Sets the `DTSTAMP` of every `VEVENT` a later write adds.
    pub fn set_stamp(&mut self, stamp: Timestamp) {
        self.stamp = stamp;
    }

    /// The collection's href.
    #[must_use]
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// The server's base URL.
    #[must_use]
    pub fn base_url(&self) -> &Uri {
        &self.base
    }

    /// How a full listing starts.
    #[must_use]
    pub fn listing(&self) -> Listing {
        self.listing
    }

    /// The exchange that discovers what the server offers for the collection.
    #[must_use]
    pub fn discover(&self) -> Discovery {
        Discovery::new(self)
    }

    /// The exchange that performs `request`.
    #[must_use]
    pub fn exchange(&self, request: &Request) -> Exchange {
        let state = match self.first(request) {
            Ok(pending) => State::Await(pending),
            Err(failure) => State::Done(Outcome::Failed(failure)),
        };
        Exchange {
            seam: self.clone(),
            state,
        }
    }

    pub(crate) fn prepare_options(&self) -> Result<HttpRequest, String> {
        let uri = make_relative_url(self.base.clone(), &self.collection)
            .map_err(|error| error.to_string())?;
        CheckSupport::caldav(&uri)
            .prepare_request(self.base.clone())
            .map(|request| HttpRequest::from_http(&request))
            .map_err(|error| error.to_string())
    }

    fn prepare(&self, request: &impl DavRequest) -> Result<HttpRequest, Failure> {
        request
            .prepare_request(self.base.clone())
            .map(|request| HttpRequest::from_http(&request))
            .map_err(|error| refusal(format!("the request could not be built: {error}")))
    }

    /// A request `libdav` has no type for: a plain `GET`, or an unconditional `PUT`.
    fn plain(
        &self,
        method: &str,
        href: &str,
        body: Option<String>,
    ) -> Result<HttpRequest, Failure> {
        let uri = make_relative_url(self.base.clone(), href)
            .map_err(|error| refusal(format!("not an href: {href}: {error}")))?;
        let mut headers = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type".to_owned(), CALENDAR_CONTENT_TYPE.to_owned()));
        }
        Ok(HttpRequest {
            method: method.to_owned(),
            url: uri.to_string(),
            headers,
            body,
        })
    }

    fn first(&self, request: &Request) -> Result<Pending, Failure> {
        match request {
            Request::List { from } => self.list(from),
            Request::Read { resource } | Request::Lookup { key: resource } => Ok(Pending {
                http: self.plain("GET", resource.as_str(), None)?,
                step: Step::Read {
                    href: resource.as_str().to_owned(),
                },
            }),
            Request::Create { resource, event } => {
                let body = self.body(event)?;
                let put =
                    PutResource::new(resource.as_str()).create(body.clone(), CALENDAR_CONTENT_TYPE);
                Ok(Pending {
                    http: self.prepare(&put)?,
                    step: Step::Put {
                        href: resource.as_str().to_owned(),
                        body,
                        create: true,
                    },
                })
            }
            Request::Update {
                resource,
                if_match,
                event,
            } => {
                let body = self.body(event)?;
                let http = match if_match {
                    Some(etag) => self.prepare(&PutResource::new(resource.as_str()).update(
                        body.clone(),
                        CALENDAR_CONTENT_TYPE,
                        etag.as_str(),
                    ))?,
                    None => self.plain("PUT", resource.as_str(), Some(body.clone()))?,
                };
                Ok(Pending {
                    http,
                    step: Step::Put {
                        href: resource.as_str().to_owned(),
                        body,
                        create: false,
                    },
                })
            }
            Request::Delete { resource, if_match } => {
                let delete = Delete::new(resource.as_str());
                let http = match if_match {
                    Some(etag) => self.prepare(&delete.with_etag(etag.as_str()))?,
                    None => self.prepare(&delete.force())?,
                };
                Ok(Pending {
                    http,
                    step: Step::Delete,
                })
            }
        }
    }

    fn body(&self, event: &ProviderEvent) -> Result<String, Failure> {
        object::write(event, self.stamp)
            .map(|(body, _losses)| body)
            .map_err(|error| refusal(format!("the object could not be written: {error}")))
    }

    fn list(&self, from: &ListFrom) -> Result<Pending, Failure> {
        match from {
            ListFrom::Start => match self.listing {
                Listing::SyncCollection => self.sync(None, true),
                Listing::Etags => self.scan(None),
            },
            ListFrom::Since(cursor) | ListFrom::Continue(cursor) => {
                let text = cursor.as_str();
                if let Some(token) = text.strip_prefix(SYNC_PREFIX) {
                    self.sync(Some(token.to_owned()), false)
                } else if let Some(snapshot) = text.strip_prefix(ETAGS_PREFIX) {
                    match serde_json::from_str::<BTreeMap<String, String>>(snapshot) {
                        Ok(snapshot) => self.scan(Some(snapshot)),
                        Err(_) => Err(Failure::CursorExpired),
                    }
                } else {
                    Err(Failure::CursorExpired)
                }
            }
        }
    }

    fn sync(&self, token: Option<String>, start: bool) -> Result<Pending, Failure> {
        let mut report = SyncCollection::new(&self.collection);
        if let Some(token) = &token {
            report = report.with_sync_token(token);
        }
        Ok(Pending {
            http: self.prepare(&report)?,
            step: Step::Sync { token, start },
        })
    }

    fn scan(&self, snapshot: Option<BTreeMap<String, String>>) -> Result<Pending, Failure> {
        Ok(Pending {
            http: self.prepare(&ListResources::new(&self.collection))?,
            step: Step::Scan { snapshot },
        })
    }

    /// The `calendar-multiget` of the next batch of `fetch.pending`, or the page where none is
    /// left.
    fn fetch(&self, mut fetch: Fetch) -> State {
        if fetch.pending.is_empty() {
            return State::Done(Outcome::Listed(Page {
                changes: fetch.changes,
                next: fetch.next,
            }));
        }
        let take = fetch.pending.len().min(self.batch);
        let batch: Vec<(String, String)> = fetch.pending.drain(..take).collect();
        let report = GetCalendarResources::new(&self.collection)
            .with_hrefs(batch.iter().map(|(href, _)| href.as_str()));
        match self.prepare(&report) {
            Ok(http) => State::Await(Pending {
                http,
                step: Step::Fetch { fetch, batch },
            }),
            Err(_) => {
                // The batch cannot be asked for: the engine reads each object on its own.
                fetch
                    .changes
                    .extend(batch.into_iter().map(|(href, etag)| bare(&href, &etag)));
                self.fetch(fetch)
            }
        }
    }
}

impl ProviderSeam for CalDavSeam {
    type Call = Exchange;
    type Response = Exchange;

    fn call(&self, request: &Request) -> Exchange {
        self.exchange(request)
    }

    /// The outcome of `response`, the exchange [`CalDavSeam::call`] returned, driven to its end.
    /// An exchange abandoned part-way may have taken effect, so it is [`Failure::Transient`].
    fn outcome(&self, _request: &Request, response: Exchange) -> Outcome {
        response.into_outcome()
    }
}

/// One engine request performed as one or more HTTP requests.
///
/// Send [`Exchange::request`] and hand its response to [`Exchange::receive`] until there is no
/// request left; then [`Exchange::into_outcome`] (or [`ProviderSeam::outcome`]) is the outcome. A
/// listing takes one report, then one `calendar-multiget` per batch of changed objects; a write
/// whose response carries no `ETag` takes a `GET` to read what the server holds.
///
/// ```
/// use calendar_sync_caldav::{CalDavSeam, HttpResponse};
/// use calendar_sync_engine::{Outcome, ProviderSeam, Request, ResourceId};
///
/// let seam = CalDavSeam::new(
///     "https://dav.example.com",
///     "/cal/",
///     "2026-11-20T00:00:00Z".parse().unwrap(),
/// )
/// .unwrap();
/// let request = Request::Read { resource: ResourceId::new("/cal/a.ics") };
/// let mut exchange = seam.call(&request);
/// let call = exchange.request().expect("one call").clone();
/// assert_eq!((call.method.as_str(), call.url.as_str()), ("GET", "https://dav.example.com/cal/a.ics"));
/// exchange.receive(Ok(HttpResponse::new(404, "")));
/// assert!(exchange.request().is_none());
/// assert_eq!(seam.outcome(&request, exchange), Outcome::Read(None));
/// ```
#[derive(Clone, Debug)]
pub struct Exchange {
    seam: CalDavSeam,
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
    /// A `GET` of one object.
    Read { href: String },
    /// A `PUT` of `body`.
    Put {
        href: String,
        body: String,
        create: bool,
    },
    /// The `GET` after a `PUT` whose response carried no `ETag`.
    ReadBack { href: String },
    /// A `DELETE`.
    Delete,
    /// A `sync-collection` report.
    Sync { token: Option<String>, start: bool },
    /// A `PROPFIND` of every member's `ETag`.
    Scan {
        snapshot: Option<BTreeMap<String, String>>,
    },
    /// A `calendar-multiget` of `batch`, each href with the `ETag` the listing gave it.
    Fetch {
        fetch: Fetch,
        batch: Vec<(String, String)>,
    },
}

/// A listed page whose changed objects are still being read.
#[derive(Clone, Debug)]
struct Fetch {
    changes: Vec<Change>,
    pending: Vec<(String, String)>,
    next: Next,
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
        let step = core::mem::replace(&mut pending.step, Step::Delete);
        let Ok(response) = response else {
            self.state = State::Done(Outcome::Failed(Failure::Transient));
            return;
        };
        self.state = match step {
            Step::Read { href } => State::Done(read(&href, &response)),
            Step::Put { href, body, create } => self.put(href, &body, create, &response),
            Step::ReadBack { href } => State::Done(match read(&href, &response) {
                Outcome::Read(Some(event)) => Outcome::Written(event),
                // The write took effect; what the server now holds is unknown.
                _ => Outcome::Failed(Failure::Transient),
            }),
            Step::Delete => State::Done(deleted(&response)),
            Step::Sync { token, start } => self.synced(token.as_deref(), start, &response),
            Step::Scan { snapshot } => self.scanned(snapshot, &response),
            Step::Fetch { fetch, batch } => self.fetched(fetch, batch, &response),
        };
    }

    fn put(&self, href: String, body: &str, create: bool, response: &HttpResponse) -> State {
        if response.is_success() {
            let Some(etag) = response.header("ETag") else {
                // RFC 4791 section 5.3.4: no ETag when the server changed what it stored.
                return match self.seam.plain("GET", &href, None) {
                    Ok(http) => State::Await(Pending {
                        http,
                        step: Step::ReadBack { href },
                    }),
                    Err(_) => State::Done(Outcome::Failed(Failure::Transient)),
                };
            };
            let tag = response.header("Schedule-Tag");
            return State::Done(
                match object::read(&href, Some(etag), tag, body.as_bytes()) {
                    Some(event) => Outcome::Written(event),
                    None => rejected("the written object holds no VEVENT".to_owned()),
                },
            );
        }
        State::Done(match response.status {
            412 if create => Outcome::Failed(Failure::AlreadyExists),
            412 => Outcome::Failed(Failure::PreconditionFailed),
            404 | 410 if !create => Outcome::Failed(Failure::NotFound),
            status => failed(status, response),
        })
    }

    fn synced(&self, token: Option<&str>, start: bool, response: &HttpResponse) -> State {
        let report = SyncCollection::new(&self.seam.collection);
        let report = match token {
            Some(token) => report.with_sync_token(token),
            None => report,
        };
        match report.parse_response(&response.parts(), &response.body) {
            Ok(parsed) => {
                let mut changes: Vec<Change> = parsed
                    .deleted
                    .into_iter()
                    .map(|href| Change::Remove {
                        resource: ResourceId::new(href),
                    })
                    .collect();
                let pending = parsed
                    .changed
                    .into_iter()
                    .filter(|listed| !listed.resource_type.is_collection)
                    .filter_map(|listed| listed.etag.map(|etag| (listed.href, etag)))
                    .collect();
                let cursor = Cursor::new(format!("{SYNC_PREFIX}{}", parsed.new_sync_token));
                let next = if truncated(&response.body) {
                    Next::More(cursor)
                } else {
                    Next::Done(cursor)
                };
                changes.shrink_to_fit();
                self.seam.fetch(Fetch {
                    changes,
                    pending,
                    next,
                })
            }
            Err(SyncCollectionParseError::PreconditionFailed(_)) => {
                State::Done(Outcome::Failed(Failure::CursorExpired))
            }
            Err(SyncCollectionParseError::Parse(ParseResponseError::BadStatusCode(status)))
                if refuses_report(status.as_u16()) =>
            {
                if start {
                    // The server does not offer the report: list by ETag instead.
                    match self.seam.scan(None) {
                        Ok(pending) => State::Await(pending),
                        Err(failure) => State::Done(Outcome::Failed(failure)),
                    }
                } else {
                    // A token the server no longer accepts in any form: list again from the
                    // start, which falls back where it must.
                    State::Done(Outcome::Failed(Failure::CursorExpired))
                }
            }
            Err(SyncCollectionParseError::Parse(ParseResponseError::BadStatusCode(status))) => {
                State::Done(failed(status.as_u16(), response))
            }
            Err(SyncCollectionParseError::Parse(error)) => State::Done(rejected(format!(
                "unreadable sync-collection report: {error}"
            ))),
        }
    }

    fn scanned(
        &self,
        snapshot: Option<BTreeMap<String, String>>,
        response: &HttpResponse,
    ) -> State {
        let listing = ListResources::new(&self.seam.collection);
        let parsed = match listing.parse_response(&response.parts(), &response.body) {
            Ok(parsed) => parsed,
            Err(ParseResponseError::BadStatusCode(status)) => {
                return State::Done(failed(status.as_u16(), response));
            }
            Err(error) => {
                return State::Done(rejected(format!("unreadable PROPFIND response: {error}")));
            }
        };
        let now: BTreeMap<String, String> = parsed
            .resources
            .into_iter()
            .filter(|listed| !listed.resource_type.is_collection)
            .filter_map(|listed| listed.etag.map(|etag| (listed.href, etag)))
            .collect();
        let before = snapshot.unwrap_or_default();
        let changes = before
            .keys()
            .filter(|href| !now.contains_key(*href))
            .map(|href| Change::Remove {
                resource: ResourceId::new(href.clone()),
            })
            .collect();
        let pending = now
            .iter()
            .filter(|(href, etag)| before.get(*href) != Some(*etag))
            .map(|(href, etag)| (href.clone(), etag.clone()))
            .collect();
        let snapshot: Map<String, Value> = now
            .into_iter()
            .map(|(href, etag)| (href, Value::String(etag)))
            .collect();
        let cursor = Cursor::new(format!("{ETAGS_PREFIX}{}", Value::Object(snapshot)));
        self.seam.fetch(Fetch {
            changes,
            pending,
            next: Next::Done(cursor),
        })
    }

    fn fetched(
        &self,
        mut fetch: Fetch,
        batch: Vec<(String, String)>,
        response: &HttpResponse,
    ) -> State {
        if transient(response.status) {
            return State::Done(Outcome::Failed(Failure::Transient));
        }
        let report = GetCalendarResources::new(&self.seam.collection)
            .with_hrefs(batch.iter().map(|(href, _)| href.as_str()));
        let Ok(parsed) = report.parse_response(&response.parts(), &response.body) else {
            // The report failed: the engine reads each object on its own.
            fetch
                .changes
                .extend(batch.iter().map(|(href, etag)| bare(href, etag)));
            return self.seam.fetch(fetch);
        };
        let mut found: BTreeMap<String, libdav::FetchedResource> = parsed
            .resources
            .into_iter()
            .map(|resource| (resource.href.clone(), resource))
            .collect();
        for (href, listed_etag) in batch {
            let change = match found.remove(&href).map(|resource| resource.content) {
                Some(Ok(content)) => {
                    match object::read(&href, Some(&content.etag), None, content.data.as_bytes()) {
                        Some(event) => Some(Change::Upsert {
                            resource: ResourceId::new(href),
                            revision: ProviderRevision::new(content.etag),
                            event: Some(event),
                        }),
                        // Not an event: a task or a journal entry in a mixed collection.
                        None => None,
                    }
                }
                Some(Err(status)) if matches!(status.as_u16(), 404 | 410) => Some(Change::Remove {
                    resource: ResourceId::new(href),
                }),
                _ => Some(bare(&href, &listed_etag)),
            };
            fetch.changes.extend(change);
        }
        self.seam.fetch(fetch)
    }
}

/// A change the engine reads before it uses: the object's href and `ETag`, and no event.
fn bare(href: &str, etag: &str) -> Change {
    Change::Upsert {
        resource: ResourceId::new(href),
        revision: ProviderRevision::new(etag),
        event: None,
    }
}

/// The outcome of a `GET` of `href`.
fn read(href: &str, response: &HttpResponse) -> Outcome {
    match response.status {
        200 => Outcome::Read(object::read(
            href,
            response.header("ETag"),
            response.header("Schedule-Tag"),
            &response.body,
        )),
        404 | 410 => Outcome::Read(None),
        status => failed(status, response),
    }
}

/// The outcome of a `DELETE`.
fn deleted(response: &HttpResponse) -> Outcome {
    match response.status {
        status if (200..300).contains(&status) => Outcome::Deleted,
        412 => Outcome::Failed(Failure::PreconditionFailed),
        404 | 410 => Outcome::Failed(Failure::NotFound),
        status => failed(status, response),
    }
}

/// Whether a report answered with `status` is one the server does not offer for this
/// collection: it is unknown (`400`, `501`), not allowed on the resource (`403`, `405`), or its
/// body not understood (`415`).
fn refuses_report(status: u16) -> bool {
    matches!(status, 400 | 403 | 405 | 415 | 501)
}

/// Whether `status` says nothing about the request, which may be tried again.
fn transient(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// The failure an unexpected status stands for.
fn failed(status: u16, response: &HttpResponse) -> Outcome {
    if transient(status) {
        return Outcome::Failed(Failure::Transient);
    }
    let detail = core::str::from_utf8(&response.body)
        .ok()
        .and_then(precondition)
        .map_or(String::new(), |name| format!(" ({name})"));
    rejected(format!("the server answered {status}{detail}"))
}

/// The precondition or postcondition an error body names (RFC 4918 section 16), such as
/// `CALDAV:valid-calendar-data`.
fn precondition(body: &str) -> Option<String> {
    let document = roxmltree::Document::parse(body).ok()?;
    let root = document.root_element();
    if root.tag_name().name() != "error" {
        return None;
    }
    let condition = root.children().find(roxmltree::Node::is_element)?;
    let prefix = match condition.tag_name().namespace() {
        Some(names::DAV) => "DAV:",
        Some(names::CALDAV) => "CALDAV:",
        _ => "",
    };
    Some(format!("{prefix}{}", condition.tag_name().name()))
}

/// Whether a `sync-collection` multistatus was truncated: a response carries `507 Insufficient
/// Storage` (RFC 6578 section 3.6), which only the collection's own response may.
fn truncated(body: &[u8]) -> bool {
    let Ok(text) = core::str::from_utf8(body) else {
        return false;
    };
    let Ok(document) = roxmltree::Document::parse(text) else {
        return false;
    };
    document
        .descendants()
        .filter(|node| node.tag_name() == names::RESPONSE)
        .flat_map(|response| response.children())
        .filter(|node| node.tag_name() == names::STATUS)
        .filter_map(|status| status.text())
        .any(|status| status.split_whitespace().nth(1) == Some("507"))
}

fn rejected(reason: String) -> Outcome {
    Outcome::Failed(refusal(reason))
}

fn refusal(reason: String) -> Failure {
    Failure::Rejected(reason)
}

/// A blocking client: the seam over an injected [`Transport`].
///
/// An async application drives each [`Exchange`] itself, sending its requests with its own
/// client.
#[derive(Debug)]
pub struct Client<T> {
    seam: CalDavSeam,
    transport: T,
}

impl<T: Transport> Client<T> {
    /// A client for `seam`'s collection that sends through `transport`.
    #[must_use]
    pub fn new(seam: CalDavSeam, transport: T) -> Self {
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

    /// Discovers what the server offers for the collection.
    ///
    /// # Errors
    ///
    /// The [`DiscoveryError`] that stopped discovery.
    pub fn discover(&mut self) -> Result<Capabilities, DiscoveryError> {
        let mut discovery = self.seam.discover();
        while let Some(call) = discovery.request().cloned() {
            let response = self.transport.send(&call);
            discovery.receive(response);
        }
        discovery.into_result()
    }

    /// The seam.
    #[must_use]
    pub fn seam(&self) -> &CalDavSeam {
        &self.seam
    }

    /// The seam, to change its listing or advance its stamp.
    pub fn seam_mut(&mut self) -> &mut CalDavSeam {
        &mut self.seam
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
