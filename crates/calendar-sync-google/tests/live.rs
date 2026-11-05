//! Live probes against a real Google calendar.
//!
//! `#[ignore]`d, and a no-op unless `CALENDAR_SYNC_GOOGLE_LIVE=1`; see the crate README for the
//! variables. The weekly `scheduled.yml` workflow runs it. Each probe drives [`GoogleSeam`] over a
//! transport that sends with the `curl` binary, and records its exchanges with the testkit's
//! [`Recorder`]; with `CALENDAR_SYNC_GOOGLE_RECORD` set to a directory, each probe's scrubbed
//! fixture is written there, to be reviewed and committed beside the synthesized ones.
//!
//! The probes create events under fresh ids in the calendar given, and delete them again.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use calendar_sync_engine::{
    Change, Cursor, DerivedIds, EditId, Failure, ListFrom, Next, Outcome, Request, ResourceId,
};
use calendar_sync_google::{
    Client, GoogleSeam, HttpRequest, HttpResponse, Transport, TransportError, group,
};
use calendar_sync_model::{
    EventStatus, EventTimes, Frequency, NormalizedEvent, Override, Passthrough, Provider,
    ProviderEvent, ProviderIds, Recurrence, RecurrenceRule, RuleEnd, TimeValue, TimeZoneId,
};
use calendar_sync_testkit::fixture::{self, Body};
use calendar_sync_testkit::recorder::Recorder;
use jiff::civil::date;
use serde_json::{Value, json};

const ZONE: &str = "America/Toronto";

/// The suite's configuration, read from the environment.
struct Config {
    calendar_id: String,
    token: String,
    record: Option<PathBuf>,
}

