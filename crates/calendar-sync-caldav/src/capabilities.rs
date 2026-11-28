//! What a server offers, discovered before syncing: an `OPTIONS` request for the `DAV` header
//! (RFC 4918 section 10.1, RFC 4791 section 5.1, RFC 6638 section 2) and a `PROPFIND` of the
//! collection for `DAV:supported-report-set` (RFC 3253, as RFC 6578 section 3.1 uses it),
//! `CALDAV:supported-calendar-component-set` (RFC 4791 section 5.2.3) and `DAV:sync-token`.

use core::fmt;

use libdav::dav::Propfind;
use libdav::requests::DavRequest;
use libdav::{Depth, names};

use crate::http::{HttpRequest, HttpResponse, TransportError};
use crate::seam::{CalDavSeam, Listing};

/// A feature a server may lack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// `calendar-access` in the `DAV` header: the server is a `CalDAV` server at all.
    CalendarAccess,
    /// The `DAV:sync-collection` report. Without it the seam lists by comparing `ETag`s
    /// ([`Listing::Etags`]).
    SyncCollection,
    /// `VEVENT` among the collection's supported components. Without it the collection holds no
    /// events (it is a task list).
    Events,
    /// `calendar-auto-schedule` in the `DAV` header: the server sends iTIP messages itself. Without
    /// it an attendee's reply is never delivered, so an RSVP is unsupported.
    AutoSchedule,
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CalendarAccess => "calendar-access (RFC 4791)",
            Self::SyncCollection => "the sync-collection report (RFC 6578)",
            Self::Events => "VEVENT in supported-calendar-component-set (RFC 4791)",
            Self::AutoSchedule => "calendar-auto-schedule (RFC 6638)",
        })
    }
}

/// What a server offers for one collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// The `DAV` header lists `calendar-access`.
    pub calendar_access: bool,
    /// The `DAV` header lists `calendar-auto-schedule`.
    pub auto_schedule: bool,
    /// The collection supports the `DAV:sync-collection` report: its
    /// `DAV:supported-report-set` lists it, or it has a `DAV:sync-token`.
    pub sync_collection: bool,
    /// The collection may hold `VEVENT`s: its `CALDAV:supported-calendar-component-set` lists
    /// `VEVENT`, or it has none, which RFC 4791 reads as every component.
    pub events: bool,
    /// The collection's `DAV:sync-token`, where it has one.
    pub sync_token: Option<String>,
    /// Every token of the `DAV` header, as the server sent them.
    pub dav: Vec<String>,
}

impl Capabilities {
    /// The features the server lacks, in [`Feature`] order.
    #[must_use]
    pub fn missing(&self) -> Vec<Feature> {
        [
            (Feature::CalendarAccess, self.calendar_access),
            (Feature::SyncCollection, self.sync_collection),
            (Feature::Events, self.events),
            (Feature::AutoSchedule, self.auto_schedule),
        ]
        .into_iter()
        .filter(|(_, offered)| !offered)
        .map(|(feature, _)| feature)
        .collect()
    }

    /// How the seam should list this collection.
    #[must_use]
    pub fn listing(&self) -> Listing {
        if self.sync_collection {
            Listing::SyncCollection
        } else {
            Listing::Etags
        }
    }
}

/// Why discovery failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    /// No response arrived.
    Transport(TransportError),
    /// The server answered with this status.
    Status(u16),
    /// The request could not be built, or the response not read.
    Malformed(String),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(f),
            Self::Status(status) => write!(f, "the server answered {status}"),
            Self::Malformed(detail) => write!(f, "malformed: {detail}"),
        }
    }
}

impl core::error::Error for DiscoveryError {}

/// Discovers a collection's [`Capabilities`] in two requests: `OPTIONS`, then a `PROPFIND`.
///
/// Send [`Discovery::request`] and hand its response to [`Discovery::receive`] until there is
/// no request left; then [`Discovery::into_result`] holds the capabilities.
#[derive(Clone, Debug)]
pub struct Discovery {
    seam: CalDavSeam,
    state: State,
}

