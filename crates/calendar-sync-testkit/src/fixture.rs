//! Recorded fixtures: the HTTP exchanges of a live probe, stored without credentials or personal
//! data, and replayed in order.
//!
//! # Format
//!
//! A fixture is one JSON document:
//!
//! ```json
//! {
//!   "format": 1,
//!   "provider": "google",
//!   "description": "An incremental list after one event changed",
//!   "exchanges": [
//!     {
//!       "request": { "method": "GET", "url": "/calendar/v3/calendars/primary/events?syncToken=t1" },
//!       "response": {
//!         "status": 200,
//!         "headers": { "content-type": "application/json" },
//!         "body": { "kind": "json", "content": { "items": [] } }
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! - `format` is [`FORMAT_VERSION`]. A reader refuses any other value.
//! - `provider` is a [`Provider`] in lowercase: `google`, `graph`, `caldav` or `ics`.
//! - Each exchange holds a request (`method`, `url` as sent, path and query, optional `headers`
//!   and `body`) and its response (`status`, optional `headers` and `body`).
//! - A `body` is `{"kind": "empty"}`, `{"kind": "json", "content": ...}` or
//!   `{"kind": "text", "content": "..."}`. An absent body is empty.
//! - Header names are compared without regard to case. Unknown fields are refused.
//!
//! # No credentials, no personal data
//!
//! [`Fixture::audit`] reports anything that looks like a credential or personal data:
//! authentication headers and cookies, credential query parameters and JSON fields, bearer and
//! basic credentials, Google access tokens, JSON Web Tokens, and email addresses outside the
//! reserved example domains (RFC 2606 and RFC 6761). [`Fixture::from_json`] refuses a fixture the
//! audit flags, so a fixture that leaks cannot be loaded. A recorder runs [`Fixture::redact`]
//! before writing, which removes or replaces every finding and maps each email address to a
//! stable `personN@example.com`.
//!
//! The audit is a deny-list. It cannot recognise a person's name or a street address in free
//! text, so recorded event content is replaced with synthetic text before it is committed.

use std::collections::BTreeMap;
use std::fmt;

pub use calendar_sync_model::Provider;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The fixture format this crate reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// What replaces a credential when a fixture is redacted.
pub const REDACTED: &str = "REDACTED";

/// Headers that carry credentials or session state. They are removed on redaction.
const CREDENTIAL_HEADERS: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-goog-api-key",
    "x-auth-token",
    "x-csrf-token",
];

/// Query parameters that carry credentials, in lowercase.
const CREDENTIAL_PARAMETERS: [&str; 12] = [
    "access_token",
    "refresh_token",
    "id_token",
    "client_secret",
    "code",
    "code_verifier",
    "key",
    "api_key",
    "apikey",
    "password",
    "sig",
    "signature",
];

/// JSON field names that carry credentials, lowercased with `_` and `-` removed.
const CREDENTIAL_FIELDS: [&str; 9] = [
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "clientsecret",
    "codeverifier",
    "password",
    "apikey",
    "secret",
    "authorization",
];

/// A recorded probe: the exchanges, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fixture {
    /// [`FORMAT_VERSION`].
    pub format: u32,
    /// The service the exchanges went to.
    pub provider: Provider,
    /// What the probe did, for a reader.
    pub description: String,
    /// The exchanges, in the order they happened.
    pub exchanges: Vec<Exchange>,
}

/// One request and its response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Exchange {
    /// The request.
    pub request: Request,
    /// The response.
    pub response: Response,
}

/// A recorded request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// The method, such as `GET` or `REPORT`.
    pub method: String,
    /// The path and query as sent.
    pub url: String,
    /// The headers that matter to the exchange.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// The body.
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
}

/// A recorded response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Response {
    /// The HTTP status.
    pub status: u16,
    /// The headers that matter to the exchange.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// The body.
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
}

