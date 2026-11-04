//! The seam against recorded exchanges: every fixture in `tests/fixtures/` is clean, and each
//! scenario replays through [`Client`] with the outcomes the engine expects. The fixtures are
//! synthesized from the documented formats until the live suite records real ones.

use std::path::{Path, PathBuf};

use calendar_sync_engine::{
    Change, Cursor, Effect, Failure, Input, ListFrom, Next, Outcome, Request, ResourceId, SyncState,
};
use calendar_sync_google::{
    Client, GoogleSeam, HttpRequest, HttpResponse, Transport, TransportError, group,
};
use calendar_sync_model::{
    EventStatus, EventTimes, InstanceKey, NormalizedEvent, Override, Provider, ProviderEvent,
    ProviderIds, ProviderRevision, TimeValue, TimeZoneId,
};
use calendar_sync_testkit::fixture::{Body, Fixture, Replay};
use jiff::civil::date;

// Reading the fixture directory is the point of this test.
#[allow(clippy::disallowed_methods)]
fn load(name: &str) -> Fixture {
    let path = dir().join(name);
    let text = std::fs::read_to_string(&path).expect("fixture text");
    Fixture::from_json(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A transport that answers from a fixture, and checks each call carries the recorded headers.
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
            .respond(request.method.as_str(), &request.path)
            .unwrap_or_else(|error| panic!("{error}"));
        if let Some(recorded) = exchange {
            for (name, value) in &recorded.request.headers {
                assert_eq!(
                    request.header(name),
                    Some(value.as_str()),
                    "{} {}: header {name}",
                    request.method,
                    request.path
                );
            }
            assert_eq!(
                request.body.is_some(),
                recorded.request.method == "POST" || recorded.request.method == "PUT",
                "a body only on an insert or an update"
            );
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

fn replaying<'a>(fixture: &'a Fixture, seam: GoogleSeam) -> Client<Replayer<'a>> {
    Client::new(
        seam,
        Replayer {
            fixture,
            replay: fixture.replay(),
        },
    )
}

fn finish(client: Client<Replayer<'_>>) {
    client
        .into_transport()
        .replay
        .finish()
        .expect("every exchange used");
}

fn listed(outcome: Outcome) -> (Vec<Change>, Next) {
    match outcome {
        Outcome::Listed(page) => (page.changes, page.next),
        other => panic!("not a page: {other:?}"),
    }
}

fn upserted(change: &Change) -> &ProviderEvent {
    match change {
        Change::Upsert {
            event: Some(event), ..
        } => event,
        other => panic!("not an upsert with its event: {other:?}"),
    }
}

fn toronto(y: i16, m: i8, d: i8, h: i8, min: i8) -> TimeValue {
    TimeValue::zoned(
        date(y, m, d).at(h, min, 0, 0),
        TimeZoneId::new("America/Toronto"),
    )
}

#[test]
fn every_fixture_is_clean_and_named_json() {
    // Reading the fixture directory is the point of this test.
    #[allow(clippy::disallowed_methods)]
    let entries = std::fs::read_dir(dir()).expect("fixtures directory");
    let mut count = 0;
    for entry in entries {
        let path = entry.expect("entry").path();
        assert_eq!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("json"),
            "{}",
            path.display()
        );
        let fixture = load(
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("name"),
        );
        assert_eq!(fixture.provider, Provider::Google);
        let mut redacted = fixture.clone();
        redacted.redact();
        assert_eq!(redacted, fixture, "{} changes on redaction", path.display());
        count += 1;
    }
    assert!(count >= 6, "fixtures missing");
}

#[test]
fn initial_sync_pages_until_the_sync_token() {
    let fixture = load("initial-sync.json");
    let mut client = replaying(&fixture, GoogleSeam::new("primary").with_page_size(2));

    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(
        next,
        Next::More(Cursor::new("CigKGjBvNjBvZzJxYzhvMzZiOWc2NG8zMGI5aw"))
    );
    assert_eq!(changes.len(), 2);
    let first = upserted(&changes[0]);
    assert_eq!(first.ids.id, "evt001");
    assert_eq!(
        first.revision,
        Some(ProviderRevision::new("\"3181161784712000\""))
    );
    assert_eq!(first.fields.times.start(), &toronto(2026, 11, 5, 10, 0));
    assert_eq!(first.fields.attendees.len(), 2);
    assert!(first.fields.reminders.use_default);
    let second = upserted(&changes[1]);
    assert!(second.fields.times.is_all_day());

    let Next::More(page) = next else {
        unreachable!()
    };
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Continue(page),
    }));
    assert_eq!(
        next,
        Next::Done(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAU="))
    );
    let third = upserted(&changes[0]);
    assert_eq!(third.fields.status, EventStatus::Tentative);
    assert_eq!(
        third.fields.times.start(),
        &TimeValue::instant("2026-11-09T14:00:00Z".parse().unwrap())
    );
    finish(client);
}

