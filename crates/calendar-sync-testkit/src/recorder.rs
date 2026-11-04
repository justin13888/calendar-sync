//! A recorder for live probes: it collects HTTP exchanges and turns them into a [`Fixture`] with
//! credentials and personal data scrubbed.
//!
//! [`Recorder::record`] keeps only the headers a replay needs ([`KEPT_HEADERS`]), so
//! `Authorization`, cookies and tracing headers never enter the fixture. [`Recorder::finish`]
//! then
//!
//! 1. replaces the free text of events with synthetic text: the JSON fields in
//!    [`FREE_TEXT_FIELDS`], the iCalendar properties in [`FREE_TEXT_PROPERTIES`], and every `CN`
//!    parameter. The same text always gets the same replacement, so a title that appears on two
//!    pages still matches;
//! 2. runs [`Fixture::redact`], which removes credentials and maps each email address to a stable
//!    `personN@example.com`;
//! 3. audits the result, and refuses to return a fixture the audit still flags.
//!
//! ```
//! use calendar_sync_testkit::fixture::{Body, Provider, Request, Response};
//! use calendar_sync_testkit::recorder::Recorder;
//!
//! let mut recorder = Recorder::new(Provider::Google, "One event");
//! recorder.record(
//!     Request {
//!         method: "GET".to_owned(),
//!         url: "/calendar/v3/calendars/primary/events".to_owned(),
//!         headers: [("Authorization".to_owned(), "Bearer ya29.secret".to_owned())].into(),
//!         body: Body::Empty,
//!     },
//!     Response {
//!         status: 200,
//!         headers: [("Content-Type".to_owned(), "application/json".to_owned())].into(),
//!         body: Body::Json(serde_json::json!({
//!             "items": [{ "id": "e1", "summary": "Dinner with Alex",
//!                         "organizer": { "email": "alex@gmail.com" } }],
//!             "nextSyncToken": "t1"
//!         })),
//!     },
//! );
//! let fixture = recorder.finish().unwrap();
//! let exchange = &fixture.exchanges[0];
//! assert!(exchange.request.headers.is_empty());
//! let Body::Json(body) = &exchange.response.body else { panic!() };
//! assert_eq!(body["items"][0]["summary"], "Synthetic text 1");
//! assert_eq!(body["items"][0]["organizer"]["email"], "person1@example.com");
//! ```

use std::collections::BTreeMap;

use serde_json::Value;

use crate::fixture::{
    Body, Exchange, FORMAT_VERSION, Fixture, FixtureError, Provider, Request, Response,
};

/// The headers a fixture keeps, lowercased. Every other header is dropped when recorded.
pub const KEPT_HEADERS: [&str; 9] = [
    "content-type",
    "etag",
    "if-match",
    "if-none-match",
    "location",
    "retry-after",
    "dav",
    "depth",
    "prefer",
];

/// JSON fields whose string values are free text a person wrote, replaced on
/// [`Recorder::finish`]: Google's and Graph's titles, descriptions, locations, names and
/// comments.
pub const FREE_TEXT_FIELDS: [&str; 10] = [
    "summary",
    "description",
    "location",
    "displayName",
    "comment",
    "subject",
    "bodyPreview",
    "content",
    "name",
    "label",
];

/// iCalendar properties whose values are free text, replaced on [`Recorder::finish`].
pub const FREE_TEXT_PROPERTIES: [&str; 5] =
    ["SUMMARY", "DESCRIPTION", "LOCATION", "COMMENT", "CONTACT"];

/// Collects the exchanges of one live probe.
#[derive(Clone, Debug)]
pub struct Recorder {
    provider: Provider,
    description: String,
    exchanges: Vec<Exchange>,
}

impl Recorder {
    /// A recorder for a probe of `provider` that does what `description` says.
    #[must_use]
    pub fn new(provider: Provider, description: impl Into<String>) -> Self {
        Self {
            provider,
            description: description.into(),
            exchanges: Vec::new(),
        }
    }

    /// Records one exchange, keeping only the headers in [`KEPT_HEADERS`], with lowercased
    /// names.
    pub fn record(&mut self, mut request: Request, mut response: Response) {
        request.headers = kept(request.headers);
        response.headers = kept(response.headers);
        self.exchanges.push(Exchange { request, response });
    }

    /// How many exchanges were recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.exchanges.len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }

    /// The fixture, with free text replaced, credentials removed and email addresses mapped to
    /// `example.com`.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Audit`] when anything the audit flags survives the scrubbing.
    pub fn finish(self) -> Result<Fixture, FixtureError> {
        let mut scrubber = Scrubber::default();
        let mut fixture = Fixture {
            format: FORMAT_VERSION,
            provider: self.provider,
            description: self.description,
            exchanges: self.exchanges,
        };
        for exchange in &mut fixture.exchanges {
            scrubber.body(&mut exchange.request.body);
            scrubber.body(&mut exchange.response.body);
        }
        fixture.redact();
        let findings = fixture.audit();
        if findings.is_empty() {
            Ok(fixture)
        } else {
            Err(FixtureError::Audit(findings))
        }
    }
}

fn kept(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .filter(|(name, _)| KEPT_HEADERS.contains(&name.as_str()))
        .collect()
}

/// Replaces free text with stable synthetic text.
#[derive(Default)]
struct Scrubber {
    texts: BTreeMap<String, String>,
}

impl Scrubber {
    fn replace(&mut self, original: &str) -> String {
        let next = self.texts.len() + 1;
        self.texts
            .entry(original.to_owned())
            .or_insert_with(|| format!("Synthetic text {next}"))
            .clone()
    }

    fn body(&mut self, body: &mut Body) {
        match body {
            Body::Empty => {}
            Body::Json(value) => self.json(value),
            Body::Text(text) => *text = self.text(text),
        }
    }

    fn json(&mut self, value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(|item| self.json(item)),
            Value::Object(fields) => {
                for (name, field) in fields.iter_mut() {
                    match field {
                        Value::String(text)
                            if FREE_TEXT_FIELDS.contains(&name.as_str()) && !text.is_empty() =>
                        {
                            *text = self.replace(text);
                        }
                        _ => self.json(field),
                    }
                }
            }
            Value::String(text) if text.contains("BEGIN:V") => *text = self.text(text),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// Scrubs iCalendar content wherever it appears in `text`: a whole object, or one inside a
    /// `CalDAV` multistatus. A folded property is unfolded when it is rewritten.
    fn text(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut logical: Vec<&str> = Vec::new();
        for line in text.split_inclusive('\n') {
            if line.starts_with([' ', '\t']) && !logical.is_empty() {
                logical.push(line);
            } else {
                self.flush(&logical, &mut out);
                logical = vec![line];
            }
        }
        self.flush(&logical, &mut out);
        out
    }

    fn flush(&mut self, physical: &[&str], out: &mut String) {
        let Some(first) = physical.first() else {
            return;
        };
        let ending = if first.ends_with("\r\n") {
            "\r\n"
        } else if first.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let mut unfolded = String::new();
        for (index, line) in physical.iter().enumerate() {
            let line = line.trim_end_matches(['\r', '\n']);
            unfolded.push_str(if index == 0 { line } else { &line[1..] });
        }
        let leading = unfolded.len() - unfolded.trim_start().len();
        let (indent, content) = unfolded.split_at(leading);
        let name_end = content.find([';', ':']).unwrap_or(content.len());
        let name = content[..name_end].to_ascii_uppercase();
        let target = FREE_TEXT_PROPERTIES.contains(&name.as_str());
        let has_cn = content.to_ascii_uppercase().contains(";CN=");
        if !target && !has_cn {
            physical.iter().for_each(|line| out.push_str(line));
            return;
        }
        let Some(colon) = value_start(content) else {
            physical.iter().for_each(|line| out.push_str(line));
            return;
        };
        let (head, value) = content.split_at(colon);
        let head = self.common_names(head);
        out.push_str(indent);
        out.push_str(&head);
        out.push(':');
        if target {
            out.push_str(&self.replace(&value[1..]));
        } else {
            out.push_str(&value[1..]);
        }
        out.push_str(ending);
    }

    /// Replaces the value of every `CN` parameter in a property's name and parameters.
    fn common_names(&mut self, head: &str) -> String {
        let mut out = String::with_capacity(head.len());
        let mut parts = split_params(head).into_iter();
        if let Some(name) = parts.next() {
            out.push_str(name);
        }
        for part in parts {
            out.push(';');
            match part.split_once('=') {
                Some((key, value)) if key.eq_ignore_ascii_case("CN") => {
                    let quoted = value.starts_with('"');
                    let replaced = self.replace(value.trim_matches('"'));
                    if quoted {
                        out.push_str(&format!("{key}=\"{replaced}\""));
                    } else {
                        out.push_str(&format!("{key}={replaced}"));
                    }
                }
                _ => out.push_str(part),
            }
        }
        out
    }
}

/// The index of the `:` that ends a content line's parameters, outside any quoted value.
fn value_start(line: &str) -> Option<usize> {
    let mut quoted = false;
    for (index, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(index),
            _ => {}
        }
    }
    None
}