/// A request or response body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "content", rename_all = "lowercase")]
pub enum Body {
    /// No body.
    #[default]
    Empty,
    /// A JSON body (Google, Graph).
    Json(Value),
    /// A text body (`CalDAV` XML, iCalendar).
    Text(String),
}

impl Body {
    /// Whether there is no body.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// What the audit found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingKind {
    /// A header that carries credentials or a session, such as `Authorization` or `Cookie`.
    CredentialHeader,
    /// A query parameter that carries a credential, such as `access_token` or `key`.
    CredentialParameter,
    /// A JSON field that carries a credential, such as `refresh_token`.
    CredentialField,
    /// Text that looks like a credential: a bearer or basic credential, a Google access token, or
    /// a JSON Web Token.
    CredentialValue,
    /// An email address outside the reserved example domains.
    EmailAddress,
}

/// One thing the audit flagged, and where.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Finding {
    /// Where it is, such as `exchanges[0].request.headers.authorization`.
    pub path: String,
    /// What it is.
    pub kind: FindingKind,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            FindingKind::CredentialHeader => "credential header",
            FindingKind::CredentialParameter => "credential query parameter",
            FindingKind::CredentialField => "credential field",
            FindingKind::CredentialValue => "credential value",
            FindingKind::EmailAddress => "email address",
        };
        write!(f, "{what} at {}", self.path)
    }
}

/// Why a fixture could not be read.
#[derive(Debug)]
pub enum FixtureError {
    /// The text is not a fixture.
    Json(serde_json::Error),
    /// The fixture is in a format this crate does not read.
    UnsupportedFormat(u32),
    /// The fixture carries credentials or personal data.
    Audit(Vec<Finding>),
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "not a fixture: {error}"),
            Self::UnsupportedFormat(format) => write!(
                f,
                "fixture format {format} is not supported; this crate reads format {FORMAT_VERSION}"
            ),
            Self::Audit(findings) => {
                write!(f, "fixture carries credentials or personal data:")?;
                for finding in findings {
                    write!(f, " {finding};")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for FixtureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedFormat(_) | Self::Audit(_) => None,
        }
    }
}

impl Fixture {
    /// Read a fixture, refusing one in another format or one the audit flags.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Json`] when the text is not a fixture, [`FixtureError::UnsupportedFormat`],
    /// and [`FixtureError::Audit`] with every finding.
    pub fn from_json(text: &str) -> Result<Self, FixtureError> {
        let fixture: Self = serde_json::from_str(text).map_err(FixtureError::Json)?;
        if fixture.format != FORMAT_VERSION {
            return Err(FixtureError::UnsupportedFormat(fixture.format));
        }
        let findings = fixture.audit();
        if findings.is_empty() {
            Ok(fixture)
        } else {
            Err(FixtureError::Audit(findings))
        }
    }

    /// Write the fixture as pretty-printed JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Json`] if serialization fails, which these types do not cause.
    pub fn to_json(&self) -> Result<String, FixtureError> {
        let mut text = serde_json::to_string_pretty(self).map_err(FixtureError::Json)?;
        text.push('\n');
        Ok(text)
    }

    /// Everything that looks like a credential or personal data, in document order.
    #[must_use]
    pub fn audit(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (index, exchange) in self.exchanges.iter().enumerate() {
            let request = format!("exchanges[{index}].request");
            let mut push = |path: String, kind| findings.push(Finding { path, kind });
            audit_url(&exchange.request.url, &format!("{request}.url"), &mut push);
            audit_headers(&exchange.request.headers, &request, &mut push);
            audit_body(
                &exchange.request.body,
                &format!("{request}.body"),
                &mut push,
            );
            let response = format!("exchanges[{index}].response");
            audit_headers(&exchange.response.headers, &response, &mut push);
            audit_body(
                &exchange.response.body,
                &format!("{response}.body"),
                &mut push,
            );
        }
        findings
    }

