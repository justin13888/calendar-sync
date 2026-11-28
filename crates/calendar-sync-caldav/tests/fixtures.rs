//! The probes against what they recorded: every fixture under `tests/fixtures/SERVER/` was
//! recorded by `live.rs` from a real server (`RustiCal` and Stalwart, in containers), scrubbed by
//! the testkit's recorder, and replays through [`Client`] with every request the recording made,
//! in order, carrying the recorded headers.

mod probes;

use std::path::{Path, PathBuf};

use calendar_sync_caldav::{
    CalDavSeam, Client, Feature, HttpRequest, HttpResponse, Transport, TransportError,
};
use calendar_sync_testkit::fixture::{Body, Fixture, Replay};

/// Each recorded server, and the features it lacks.
const SERVERS: [(&str, &[Feature]); 2] =
    [("rustical", &[Feature::AutoSchedule]), ("stalwart", &[])];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// Reading the fixture directory is the point of this test.
#[allow(clippy::disallowed_methods)]
fn load(server: &str, probe: &str) -> Fixture {
    let path = dir().join(server).join(format!("{probe}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    Fixture::from_json(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A transport that answers from a fixture, and checks each call carries the recorded headers
/// and, for a report or a `PROPFIND`, the recorded body. A `PUT` body is not compared: the
/// recording replaced its free text.
struct Replayer<'a> {
    fixture: &'a Fixture,
    replay: Replay<'a>,
}

impl Transport for Replayer<'_> {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let index = self.fixture.exchanges.len() - self.replay.remaining();
        let exchange = self.fixture.exchanges.get(index);
        let response = self
            .replay
            .respond(&request.method, request.path())
            .unwrap_or_else(|error| panic!("{error}"));
        if let Some(recorded) = exchange {
            for (name, value) in &recorded.request.headers {
                assert_eq!(
                    request.header(name),
                    Some(value.as_str()),
                    "{} {}: header {name}",
                    request.method,
                    request.path()
                );
            }
            if recorded.request.method != "PUT" {
                let expected = match &recorded.request.body {
                    Body::Text(text) => Some(text.as_str()),
                    Body::Empty | Body::Json(_) => None,
                };
                assert_eq!(
                    request.body.as_deref(),
                    expected,
                    "{} {}: body",
                    request.method,
                    request.path()
                );
            }
        }
        Ok(HttpResponse {
            status: response.status,
            headers: response
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            body: match &response.body {
                Body::Empty => Vec::new(),
                Body::Json(value) => value.to_string().into_bytes(),
                Body::Text(text) => text.clone().into_bytes(),
            },
        })
    }
}

fn replaying(fixture: &Fixture) -> Client<Replayer<'_>> {
    // Every probe starts with discovery, whose OPTIONS request names the collection.
    let collection = fixture
        .exchanges
        .first()
        .map(|exchange| exchange.request.url.clone())
        .expect("a recorded exchange");
    let seam = CalDavSeam::new(
        "https://replay.example.com",
        collection,
        probes::STAMP.parse().expect("stamp"),
    )
    .expect("seam");
    Client::new(
        seam,
        Replayer {
            fixture,
            replay: fixture.replay(),
        },
    )
}

#[test]
fn every_probe_replays_against_every_recorded_server() {
    for (server, _) in SERVERS {
        for probe in probes::PROBES {
            let fixture = load(server, probe);
            let mut client = replaying(&fixture);
            probes::run(probe, &mut client);
            client
                .into_transport()
                .replay
                .finish()
                .unwrap_or_else(|error| panic!("{server}/{probe}: {error}"));
        }
    }
}

#[test]
fn each_server_reports_what_it_lacks() {
    for (server, lacks) in SERVERS {
        let fixture = load(server, "discover");
        let mut client = replaying(&fixture);
        let capabilities = client.discover().expect("discovery");
        assert_eq!(capabilities.missing(), lacks, "{server}");
    }
}

// Listing the fixture directory is the point of this test.
#[allow(clippy::disallowed_methods)]
#[test]
fn every_recorded_file_is_a_probe_of_a_known_server() {
    for entry in std::fs::read_dir(dir()).expect("fixtures directory") {
        let path = entry.expect("entry").path();
        let server = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        assert!(
            SERVERS.iter().any(|(known, _)| *known == server),
            "{server} is not a recorded server"
        );
        for file in std::fs::read_dir(&path).expect("server directory") {
            let file = file.expect("entry").path();
            let stem = file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("stem");
            assert!(
                probes::PROBES.contains(&stem) && file.extension().is_some_and(|ext| ext == "json"),
                "{} is not a probe fixture",
                file.display()
            );
        }
    }
}