/// A property's name and parameters, split on `;` outside quoted values.
fn split_params(head: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (index, c) in head.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&head[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&head[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Recorder;
    use crate::fixture::{Body, Provider, Request, Response};

    fn get(url: &str) -> Request {
        Request {
            method: "GET".to_owned(),
            url: url.to_owned(),
            headers: [
                (
                    "Authorization".to_owned(),
                    "Bearer ya29.a0secret".to_owned(),
                ),
                ("X-Goog-Request-Id".to_owned(), "trace".to_owned()),
                ("If-Match".to_owned(), "\"1\"".to_owned()),
            ]
            .into(),
            body: Body::Empty,
        }
    }

    #[test]
    fn json_free_text_is_replaced_stably_and_credentials_removed() {
        let mut recorder = Recorder::new(Provider::Google, "two pages");
        let page = |token: &str| Response {
            status: 200,
            headers: [("Set-Cookie".to_owned(), "SID=abc".to_owned())].into(),
            body: Body::Json(json!({
                "items": [{
                    "id": "e1",
                    "summary": "Dinner with Alex",
                    "location": "12 Main Street",
                    "attendees": [{ "email": "alex@gmail.com", "displayName": "Alex Smith", "comment": "ok" }],
                    "refresh_token": "1//secret"
                }],
                "nextPageToken": token
            })),
        };
        recorder.record(
            get("/calendar/v3/calendars/me%40gmail.com/events?access_token=ya29.x"),
            page("p1"),
        );
        recorder.record(
            get("/calendar/v3/calendars/me%40gmail.com/events"),
            page("p2"),
        );
        assert_eq!(recorder.len(), 2);
        let fixture = recorder.finish().expect("clean");
        assert!(fixture.audit().is_empty());
        let first = &fixture.exchanges[0];
        assert_eq!(
            first.request.headers.keys().collect::<Vec<_>>(),
            vec!["if-match"]
        );
        assert!(first.response.headers.is_empty());
        assert_eq!(
            first.request.url,
            "/calendar/v3/calendars/person1%40example.com/events?access_token=REDACTED"
        );
        let Body::Json(one) = &first.response.body else {
            panic!()
        };
        let Body::Json(two) = &fixture.exchanges[1].response.body else {
            panic!()
        };
        // Fields are visited in name order: the attendee's comment and name, the location, the
        // title. The second page's title is the first's, so it gets the same replacement.
        assert_eq!(
            one["items"][0]["attendees"][0]["comment"],
            "Synthetic text 1"
        );
        assert_eq!(
            one["items"][0]["attendees"][0]["displayName"],
            "Synthetic text 2"
        );
        assert_eq!(one["items"][0]["location"], "Synthetic text 3");
        assert_eq!(one["items"][0]["summary"], "Synthetic text 4");
        assert_eq!(two["items"][0]["summary"], "Synthetic text 4");
        assert_eq!(
            one["items"][0]["attendees"][0]["email"],
            "person2@example.com"
        );
        assert_eq!(one["items"][0]["refresh_token"], "REDACTED");
        // Ids and tokens are not free text.
        assert_eq!(one["items"][0]["id"], "e1");
        assert_eq!(one["nextPageToken"], "p1");
    }

    #[test]
    fn icalendar_free_text_and_common_names_are_replaced() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Dinner with\r\n  Alex\r\nATTENDEE;CN=\"Smith, Alex\";ROLE=REQ-PARTICIPANT:mailto:alex@gmail.com\r\nDTSTART:20261105T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut recorder = Recorder::new(Provider::CalDav, "one object");
        recorder.record(
            get("/cal/u1.ics"),
            Response {
                status: 200,
                headers: [("ETag".to_owned(), "\"7\"".to_owned())].into(),
                body: Body::Text(ics.to_owned()),
            },
        );
        let fixture = recorder.finish().expect("clean");
        let Body::Text(text) = &fixture.exchanges[0].response.body else {
            panic!()
        };
        assert_eq!(
            text,
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Synthetic text 1\r\nATTENDEE;CN=\"Synthetic text 2\";ROLE=REQ-PARTICIPANT:mailto:person1@example.com\r\nDTSTART:20261105T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        assert_eq!(fixture.exchanges[0].response.headers["etag"], "\"7\"");
    }
}