    /// Remove or replace everything [`Self::audit`] flags. Credential headers are removed,
    /// credential parameters, fields and values become [`REDACTED`], and each email address
    /// becomes `personN@example.com`, the same address always the same replacement.
    pub fn redact(&mut self) {
        let mut redactor = Redactor::default();
        for exchange in &mut self.exchanges {
            exchange.request.url = redactor.url(&exchange.request.url);
            redactor.headers(&mut exchange.request.headers);
            redactor.body(&mut exchange.request.body);
            redactor.headers(&mut exchange.response.headers);
            redactor.body(&mut exchange.response.body);
        }
    }

    /// Serve the recorded responses in order.
    #[must_use]
    pub fn replay(&self) -> Replay<'_> {
        Replay {
            fixture: self,
            next: 0,
        }
    }
}

/// Why a replay failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// A request arrived after every recorded exchange was served.
    Exhausted {
        /// The request's method.
        method: String,
        /// The request's URL.
        url: String,
    },
    /// The request is not the one recorded next.
    Mismatch {
        /// The exchange's index.
        index: usize,
        /// The recorded method and URL.
        expected: String,
        /// The request's method and URL.
        actual: String,
    },
    /// Recorded exchanges were never requested.
    Unused {
        /// How many.
        remaining: usize,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted { method, url } => {
                write!(f, "no recorded exchange left for {method} {url}")
            }
            Self::Mismatch {
                index,
                expected,
                actual,
            } => write!(f, "exchange {index}: expected {expected}, got {actual}"),
            Self::Unused { remaining } => {
                write!(f, "{remaining} recorded exchanges were never requested")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

/// A replay of a [`Fixture`], from [`Fixture::replay`].
#[derive(Clone, Debug)]
pub struct Replay<'a> {
    fixture: &'a Fixture,
    next: usize,
}

impl<'a> Replay<'a> {
    /// The recorded response to the next request, which must match the recorded method and URL.
    /// The method is compared without regard to case.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Exhausted`] and [`ReplayError::Mismatch`].
    pub fn respond(&mut self, method: &str, url: &str) -> Result<&'a Response, ReplayError> {
        let Some(exchange) = self.fixture.exchanges.get(self.next) else {
            return Err(ReplayError::Exhausted {
                method: method.to_owned(),
                url: url.to_owned(),
            });
        };
        let recorded = &exchange.request;
        if !recorded.method.eq_ignore_ascii_case(method) || recorded.url != url {
            return Err(ReplayError::Mismatch {
                index: self.next,
                expected: format!("{} {}", recorded.method, recorded.url),
                actual: format!("{method} {url}"),
            });
        }
        self.next += 1;
        Ok(&exchange.response)
    }

    /// How many recorded exchanges are left.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.fixture.exchanges.len() - self.next
    }

    /// Check every recorded exchange was requested.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Unused`].
    pub fn finish(self) -> Result<(), ReplayError> {
        match self.remaining() {
            0 => Ok(()),
            remaining => Err(ReplayError::Unused { remaining }),
        }
    }
}

fn is_credential_header(name: &str) -> bool {
    CREDENTIAL_HEADERS
        .iter()
        .any(|header| header.eq_ignore_ascii_case(name))
}

fn is_credential_parameter(name: &str) -> bool {
    CREDENTIAL_PARAMETERS
        .iter()
        .any(|parameter| parameter.eq_ignore_ascii_case(name))
}

fn is_credential_field(name: &str) -> bool {
    let folded: String = name
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    CREDENTIAL_FIELDS.contains(&folded.as_str())
}

fn audit_url(url: &str, path: &str, push: &mut impl FnMut(String, FindingKind)) {
    if let Some((_, query)) = url.split_once('?') {
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            if is_credential_parameter(name) && !value.is_empty() && value != REDACTED {
                push(format!("{path}?{name}"), FindingKind::CredentialParameter);
            }
        }
    }
    audit_text(&url.replace("%40", "@"), path, push);
}