// Reading the environment is how a live suite is configured.
#[allow(clippy::disallowed_methods)]
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn config() -> Option<Config> {
    if var("CALENDAR_SYNC_GOOGLE_LIVE").as_deref() != Some("1") {
        eprintln!("skipped: set CALENDAR_SYNC_GOOGLE_LIVE=1 to probe Google Calendar");
        return None;
    }
    let calendar_id = var("GOOGLE_CALENDAR_ID").expect("GOOGLE_CALENDAR_ID");
    let token = var("GOOGLE_CALENDAR_ACCESS_TOKEN").unwrap_or_else(refresh);
    Some(Config {
        calendar_id,
        token,
        record: var("CALENDAR_SYNC_GOOGLE_RECORD").map(PathBuf::from),
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

/// Exchanges the OAuth refresh token for an access token.
fn refresh() -> String {
    let field = |name: &str| var(name).unwrap_or_else(|| panic!("{name}"));
    let form = [
        ("client_id", field("GOOGLE_OAUTH_CLIENT_ID")),
        ("client_secret", field("GOOGLE_OAUTH_CLIENT_SECRET")),
        ("refresh_token", field("GOOGLE_OAUTH_REFRESH_TOKEN")),
        ("grant_type", "refresh_token".to_owned()),
    ];
    let mut config = String::from(
        "url = \"https://oauth2.googleapis.com/token\"\nsilent\nshow-error\ninclude\n",
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
            Recorder::new(Provider::Google, format!("Live probe: {description}")),
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

/// Lists a whole pass, returning its changes and the cursor the next pass starts from.
fn pass(client: &mut Client<Curl>, from: ListFrom) -> (Vec<Change>, Cursor) {
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
            other => panic!("list failed: {other:?}"),
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

/// A new event under the id the engine derives from `edit`.
fn new_event(edit: &str, title: &str, day: i8) -> (ResourceId, ProviderEvent) {
    let id = DerivedIds::of(&EditId::new(edit)).google_id();
    let mut fields =
        NormalizedEvent::new(EventTimes::new(at(day, 10), Some(at(day, 11))).expect("times"));
    fields.title = Some(title.to_owned());
    let event = ProviderEvent {
        ids: ProviderIds::new(Provider::Google, id.as_str()),
        revision: None,
        mapper_version: calendar_sync_google::MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    };
    (id, event)
}

/// A prefix unique to this run: Google keeps a deleted event's id, so ids are never reused.
// The run's start time is only a source of unique ids.
#[allow(clippy::disallowed_methods)]
fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_nanos();
    format!("live{nanos}")
}

#[test]
#[ignore = "probes a live Google calendar; see the crate README"]
fn google_calendar_live_probe() {
    let Some(config) = config() else {
        return;
    };
    let run = run_id();
    let mut client = Client::new(
        GoogleSeam::new(config.calendar_id).with_page_size(2),
        Curl {
            token: config.token,
            recorder: None,
            record: config.record,
        },
    );

    // A full pass pages until the sync token.
    client
        .transport_mut()
        .begin("initial-sync", "a full list, paged");
    let (_, cursor) = pass(&mut client, ListFrom::Start);
    client.transport_mut().end();

    // Inserts take the client-chosen id; updates and deletes are conditional.
    client.transport_mut().begin(
        "conditional-writes",
        "an insert under a client-chosen id, a conditional update, a stale one, and a repeated insert",
    );
    let (single, event) = new_event(&format!("{run}-single"), "Probe event", 7);
    let create = Request::Create {
        resource: single.clone(),
        event,
    };
    let created = written(client.perform(&create));
    assert_eq!(created.ids.id, single.as_str());
    let first_revision = created.revision.clone().expect("etag");
    let mut edited = created.clone();
    edited.fields.title = Some("Probe event, edited".to_owned());
    let update = Request::Update {
        resource: single.clone(),
        if_match: Some(first_revision),
        event: edited,
    };
    let updated = written(client.perform(&update));
    assert_eq!(updated.fields.title.as_deref(), Some("Probe event, edited"));
    assert_eq!(
        client.perform(&update),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    assert_eq!(
        client.perform(&create),
        Outcome::Failed(Failure::AlreadyExists)
    );
    client.transport_mut().end();

    // A field this crate does not map survives a read-modify-write.
    client.transport_mut().begin(
        "unknown-fields",
        "an unmapped field survives a read-modify-write",
    );
    let (kept, mut event) = new_event(&format!("{run}-kept"), "Probe passthrough", 8);
    event.passthrough.push(
        "extendedProperties",
        json!({ "private": { "calendarSyncProbe": "kept" } }).to_string(),
    );
    written(client.perform(&Request::Create {
        resource: kept.clone(),
        event,
    }));
    let mut current = read(&mut client, kept.as_str());
    current.fields.title = Some("Probe passthrough, edited".to_owned());
    written(client.perform(&Request::Update {
        resource: kept.clone(),
        if_match: current.revision.clone(),
        event: current,
    }));
    let after = read(&mut client, kept.as_str());
    let properties: Value = serde_json::from_str(
        after
            .passthrough
            .get("extendedProperties")
            .next()
            .expect("extendedProperties kept"),
    )
    .expect("json");
    assert_eq!(properties["private"]["calendarSyncProbe"], "kept");
    client.transport_mut().end();

    // An incremental pass reports the writes since the first pass.
    client
        .transport_mut()
        .begin("incremental-sync", "an incremental list after writes");
    let (changes, cursor) = pass(&mut client, ListFrom::Since(cursor));
    let changed: Vec<&str> = changes
        .iter()
        .filter_map(|change| match change {
            Change::Upsert { resource, .. } => Some(resource.as_str()),
            Change::Remove { .. } => None,
        })
        .collect();
    assert!(changed.contains(&single.as_str()), "{changed:?}");
    assert!(changed.contains(&kept.as_str()), "{changed:?}");
    client.transport_mut().end();

    // A series with one instance moved and one cancelled.
    client.transport_mut().begin(
        "recurring-exception",
        "a weekly series with one instance moved and one cancelled",
    );
    let (series_id, mut master) = new_event(&format!("{run}-series"), "Probe series", 14);
    let mut rule = RecurrenceRule::new(Frequency::Weekly);
    rule.end = RuleEnd::Count { count: 3 };
    master.fields.recurrence = Some(Recurrence::rule(rule));
    written(client.perform(&Request::Create {
        resource: series_id.clone(),
        event: master,
    }));
    // Google names an instance `MASTER_START`, its original start in UTC.
    let moved_id = format!("{}_20300121T150000Z", series_id.as_str());
    let mut moved = read(&mut client, &moved_id);
    assert_eq!(moved.ids.series_id.as_deref(), Some(series_id.as_str()));
    moved.fields.times = EventTimes::new(at(21, 12), Some(at(21, 13))).expect("times");
    written(client.perform(&Request::Update {
        resource: ResourceId::new(&moved_id),
        if_match: moved.revision.clone(),
        event: moved,
    }));
    let cancelled_id = format!("{}_20300128T150000Z", series_id.as_str());
    let mut cancelled = read(&mut client, &cancelled_id);
    cancelled.fields.status = EventStatus::Cancelled;
    written(client.perform(&Request::Update {
        resource: ResourceId::new(&cancelled_id),
        if_match: cancelled.revision.clone(),
        event: cancelled,
    }));
    let (changes, cursor) = pass(&mut client, ListFrom::Since(cursor));
    let events: Vec<ProviderEvent> = changes
        .into_iter()
        .filter_map(|change| match change {
            Change::Upsert {
                event: Some(event), ..
            } if event.ids.id.starts_with(series_id.as_str()) => Some(event),
            _ => None,
        })
        .collect();
    let grouped = group(events);
    let [series] = &grouped[..] else {
        panic!("{grouped:?}")
    };
    assert!(series.master.is_some());
    assert_eq!(series.overrides.len(), 2, "{series:?}");
    assert!(
        series
            .overrides
            .values()
            .any(|value| matches!(value, Override::Cancelled { .. }))
    );
    client.transport_mut().end();

    // A conditional delete, and the deletion in the next incremental pass.
    client.transport_mut().begin(
        "deletion",
        "a conditional delete, reported by the next incremental list",
    );
    let current = read(&mut client, single.as_str());
    assert_eq!(
        client.perform(&Request::Delete {
            resource: single.clone(),
            if_match: current.revision,
        }),
        Outcome::Deleted
    );
    let (changes, _) = pass(&mut client, ListFrom::Since(cursor));
    assert!(changes.contains(&Change::Remove {
        resource: single.clone()
    }));
    client.transport_mut().end();

    // Clean up, unconditionally.
    for resource in [kept, series_id] {
        let outcome = client.perform(&Request::Delete {
            resource: resource.clone(),
            if_match: None,
        });
        assert_eq!(
            outcome,
            Outcome::Deleted,
            "cleaning up {}",
            resource.as_str()
        );
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
