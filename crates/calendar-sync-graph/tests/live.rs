//! Live probes against a real Microsoft Graph mailbox.
//!
//! `#[ignore]`d, and a no-op unless `CALENDAR_SYNC_GRAPH_LIVE=1`; see the crate README for the
//! variables. The weekly `scheduled.yml` workflow runs it. Each probe drives [`GraphSeam`] over a
//! transport that sends with the `curl` binary, and records its exchanges with the testkit's
//! [`Recorder`]; with `CALENDAR_SYNC_GRAPH_RECORD` set to a directory, each probe's scrubbed
//! fixture is written there, to be reviewed and committed beside the synthesized ones.
//!
//! The probes create events in January 2030, inside the window they sync, and delete them again.
//! Two questions the conformance spike owns are reported rather than asserted: what Graph does
//! with a stale `If-Match`, and whether a create repeated under one `transactionId` makes a
//! second event.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use calendar_sync_engine::{
    Change, Cursor, DerivedIds, EditId, Failure, ListFrom, Next, Outcome, Request, ResourceId,
};
use calendar_sync_graph::{
    Client, GraphSeam, HttpRequest, HttpResponse, Precondition, Transport, TransportError, Window,
    group,
};
use calendar_sync_model::{
    EventTimes, Frequency, NormalizedEvent, Passthrough, Provider, ProviderEvent, ProviderIds,
    Recurrence, RecurrenceRule, RuleEnd, TimeValue, TimeZoneId,
};
use calendar_sync_testkit::fixture::{self, Body};
use calendar_sync_testkit::recorder::Recorder;
use jiff::civil::date;
use serde_json::Value;

const ZONE: &str = "America/Toronto";

/// The suite's configuration, read from the environment.
struct Config {
    calendar_id: Option<String>,
    token: String,
    record: Option<PathBuf>,
}

// Reading the environment is how a live suite is configured.
#[allow(clippy::disallowed_methods)]
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn config() -> Option<Config> {
    if var("CALENDAR_SYNC_GRAPH_LIVE").as_deref() != Some("1") {
        eprintln!("skipped: set CALENDAR_SYNC_GRAPH_LIVE=1 to probe Microsoft Graph");
        return None;
    }
    let token = var("GRAPH_ACCESS_TOKEN").unwrap_or_else(refresh);
    Some(Config {
        calendar_id: var("GRAPH_CALENDAR_ID"),
        token,
        record: var("CALENDAR_SYNC_GRAPH_RECORD").map(PathBuf::from),
    })
}