fn audit_headers(
    headers: &BTreeMap<String, String>,
    parent: &str,
    push: &mut impl FnMut(String, FindingKind),
) {
    for (name, value) in headers {
        let path = format!("{parent}.headers.{}", name.to_ascii_lowercase());
        if is_credential_header(name) {
            push(path, FindingKind::CredentialHeader);
        } else {
            audit_text(value, &path, push);
        }
    }
}

fn audit_body(body: &Body, path: &str, push: &mut impl FnMut(String, FindingKind)) {
    match body {
        Body::Empty => {}
        Body::Json(value) => audit_json(value, path, push),
        Body::Text(text) => audit_text(text, path, push),
    }
}

/// Whether a credential field holds nothing secret: null, empty, or already redacted.
fn is_cleared(field: &Value) -> bool {
    match field {
        Value::Null => true,
        Value::String(text) => text.is_empty() || text == REDACTED,
        _ => false,
    }
}

fn audit_json(value: &Value, path: &str, push: &mut impl FnMut(String, FindingKind)) {
    match value {
        Value::String(text) => audit_text(text, path, push),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                audit_json(item, &format!("{path}[{index}]"), push);
            }
        }
        Value::Object(fields) => {
            for (name, field) in fields {
                let path = format!("{path}.{name}");
                if is_credential_field(name) && !is_cleared(field) {
                    push(path.clone(), FindingKind::CredentialField);
                }
                audit_text(name, &path, push);
                audit_json(field, &path, push);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn audit_text(text: &str, path: &str, push: &mut impl FnMut(String, FindingKind)) {
    if !credential_spans(text).is_empty() {
        push(path.to_owned(), FindingKind::CredentialValue);
    }
    if !email_spans(text).is_empty() {
        push(path.to_owned(), FindingKind::EmailAddress);
    }
}

/// Characters a token can hold.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-')
}

/// Byte ranges of words: maximal runs of token characters.
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start = None;
    for (index, c) in text.char_indices() {
        match (is_token_char(c), start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                words.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        words.push((begin, text.len()));
    }
    words
}

fn is_jwt(word: &str) -> bool {
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
        })
}

/// Byte ranges of credential values in `text`.
fn credential_spans(text: &str) -> Vec<(usize, usize)> {
    let words = words(text);
    let mut spans = Vec::new();
    for (index, &(start, end)) in words.iter().enumerate() {
        let word = &text[start..end];
        // A credential after an authentication scheme: `Bearer <token>`, `Basic <base64>`.
        let after_scheme = index > 0 && {
            let (previous_start, previous_end) = words[index - 1];
            let previous = &text[previous_start..previous_end];
            (previous.eq_ignore_ascii_case("bearer") || previous.eq_ignore_ascii_case("basic"))
                && text[previous_end..start].chars().all(|c| c == ' ')
        };
        if word.starts_with("ya29.") || is_jwt(word) || (after_scheme && word.len() >= 16) {
            spans.push((start, end));
        }
    }
    spans
}

const RESERVED_DOMAINS: [&str; 3] = ["example.com", "example.net", "example.org"];
const RESERVED_TLDS: [&str; 4] = ["example", "test", "invalid", "localhost"];

fn is_reserved_domain(domain: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    RESERVED_DOMAINS
        .iter()
        .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")))
        || domain
            .rsplit('.')
            .next()
            .is_some_and(|tld| RESERVED_TLDS.contains(&tld))
}

fn is_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_domain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
}