#[test]
fn incremental_sync_sends_the_sync_token() {
    let fixture = load("incremental-sync.json");
    let mut client = replaying(&fixture, GoogleSeam::new("primary"));
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Since(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAU=")),
    }));
    assert_eq!(
        next,
        Next::Done(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAY="))
    );
    let [change] = &changes[..] else {
        panic!("{changes:?}")
    };
    let Change::Upsert {
        resource, revision, ..
    } = change
    else {
        panic!("{change:?}")
    };
    assert_eq!(resource.as_str(), "evt001");
    assert_eq!(revision.as_str(), "\"3181161784799000\"");
    assert_eq!(
        upserted(change).fields.times.start(),
        &toronto(2026, 11, 5, 11, 0)
    );
    finish(client);
}

#[test]
fn a_deleted_event_is_removed_and_a_delete_is_conditional() {
    let fixture = load("deletion.json");
    let mut client = replaying(&fixture, GoogleSeam::new("primary"));
    let (changes, _) = listed(client.perform(&Request::List {
        from: ListFrom::Since(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAY=")),
    }));
    assert_eq!(
        changes,
        vec![Change::Remove {
            resource: ResourceId::new("evt002")
        }]
    );
    let delete = Request::Delete {
        resource: ResourceId::new("evt001"),
        if_match: Some(ProviderRevision::new("\"3181161784799000\"")),
    };
    assert_eq!(client.perform(&delete), Outcome::Deleted);
    assert_eq!(client.perform(&delete), Outcome::Failed(Failure::NotFound));
    finish(client);
}

#[test]
fn an_expired_sync_token_starts_over() {
    let fixture = load("token-reset.json");
    let mut client = replaying(&fixture, GoogleSeam::new("primary"));
    assert_eq!(
        client.perform(&Request::List {
            from: ListFrom::Since(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAc=")),
        }),
        Outcome::Failed(Failure::CursorExpired)
    );
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(changes.len(), 1);
    assert_eq!(
        next,
        Next::Done(Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAg="))
    );
    finish(client);
}

#[test]
fn a_recurring_exception_is_keyed_by_its_original_start() {
    let fixture = load("recurring-exception.json");
    let mut client = replaying(&fixture, GoogleSeam::new("primary"));
    let (changes, _) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(changes.len(), 3);
    let events: Vec<ProviderEvent> = changes.iter().map(upserted).cloned().collect();
    let series = group(events);
    let [series] = &series[..] else {
        panic!("{series:?}")
    };
    let master = series.master.as_ref().expect("master");
    assert_eq!(master.ids.id, "rec001");
    let recurrence = master.fields.recurrence.as_ref().expect("recurrence");
    assert_eq!(recurrence.excluded_dates, vec![toronto(2026, 12, 1, 10, 0)]);

    assert_eq!(series.overrides.len(), 2);
    let moved = &series.overrides[&InstanceKey::new(toronto(2026, 11, 10, 10, 0))];
    let Override::Modified { event, .. } = moved else {
        panic!("{moved:?}")
    };
    assert_eq!(event.ids.id, "rec001_20261110T150000Z");
    assert_eq!(event.ids.series_id.as_deref(), Some("rec001"));
    assert_eq!(event.fields.times.start(), &toronto(2026, 11, 10, 11, 0));
    let cancelled = &series.overrides[&InstanceKey::new(toronto(2026, 11, 17, 10, 0))];
    let Override::Cancelled { ids, revision } = cancelled else {
        panic!("{cancelled:?}")
    };
    assert_eq!(
        ids.as_ref().map(|ids| ids.id.as_str()),
        Some("rec001_20261117T150000Z")
    );
    assert_eq!(
        revision,
        &Some(ProviderRevision::new("\"3181161784902000\""))
    );
    finish(client);
}

fn new_event(title: &str) -> ProviderEvent {
    let mut fields = NormalizedEvent::new(
        EventTimes::new(
            toronto(2026, 11, 12, 9, 0),
            Some(toronto(2026, 11, 12, 9, 30)),
        )
        .unwrap(),
    );
    fields.title = Some(title.to_owned());
    fields.reminders.use_default = true;
    ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "cseditckog"),
        revision: None,
        mapper_version: calendar_sync_google::MAPPER_VERSION,
        fields,
        passthrough: calendar_sync_model::Passthrough::new(),
    }
}

#[test]
fn writes_are_conditional_and_inserts_use_the_client_chosen_id() {
    let fixture = load("conditional-writes.json");
    let seam = GoogleSeam::new("team@example.com");
    let resource = ResourceId::new("cseditckog");

    let create = Request::Create {
        resource: resource.clone(),
        event: new_event("Synthetic text 9"),
    };
    let call = calendar_sync_engine::ProviderSeam::call(&seam, &create);
    let body: serde_json::Value = serde_json::from_str(call.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["id"], "cseditckog");
    assert_eq!(body["summary"], "Synthetic text 9");

    let mut client = replaying(&fixture, seam);
    let Outcome::Written(created) = client.perform(&create) else {
        panic!("not written")
    };
    let revision = created.revision.clone().expect("etag");
    assert_eq!(revision.as_str(), "\"3181161785000000\"");

    let mut edited = created.clone();
    edited.fields.title = Some("Synthetic text 10".to_owned());
    let update = Request::Update {
        resource: resource.clone(),
        if_match: Some(revision),
        event: edited,
    };
    let Outcome::Written(updated) = client.perform(&update) else {
        panic!("not written")
    };
    assert_eq!(updated.fields.title.as_deref(), Some("Synthetic text 10"));
    // The same update again: the etag moved on.
    assert_eq!(
        client.perform(&update),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    // A retried insert: the id is taken.
    assert_eq!(
        client.perform(&create),
        Outcome::Failed(Failure::AlreadyExists)
    );
    assert_eq!(
        client.perform(&Request::Read {
            resource: ResourceId::new("missing0")
        }),
        Outcome::Read(None)
    );
    let read = Request::Read { resource };
    assert_eq!(client.perform(&read), Outcome::Failed(Failure::Transient));
    assert_eq!(
        client.perform(&read),
        Outcome::Failed(Failure::Rejected("403 forbidden: Forbidden".to_owned()))
    );
    finish(client);
}

#[test]
fn a_lost_response_is_transient() {
    struct Lost;
    impl Transport for Lost {
        fn send(&mut self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            Err(TransportError::new("connection reset"))
        }
    }
    let mut client = Client::new(GoogleSeam::new("primary"), Lost);
    assert_eq!(
        client.perform(&Request::List {
            from: ListFrom::Start
        }),
        Outcome::Failed(Failure::Transient)
    );
}

/// The engine itself, driven through the fixtures in order: a full pass, an incremental pass, a
/// deletion, and an expired token that starts the pass over.
#[test]
fn the_engine_syncs_through_the_seam() {
    fn pass(state: &mut SyncState, client: &mut Client<Replayer<'_>>) {
        let mut queue = state.apply(Input::Sync);
        while let Some(effect) = queue.pop() {
            if let Effect::Request { id, request } = effect {
                let outcome = client.perform(&request);
                queue.extend(state.apply(Input::Outcome {
                    request: id,
                    outcome,
                }));
            }
        }
        assert!(state.is_quiet(), "the pass did not settle");
    }

    let mut state = SyncState::new();
    let initial = load("initial-sync.json");
    let mut client = replaying(&initial, GoogleSeam::new("primary").with_page_size(2));
    pass(&mut state, &mut client);
    finish(client);
    assert_eq!(state.records().len(), 3);
    assert_eq!(
        state.cursor(),
        Some(&Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAU="))
    );

    let incremental = load("incremental-sync.json");
    let mut client = replaying(&incremental, GoogleSeam::new("primary"));
    pass(&mut state, &mut client);
    finish(client);
    let evt001 = &state.records()[&ResourceId::new("evt001")];
    assert_eq!(evt001.fields.times.start(), &toronto(2026, 11, 5, 11, 0));

    // Only the deletion fixture's list is the engine's; its deletes are the seam test's.
    let deletion = load("deletion.json");
    let mut client = replaying(&deletion, GoogleSeam::new("primary"));
    pass(&mut state, &mut client);
    assert!(!state.records().contains_key(&ResourceId::new("evt002")));
    assert_eq!(state.records().len(), 2);

    let reset = load("token-reset.json");
    let mut client = replaying(&reset, GoogleSeam::new("primary"));
    pass(&mut state, &mut client);
    finish(client);
    assert_eq!(
        state.cursor(),
        Some(&Cursor::new("CPDAlvWDx70CEPDAlvWDx70CGAg="))
    );
    assert_eq!(
        state.records().keys().collect::<Vec<_>>(),
        vec![&ResourceId::new("evt003")]
    );
}