/// Escapes `text` as a double-quoted `curl` config value.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Runs `curl` with `config` on standard input, so no credential appears in a process list.
/// Returns the status, headers and body of the final response.
fn curl(config: &str) -> Result<HttpResponse, TransportError> {
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| TransportError::new(format!("curl: {error}")))?;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(config.as_bytes())
        .map_err(|error| TransportError::new(format!("curl stdin: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| TransportError::new(format!("curl: {error}")))?;
    if !output.status.success() {
        return Err(TransportError::new(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    parse(&output.stdout)
}

/// Splits `curl --include` output into the last response's status, headers and body.
fn parse(mut raw: &[u8]) -> Result<HttpResponse, TransportError> {
    loop {
        let end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| TransportError::new("no header block"))?;
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        raw = &raw[end + 4..];
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| TransportError::new(format!("no status in {head:?}")))?;
        if (100..200).contains(&status) {
            continue;
        }
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
            .collect();
        return Ok(HttpResponse {
            status,
            headers,
            body: raw.to_vec(),
        });
    }
}

/// Exchanges the OAuth refresh token for an access token at the Microsoft identity platform.
fn refresh() -> String {
    let field = |name: &str| var(name).unwrap_or_else(|| panic!("{name}"));
    let tenant = var("GRAPH_TENANT").unwrap_or_else(|| "common".to_owned());
    let mut form = vec![
        ("client_id", field("GRAPH_CLIENT_ID")),
        ("refresh_token", field("GRAPH_REFRESH_TOKEN")),
        ("grant_type", "refresh_token".to_owned()),
        (
            "scope",
            "https://graph.microsoft.com/Calendars.ReadWrite offline_access".to_owned(),
        ),
    ];
    if let Some(secret) = var("GRAPH_CLIENT_SECRET") {
        form.push(("client_secret", secret));
    }
    let mut config = format!(
        "url = {}\nsilent\nshow-error\ninclude\n",
        quoted(&format!(
            "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
        ))
    );
    for (name, value) in form {
        config.push_str(&format!(
            "data-urlencode = {}\n",
            quoted(&format!("{name}={value}"))
        ));
    }
    let response = curl(&config).expect("token request");
    let body: Value = serde_json::from_slice(&response.body).expect("token response");
    assert_eq!(response.status, 200, "token refresh failed: {body}");
    body["access_token"]
        .as_str()
        .expect("access_token")
        .to_owned()
}

/// Sends with `curl`, adding the bearer token, and records every exchange.
struct Curl {
    token: String,
    recorder: Option<(String, Recorder)>,
    record: Option<PathBuf>,
}

impl Curl {
    /// Starts recording the probe `name`.
    fn begin(&mut self, name: &str, description: &str) {
        self.recorder = Some((
            name.to_owned(),
            Recorder::new(Provider::Graph, format!("Live probe: {description}")),
        ));
    }

    /// Ends the probe, writing its scrubbed fixture when recording.
    fn end(&mut self) {
        let Some((name, recorder)) = self.recorder.take() else {
            return;
        };
        let fixture = recorder.finish().expect("the scrubbed fixture is clean");
        if let Some(dir) = &self.record {
            write(
                &dir.join(format!("{name}.json")),
                &fixture.to_json().expect("json"),
            );
        }
    }
}

// Writing a recorded fixture is the point of recording.
#[allow(clippy::disallowed_methods)]
fn write(path: &std::path::Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("record directory");
    }
    std::fs::write(path, text).expect("fixture written");
}

fn body(bytes: &[u8]) -> Body {
    if bytes.is_empty() {
        Body::Empty
    } else if let Ok(value) = serde_json::from_slice(bytes) {
        Body::Json(value)
    } else {
        Body::Text(String::from_utf8_lossy(bytes).into_owned())
    }
}

impl Transport for Curl {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut config = format!(
            "url = {}\nrequest = {}\nsilent\nshow-error\ninclude\n",
            quoted(&request.url()),
            quoted(request.method.as_str())
        );
        config.push_str(&format!(
            "header = {}\n",
            quoted(&format!("Authorization: Bearer {}", self.token))
        ));
        for (name, value) in &request.headers {
            config.push_str(&format!(
                "header = {}\n",
                quoted(&format!("{name}: {value}"))
            ));
        }
        if let Some(body) = &request.body {
            config.push_str(&format!("data-binary = {}\n", quoted(body)));
        }
        let response = curl(&config)?;
        if let Some((_, recorder)) = &mut self.recorder {
            recorder.record(
                fixture::Request {
                    method: request.method.as_str().to_owned(),
                    url: request.path.clone(),
                    headers: request.headers.iter().cloned().collect::<BTreeMap<_, _>>(),
                    body: request
                        .body
                        .as_deref()
                        .map_or(Body::Empty, |text| body(text.as_bytes())),
                },
                fixture::Response {
                    status: response.status,
                    headers: response.headers.iter().cloned().collect(),
                    body: body(&response.body),
                },
            );
        }
        Ok(response)
    }
}

/// Lists a whole delta round, returning its changes and the link the next round starts from.
fn round(client: &mut Client<Curl>, from: ListFrom) -> (Vec<Change>, Cursor) {
    let mut changes = Vec::new();
    let mut request = Request::List { from };
    loop {
        match client.perform(&request) {
            Outcome::Listed(page) => {
                changes.extend(page.changes);
                match page.next {
                    Next::More(cursor) => {
                        request = Request::List {
                            from: ListFrom::Continue(cursor),
                        }
                    }
                    Next::Done(cursor) => return (changes, cursor),
                }
            }
            other => panic!("delta failed: {other:?}"),
        }
    }
}

fn written(outcome: Outcome) -> ProviderEvent {
    match outcome {
        Outcome::Written(event) => event,
        other => panic!("not written: {other:?}"),
    }
}

fn read(client: &mut Client<Curl>, id: &str) -> ProviderEvent {
    match client.perform(&Request::Read {
        resource: ResourceId::new(id),
    }) {
        Outcome::Read(Some(event)) => event,
        other => panic!("{id} not read: {other:?}"),
    }
}

fn at(day: i8, hour: i8) -> TimeValue {
    TimeValue::zoned(date(2030, 1, day).at(hour, 0, 0, 0), TimeZoneId::new(ZONE))
}

/// A new event, created under the idempotency key the engine derives from `edit`.
fn new_event(edit: &str, title: &str, day: i8) -> (ResourceId, ProviderEvent) {
    let key = DerivedIds::of(&EditId::new(edit)).idempotency_key();
    let mut fields =
        NormalizedEvent::new(EventTimes::new(at(day, 10), Some(at(day, 11))).expect("times"));
    fields.title = Some(title.to_owned());
    fields.description = Some("Kept across a PATCH of the subject".to_owned());
    let event = ProviderEvent {
        ids: ProviderIds::new(Provider::Graph, key.as_str()),
        revision: None,
        mapper_version: calendar_sync_graph::MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    };
    (key, event)
}

/// A prefix unique to this run, so a key from an earlier run is never looked up again.
// The run's start time is only a source of unique keys.
#[allow(clippy::disallowed_methods)]
fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_nanos();
    format!("live{nanos}")
}

