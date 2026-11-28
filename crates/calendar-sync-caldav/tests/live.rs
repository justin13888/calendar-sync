//! Live probes against a real `CalDAV` server: the `RustiCal` and Stalwart containers CI starts,
//! or an iCloud or Fastmail calendar.
//!
//! `#[ignore]`d, and a no-op unless `CALENDAR_SYNC_CALDAV_LIVE=1`; see the crate README for the
//! variables. Each probe in `probes/mod.rs` drives [`CalDavSeam`] over a transport that sends
//! with the `curl` binary, and records its exchanges with the testkit's [`Recorder`]; with
//! `CALENDAR_SYNC_CALDAV_RECORD` set to a directory, each probe's scrubbed fixture is written
//! there as `NAME.json`, to be reviewed and committed under `tests/fixtures/SERVER/`, where
//! `fixtures.rs` replays it.
//!
//! The probes create events in January 2030 under fixed hrefs, and delete them again; a run
//! interrupted part-way is cleared by the next.

mod probes;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use calendar_sync_caldav::{
    CalDavSeam, Client, HttpRequest, HttpResponse, Transport, TransportError,
};
use calendar_sync_model::Provider;
use calendar_sync_testkit::fixture::{self, Body};
use calendar_sync_testkit::recorder::Recorder;

/// The suite's configuration, read from the environment.
struct Config {
    url: String,
    collection: String,
    user: String,
    password: String,
    record: Option<PathBuf>,
}

// Reading the environment is how a live suite is configured.
#[allow(clippy::disallowed_methods)]
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn config() -> Option<Config> {
    if var("CALENDAR_SYNC_CALDAV_LIVE").as_deref() != Some("1") {
        eprintln!("skipped: set CALENDAR_SYNC_CALDAV_LIVE=1 to probe a CalDAV server");
        return None;
    }
    let field = |name: &str| var(name).unwrap_or_else(|| panic!("{name} is not set"));
    Some(Config {
        url: field("CALDAV_URL"),
        collection: field("CALDAV_COLLECTION"),
        user: field("CALDAV_USERNAME"),
        password: field("CALDAV_PASSWORD"),
        record: var("CALENDAR_SYNC_CALDAV_RECORD").map(PathBuf::from),
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

/// Sends with `curl`, with HTTP Basic credentials, and records every exchange.
struct Curl {
    user: String,
    password: String,
    recorder: Option<(String, Recorder)>,
    record: Option<PathBuf>,
}

impl Curl {
    /// Starts recording the probe `name`.
    fn begin(&mut self, name: &str) {
        self.recorder = Some((
            name.to_owned(),
            Recorder::new(Provider::CalDav, format!("Live probe: {name}")),
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
    std::fs::write(path, format!("{text}\n")).expect("fixture written");
}

fn body(bytes: &[u8]) -> Body {
    if bytes.is_empty() {
        Body::Empty
    } else {
        Body::Text(String::from_utf8_lossy(bytes).into_owned())
    }
}

impl Transport for Curl {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut config = format!(
            "url = {}\nrequest = {}\nsilent\nshow-error\ninclude\n",
            quoted(&request.url),
            quoted(&request.method)
        );
        config.push_str(&format!(
            "user = {}\n",
            quoted(&format!("{}:{}", self.user, self.password))
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
                    method: request.method.clone(),
                    url: request.path().to_owned(),
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

#[test]
#[ignore = "probes a live CalDAV server; set CALENDAR_SYNC_CALDAV_LIVE=1"]
fn live_probes() {
    let Some(config) = config() else {
        return;
    };
    let seam = CalDavSeam::new(
        &config.url,
        config.collection.clone(),
        probes::STAMP.parse().expect("stamp"),
    )
    .expect("CALDAV_URL");
    let mut client = Client::new(
        seam,
        Curl {
            user: config.user,
            password: config.password,
            recorder: None,
            record: config.record,
        },
    );
    for name in probes::PROBES {
        eprintln!("probe: {name}");
        client.transport_mut().begin(name);
        probes::run(name, &mut client);
        client.transport_mut().end();
    }
}
