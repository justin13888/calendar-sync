//! Every fixture checked into `fixtures/` reads, carries no credentials or personal data, and
//! replays end to end. A fixture added later is checked without changing this test.

use std::path::{Path, PathBuf};

use calendar_sync_testkit::fixture::Fixture;

// Reading the fixture directory is the point of this test.
#[allow(clippy::disallowed_methods)]
fn fixtures() -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files: Vec<(PathBuf, String)> = std::fs::read_dir(&dir)
        .expect("fixtures directory")
        .map(|entry| entry.expect("directory entry").path())
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("fixture text");
            (path, text)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_fixture_is_clean_and_replays() {
    let files = fixtures();
    assert!(!files.is_empty(), "no fixtures found");
    for (path, text) in files {
        assert_eq!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("json"),
            "{} is not a .json fixture",
            path.display()
        );
        let fixture =
            Fixture::from_json(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(!fixture.exchanges.is_empty(), "{} is empty", path.display());

        let written = fixture.to_json().expect("serializes");
        assert_eq!(Fixture::from_json(&written).expect("reads back"), fixture);

        let mut redacted = fixture.clone();
        redacted.redact();
        assert_eq!(redacted, fixture, "{} changes on redaction", path.display());

        let mut replay = fixture.replay();
        for exchange in &fixture.exchanges {
            let response = replay
                .respond(&exchange.request.method, &exchange.request.url)
                .expect("recorded request");
            assert_eq!(response, &exchange.response);
        }
        replay.finish().expect("every exchange used");
    }
}