#[derive(Clone, Debug)]
enum State {
    Options(HttpRequest),
    Properties {
        request: HttpRequest,
        dav: Vec<String>,
    },
    Done(Result<Capabilities, DiscoveryError>),
}

impl Discovery {
    pub(crate) fn new(seam: &CalDavSeam) -> Self {
        let state = match seam.prepare_options() {
            Ok(request) => State::Options(request),
            Err(error) => State::Done(Err(DiscoveryError::Malformed(error))),
        };
        Self {
            seam: seam.clone(),
            state,
        }
    }

    /// The HTTP request to send next, or `None` once the result is known.
    #[must_use]
    pub fn request(&self) -> Option<&HttpRequest> {
        match &self.state {
            State::Options(request) | State::Properties { request, .. } => Some(request),
            State::Done(_) => None,
        }
    }

    /// Hands over what sending [`Discovery::request`] produced. Ignored once the result is
    /// known.
    pub fn receive(&mut self, response: Result<HttpResponse, TransportError>) {
        let state = core::mem::replace(
            &mut self.state,
            State::Done(Err(DiscoveryError::Malformed("abandoned".to_owned()))),
        );
        self.state = match (state, response) {
            (State::Done(result), _) => State::Done(result),
            (_, Err(error)) => State::Done(Err(DiscoveryError::Transport(error))),
            (State::Options(_), Ok(response)) => {
                if !response.is_success() {
                    State::Done(Err(DiscoveryError::Status(response.status)))
                } else {
                    let dav = dav_tokens(response.header("DAV").unwrap_or_default());
                    match self.properties_request() {
                        Ok(request) => State::Properties { request, dav },
                        Err(error) => State::Done(Err(DiscoveryError::Malformed(error))),
                    }
                }
            }
            (State::Properties { dav, .. }, Ok(response)) => {
                State::Done(capabilities(dav, &response))
            }
        };
    }

    /// The capabilities: an error where a request is still to be sent.
    ///
    /// # Errors
    ///
    /// The [`DiscoveryError`] that stopped discovery.
    pub fn into_result(self) -> Result<Capabilities, DiscoveryError> {
        match self.state {
            State::Done(result) => result,
            State::Options(_) | State::Properties { .. } => Err(DiscoveryError::Malformed(
                "discovery did not finish".to_owned(),
            )),
        }
    }

    fn properties_request(&self) -> Result<HttpRequest, String> {
        let properties = [
            &names::SUPPORTED_REPORT_SET,
            &names::SUPPORTED_CALENDAR_COMPONENT_SET,
            &names::SYNC_TOKEN,
        ];
        let propfind = Propfind::new(self.seam.collection())
            .with_properties(&properties)
            .with_depth(Depth::Zero);
        propfind
            .prepare_request(self.seam.base_url().clone())
            .map(|request| HttpRequest::from_http(&request))
            .map_err(|error| error.to_string())
    }
}