#[test]
#[ignore = "probes a live Microsoft Graph mailbox; see the crate README"]
fn microsoft_graph_live_probe() {
    let Some(config) = config() else {
        return;
    };
    let run = run_id();
    let mut seam = GraphSeam::new(Window {
        start: "2030-01-01T00:00:00Z".parse().expect("start"),
        end: "2030-02-01T00:00:00Z".parse().expect("end"),
    })
    .with_page_size(5);
    if let Some(calendar) = &config.calendar_id {
        seam = seam.with_calendar(calendar);
    }
    let mut client = Client::new(
        seam.clone(),
        Curl {
            token: config.token,
            recorder: None,
            record: config.record,
        },
    );
    let mut created = Vec::new();

    // A first round pages until the delta link.
    client
        .transport_mut()
        .begin("initial-sync", "a first delta round, paged");
    let (_, cursor) = round(&mut client, ListFrom::Start);
    client.transport_mut().end();

    // A create is found again by its key.
    client.transport_mut().begin(
        "create-and-lookup",
        "a lookup, a create under a transactionId and an extended property, and a lookup again",
    );
    let (key, event) = new_event(&format!("{run}-single"), "Probe event", 7);
    let lookup = Request::Lookup { key: key.clone() };
    assert_eq!(client.perform(&lookup), Outcome::Read(None));
    let create = Request::Create {
        resource: key.clone(),
        event,
    };
    let made = written(client.perform(&create));
    created.push(made.ids.id.clone());
    let Outcome::Read(Some(found)) = client.perform(&lookup) else {
        panic!("the create is not found by its key")
    };
    assert_eq!(found.ids.id, made.ids.id);
    client.transport_mut().end();

    // Spike: a create repeated under the same transactionId.
    let again = written(client.perform(&create));
    if again.ids.id == made.ids.id {
        eprintln!("transactionId: a repeated create returned the first event");
    } else {
        eprintln!("transactionId: a repeated create made a second event");
        created.push(again.ids.id.clone());
    }

    // Updates read, compare the changeKey and patch only what changed.
    client.transport_mut().begin(
        "conditional-writes",
        "a read-compare update, a stale one, and a read-compare delete",
    );
    let resource = ResourceId::new(&made.ids.id);
    let mut edited = made.clone();
    edited.fields.title = Some("Probe event, edited".to_owned());
    let update = Request::Update {
        resource: resource.clone(),
        if_match: made.revision.clone(),
        event: edited,
    };
    let updated = written(client.perform(&update));
    assert_eq!(updated.fields.title.as_deref(), Some("Probe event, edited"));
    assert_eq!(
        updated.fields.description.as_deref(),
        Some("Kept across a PATCH of the subject")
    );
    assert_eq!(
        client.perform(&update),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    client.transport_mut().end();

    // Spike: what Graph does with a stale If-Match, sent alone.
    let mut if_match = Client::new(
        seam.clone().with_precondition(Precondition::IfMatch),
        client.transport_mut(),
    );
    let mut stale = updated.clone();
    stale.fields.title = Some("Probe event, stale".to_owned());
    let outcome = if_match.perform(&Request::Update {
        resource: resource.clone(),
        if_match: made.revision.clone(),
        event: stale,
    });
    eprintln!("If-Match: a stale changeKey sent alone gave {outcome:?}");

    // An incremental round reports the writes.
    client
        .transport_mut()
        .begin("incremental-sync", "a delta round after writes");
    let (changes, cursor) = round(&mut client, ListFrom::Since(cursor));
    assert!(
        changes.iter().any(|change| matches!(
            change,
            Change::Upsert { resource: changed, .. } if *changed == resource
        )),
        "{changes:?}"
    );
    client.transport_mut().end();

    // A weekly series: what delta returns for it, grouped.
    client.transport_mut().begin(
        "recurring-series",
        "a weekly series of three, as a delta round returns it",
    );
    let (series_key, mut master) = new_event(&format!("{run}-series"), "Probe series", 14);
    let mut rule = RecurrenceRule::new(Frequency::Weekly);
    rule.end = RuleEnd::Count { count: 3 };
    master.fields.recurrence = Some(Recurrence::rule(rule));
    let series_made = written(client.perform(&Request::Create {
        resource: series_key,
        event: master,
    }));
    created.push(series_made.ids.id.clone());
    let (changes, cursor) = round(&mut client, ListFrom::Since(cursor));
    let events: Vec<ProviderEvent> = changes
        .into_iter()
        .filter_map(|change| match change {
            Change::Upsert {
                event: Some(event), ..
            } => Some(event),
            _ => None,
        })
        .filter(|event| {
            event.ids.id == series_made.ids.id
                || event.ids.series_id.as_deref() == Some(series_made.ids.id.as_str())
        })
        .collect();
    eprintln!(
        "series: delta returned {} events for it, master included: {}",
        events.len(),
        events
            .iter()
            .any(|event| event.ids.id == series_made.ids.id)
    );
    assert!(!group(events).is_empty());
    client.transport_mut().end();

    // A delete, and the removal the next round confirms.
    client.transport_mut().begin(
        "deletion",
        "a read-compare delete, confirmed as a removal by the next delta round",
    );
    let current = read(&mut client, resource.as_str());
    assert_eq!(
        client.perform(&Request::Delete {
            resource: resource.clone(),
            if_match: current.revision,
        }),
        Outcome::Deleted
    );
    created.retain(|id| *id != made.ids.id);
    let (changes, _) = round(&mut client, ListFrom::Since(cursor));
    assert!(
        changes.contains(&Change::Remove {
            resource: resource.clone()
        }),
        "{changes:?}"
    );
    client.transport_mut().end();

    // Clean up, unconditionally.
    for id in created {
        let outcome = client.perform(&Request::Delete {
            resource: ResourceId::new(&id),
            if_match: None,
        });
        assert_eq!(outcome, Outcome::Deleted, "cleaning up {id}");
    }
}

#[test]
fn curl_output_is_split_into_status_headers_and_body() {
    let response = parse(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 412\r\ncontent-type: application/json\r\n\r\n{\"error\":{}}",
    )
    .unwrap();
    assert_eq!(response.status, 412);
    assert_eq!(response.header("Content-Type"), Some("application/json"));
    assert_eq!(response.body, b"{\"error\":{}}");
    assert_eq!(quoted("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
}