/// Byte ranges of email addresses outside the reserved domains.
fn email_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (at, _) in text.match_indices('@') {
        let start = text[..at]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_local_char(*c))
            .last()
            .map_or(at, |(index, _)| index);
        let domain_end = text[at + 1..]
            .char_indices()
            .find(|(_, c)| !is_domain_char(*c))
            .map_or(text.len(), |(index, _)| at + 1 + index);
        let domain = text[at + 1..domain_end].trim_end_matches(['.', '-']);
        let end = at + 1 + domain.len();
        let labels_ok = domain.contains('.') && domain.split('.').all(|label| !label.is_empty());
        if start < at && labels_ok && !is_reserved_domain(domain) {
            spans.push((start, end));
        }
    }
    spans
}

#[derive(Default)]
struct Redactor {
    emails: BTreeMap<String, String>,
}

impl Redactor {
    fn email(&mut self, address: &str) -> String {
        let next = self.emails.len() + 1;
        self.emails
            .entry(address.to_ascii_lowercase())
            .or_insert_with(|| format!("person{next}@example.com"))
            .clone()
    }

    fn text(&mut self, text: &str) -> String {
        let mut spans: Vec<(usize, usize, Option<String>)> = credential_spans(text)
            .into_iter()
            .map(|(start, end)| (start, end, None))
            .collect();
        for (start, end) in email_spans(text) {
            if !spans.iter().any(|(s, e, _)| start < *e && *s < end) {
                spans.push((start, end, Some(self.email(&text[start..end]))));
            }
        }
        spans.sort_unstable_by_key(|(start, _, _)| *start);
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end, replacement) in spans {
            if start < cursor {
                continue;
            }
            out.push_str(&text[cursor..start]);
            out.push_str(replacement.as_deref().unwrap_or(REDACTED));
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        out
    }