/// The tokens of a `DAV` header, lowercased.
fn dav_tokens(header: &str) -> Vec<String> {
    header
        .split(',')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

/// The capabilities a `PROPFIND` multistatus reports, with the `DAV` header's tokens.
fn capabilities(dav: Vec<String>, response: &HttpResponse) -> Result<Capabilities, DiscoveryError> {
    if !response.is_success() {
        return Err(DiscoveryError::Status(response.status));
    }
    let text = core::str::from_utf8(&response.body)
        .map_err(|error| DiscoveryError::Malformed(error.to_string()))?;
    let document = roxmltree::Document::parse(text)
        .map_err(|error| DiscoveryError::Malformed(error.to_string()))?;
    // Only properties in a propstat whose status is 200 are held; a 404 propstat lists those the
    // collection lacks.
    let held = document
        .descendants()
        .filter(|node| node.tag_name() == names::PROPSTAT)
        .filter(|propstat| {
            propstat
                .children()
                .find(|node| node.tag_name() == names::STATUS)
                .and_then(|status| status.text())
                .is_none_or(|status| status.split_whitespace().nth(1) == Some("200"))
        })
        .flat_map(|propstat| propstat.descendants())
        .collect::<Vec<_>>();
    let property = |name: &libdav::PropertyName<'_, '_>| {
        held.iter().find(|node| node.tag_name() == *name).copied()
    };
    let sync_token = property(&names::SYNC_TOKEN)
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    let reports_sync = property(&names::SUPPORTED_REPORT_SET).is_some_and(|set| {
        set.descendants()
            .any(|node| node.tag_name() == names::SYNC_COLLECTION)
    });
    let events = property(&names::SUPPORTED_CALENDAR_COMPONENT_SET).is_none_or(|set| {
        set.descendants().any(|node| {
            node.tag_name() == names::COMP
                && node
                    .attribute("name")
                    .is_some_and(|name| name.eq_ignore_ascii_case("VEVENT"))
        })
    });
    let has = |token: &str| dav.iter().any(|offered| offered == token);
    Ok(Capabilities {
        calendar_access: has("calendar-access"),
        auto_schedule: has("calendar-auto-schedule"),
        sync_collection: reports_sync || sync_token.is_some(),
        events,
        sync_token,
        dav,
    })
}

#[cfg(test)]
mod tests {
    use super::{Capabilities, Feature, capabilities, dav_tokens};
    use crate::http::HttpResponse;
    use crate::seam::Listing;

    const FULL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/cal/</D:href>
    <D:propstat>
      <D:prop>
        <D:supported-report-set>
          <D:supported-report><D:report><C:calendar-multiget/></D:report></D:supported-report>
          <D:supported-report><D:report><D:sync-collection/></D:report></D:supported-report>
        </D:supported-report-set>
        <C:supported-calendar-component-set>
          <C:comp name="VEVENT"/><C:comp name="VTODO"/>
        </C:supported-calendar-component-set>
        <D:sync-token>http://example.com/sync/7</D:sync-token>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

    const BARE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/cal/</D:href>
    <D:propstat>
      <D:prop>
        <C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
    <D:propstat>
      <D:prop><D:supported-report-set/><D:sync-token/></D:prop>
      <D:status>HTTP/1.1 404 Not Found</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

    #[test]
    fn a_full_server_lacks_nothing() {
        let dav = dav_tokens("1, 2, 3, calendar-access, Calendar-Auto-Schedule");
        let found = capabilities(dav, &HttpResponse::new(207, FULL)).unwrap();
        assert!(found.calendar_access && found.auto_schedule && found.sync_collection);
        assert!(found.events);
        assert_eq!(
            found.sync_token.as_deref(),
            Some("http://example.com/sync/7")
        );
        assert!(found.missing().is_empty());
        assert_eq!(found.listing(), Listing::SyncCollection);
    }

    #[test]
    fn a_bare_server_reports_what_it_lacks() {
        let found = capabilities(dav_tokens("1, 2"), &HttpResponse::new(207, BARE)).unwrap();
        assert_eq!(
            found.missing(),
            vec![
                Feature::CalendarAccess,
                Feature::SyncCollection,
                Feature::Events,
                Feature::AutoSchedule
            ]
        );
        assert_eq!(found.listing(), Listing::Etags);
    }

    #[test]
    fn no_component_set_means_every_component() {
        let body = FULL.replace(
            "<C:supported-calendar-component-set>\n          <C:comp name=\"VEVENT\"/><C:comp name=\"VTODO\"/>\n        </C:supported-calendar-component-set>",
            "",
        );
        let found = capabilities(Vec::new(), &HttpResponse::new(207, body)).unwrap();
        assert!(found.events);
    }

    #[test]
    fn a_failed_propfind_is_an_error() {
        assert!(capabilities(Vec::new(), &HttpResponse::new(403, "")).is_err());
        let found: Result<Capabilities, _> =
            capabilities(Vec::new(), &HttpResponse::new(207, "not xml"));
        assert!(found.is_err());
    }
}