    fn url(&mut self, url: &str) -> String {
        let url = url.replace("%40", "@");
        let (path, query) = match url.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (url.as_str(), None),
        };
        let mut out = self.text(path);
        if let Some(query) = query {
            let pairs: Vec<String> = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) if is_credential_parameter(name) => {
                        format!("{name}={REDACTED}")
                    }
                    Some((name, value)) => format!("{}={}", self.text(name), self.text(value)),
                    None => self.text(pair),
                })
                .collect();
            out.push('?');
            out.push_str(&pairs.join("&"));
        }
        out
    }

    fn headers(&mut self, headers: &mut BTreeMap<String, String>) {
        headers.retain(|name, _| !is_credential_header(name));
        for value in headers.values_mut() {
            *value = self.text(value);
        }
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
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.json(item)),
            Value::Object(fields) => {
                let old = std::mem::take(fields);
                for (name, mut field) in old {
                    if is_credential_field(&name) && !field.is_null() {
                        field = Value::String(REDACTED.to_owned());
                    } else {
                        self.json(&mut field);
                    }
                    fields.insert(self.text(&name), field);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        Body, Exchange, FindingKind, Fixture, FixtureError, Provider, ReplayError, Request,
        Response, credential_spans, email_spans,
    };

    fn exchange(url: &str, body: Body) -> Exchange {
        Exchange {
            request: Request {
                method: "GET".to_owned(),
                url: url.to_owned(),
                headers: [(
                    "Authorization".to_owned(),
                    "Bearer ya29.a0AfH6SMBx".to_owned(),
                )]
                .into(),
                body: Body::Empty,
            },
            response: Response {
                status: 200,
                headers: [("Set-Cookie".to_owned(), "SID=abc".to_owned())].into(),
                body,
            },
        }
    }

    fn leaky() -> Fixture {
        Fixture {
            format: 1,
            provider: Provider::Google,
            description: "leaky".to_owned(),
            exchanges: vec![exchange(
                "/calendar/v3/calendars/alice%40gmail.com/events?key=AIzaSy123&syncToken=t",
                Body::Json(json!({
                    "refresh_token": "1//0abc",
                    "organizer": { "email": "alice@gmail.com" },
                    "attendees": [{ "email": "bob@contoso.com" }, { "email": "ALICE@gmail.com" }],
                    "description": "Join with Bearer abcdefghijklmnopqrstuvwxyz",
                    "jwt": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
                    "safe": "carol@example.com",
                })),
            )],
        }
    }

    #[test]
    fn audit_flags_each_leak() {
        let kinds: Vec<FindingKind> = leaky().audit().into_iter().map(|f| f.kind).collect();
        for kind in [
            FindingKind::CredentialHeader,
            FindingKind::CredentialParameter,
            FindingKind::CredentialField,
            FindingKind::CredentialValue,
            FindingKind::EmailAddress,
        ] {
            assert!(kinds.contains(&kind), "{kind:?} not flagged in {kinds:?}");
        }
    }

    #[test]
    fn redaction_clears_the_audit_and_keeps_correlation() {
        let mut fixture = leaky();
        fixture.redact();
        assert_eq!(fixture.audit(), []);
        let exchange = &fixture.exchanges[0];
        assert!(exchange.request.headers.is_empty());
        assert!(exchange.response.headers.is_empty());
        assert_eq!(
            exchange.request.url,
            "/calendar/v3/calendars/person1@example.com/events?key=REDACTED&syncToken=t"
        );
        let Body::Json(body) = &exchange.response.body else {
            panic!("json body");
        };
        assert_eq!(body["refresh_token"], "REDACTED");
        assert_eq!(body["organizer"]["email"], "person1@example.com");
        assert_eq!(body["attendees"][0]["email"], "person2@example.com");
        assert_eq!(body["attendees"][1]["email"], "person1@example.com");
        assert_eq!(body["description"], "Join with Bearer REDACTED");
        assert_eq!(body["jwt"], "REDACTED");
        assert_eq!(body["safe"], "carol@example.com");
        // A redacted fixture reads back.
        let text = fixture.to_json().unwrap();
        assert_eq!(Fixture::from_json(&text).unwrap(), fixture);
    }

    #[test]
    fn from_json_refuses_a_leaky_fixture_and_another_format() {
        let text = leaky().to_json().unwrap();
        assert!(matches!(
            Fixture::from_json(&text),
            Err(FixtureError::Audit(findings)) if !findings.is_empty()
        ));
        let mut other = leaky();
        other.redact();
        other.format = 2;
        assert!(matches!(
            Fixture::from_json(&other.to_json().unwrap()),
            Err(FixtureError::UnsupportedFormat(2))
        ));
        assert!(matches!(
            Fixture::from_json(
                r#"{"format":1,"provider":"google","description":"","exchanges":[],"extra":1}"#
            ),
            Err(FixtureError::Json(_))
        ));
    }

    #[test]
    fn detectors_leave_ordinary_text_alone() {
        for text in [
            "@odata.nextLink",
            "Bearer realm=\"example\"",
            "user@localhost.test",
            "team@sub.example.org",
            "a @ b.com",
            "eyJ only one part",
            "BEGIN:VCALENDAR",
        ] {
            assert_eq!(email_spans(text), [], "{text}");
            assert_eq!(credential_spans(text), [], "{text}");
        }
        assert_eq!(email_spans("mailto:dana@contoso.com."), [(7, 23)]);
    }

    #[test]
    fn replay_serves_in_order_and_checks_requests() {
        let mut fixture = leaky();
        fixture.redact();
        fixture.exchanges.push(fixture.exchanges[0].clone());
        let url = fixture.exchanges[0].request.url.clone();
        let mut replay = fixture.replay();
        assert_eq!(replay.respond("get", &url).unwrap().status, 200);
        assert!(matches!(
            replay.respond("GET", "/other"),
            Err(ReplayError::Mismatch { index: 1, .. })
        ));
        assert!(matches!(
            replay.clone().finish(),
            Err(ReplayError::Unused { remaining: 1 })
        ));
        replay.respond("GET", &url).unwrap();
        assert!(matches!(
            replay.respond("GET", &url),
            Err(ReplayError::Exhausted { .. })
        ));
        replay.finish().unwrap();
    }
}
