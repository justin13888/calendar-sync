//! The seam against recorded exchanges: every fixture in `tests/fixtures/` is clean, and each
//! scenario replays through [`Client`] with the outcomes the engine expects. The fixtures are
//! synthesized from the documented formats until the live suite records real ones.

use std::path::{Path, PathBuf};

use calendar_sync_engine::{
    Change, Cursor, Effect, Failure, Input, ListFrom, Next, Outcome, Request, ResourceId, SyncState,
};
use calendar_sync_graph::{
    Client, GraphSeam, HttpRequest, HttpResponse, Precondition, Transport, TransportError, Window,
    group,
};
use calendar_sync_model::{
    EventStatus, EventTimes, Frequency, InstanceKey, NormalizedEvent, Override, Passthrough,
    Provider, ProviderEvent, ProviderIds, ProviderRevision, RuleEnd, TimeValue, TimeZoneId,
    Transparency,
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

/// A transport that answers from a fixture, and checks each call carries the recorded headers
/// and, where one was recorded, the recorded JSON body.
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
                recorded.request.method == "POST" || recorded.request.method == "PATCH",
                "a body only on a create or an update"
            );
            if let Body::Json(expected) = &recorded.request.body {
                let sent: serde_json::Value =
                    serde_json::from_str(request.body.as_deref().unwrap_or("null")).unwrap();
                assert_eq!(&sent, expected, "{} {}: body", request.method, request.path);
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

fn window() -> Window {
    Window {
        start: "2026-11-01T00:00:00Z".parse().unwrap(),
        end: "2026-12-01T00:00:00Z".parse().unwrap(),
    }
}

fn replaying<'a>(fixture: &'a Fixture, seam: GraphSeam) -> Client<Replayer<'a>> {
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

fn written(outcome: Outcome) -> ProviderEvent {
    match outcome {
        Outcome::Written(event) => event,
        other => panic!("not written: {other:?}"),
    }
}

fn new_york(m: i8, d: i8, h: i8) -> TimeValue {
    TimeValue::zoned(
        date(2026, m, d).at(h, 0, 0, 0),
        TimeZoneId::new("America/New_York"),
    )
}

fn delta(token: &str) -> Cursor {
    Cursor::new(format!(
        "https://graph.microsoft.com/v1.0/me/calendarView/delta?{token}"
    ))
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
        assert_eq!(fixture.provider, Provider::Graph);
        let mut redacted = fixture.clone();
        redacted.redact();
        assert_eq!(redacted, fixture, "{} changes on redaction", path.display());
        count += 1;
    }
    assert!(count >= 6, "fixtures missing");
}

#[test]
fn a_first_round_pages_until_the_delta_link() {
    let fixture = load("initial-sync.json");
    let mut client = replaying(&fixture, GraphSeam::new(window()).with_page_size(2));
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(next, Next::More(delta("$skiptoken=skip001")));
    assert_eq!(changes.len(), 2);
    let first = upserted(&changes[0]);
    assert_eq!(first.ids.id, "AAMkA001");
    assert_eq!(first.revision, Some(ProviderRevision::new("ck-a1")));
    assert_eq!(first.fields.times.start(), &new_york(11, 5, 10));
    assert_eq!(first.fields.attendees.len(), 2);
    assert_eq!(first.fields.reminders.alerts.len(), 1);
    let second = upserted(&changes[1]);
    assert!(second.fields.times.is_all_day());
    assert_eq!(second.fields.transparency, Transparency::Transparent);
    assert!(second.fields.reminders.alerts.is_empty());

    let Next::More(page) = next else {
        unreachable!()
    };
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Continue(page),
    }));
    assert_eq!(next, Next::Done(delta("$deltatoken=delta001")));
    let third = upserted(&changes[0]);
    assert_eq!(third.fields.status, EventStatus::Tentative);
    assert_eq!(
        third.fields.times.start(),
        &TimeValue::instant("2026-11-09T14:00:00Z".parse().unwrap())
    );
    finish(client);
}

#[test]
fn removals_are_confirmed_and_sparse_rows_read() {
    let fixture = load("incremental-sync.json");
    let mut client = replaying(&fixture, GraphSeam::new(window()));
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Since(delta("$deltatoken=delta001")),
    }));
    assert_eq!(next, Next::Done(delta("$deltatoken=delta002")));
    let [changed, removed, moved, sparse] = &changes[..] else {
        panic!("{changes:?}")
    };
    assert_eq!(upserted(changed).fields.times.start(), &new_york(11, 5, 11));
    assert_eq!(
        removed,
        &Change::Remove {
            resource: ResourceId::new("AAMkB001")
        }
    );
    // Marked removed, but it only left the window: it still exists, so it is not removed.
    let moved = upserted(moved);
    assert_eq!(moved.ids.id, "AAMkC001");
    assert_eq!(moved.revision, Some(ProviderRevision::new("ck-c2")));
    let sparse = upserted(sparse);
    assert_eq!(sparse.ids.id, "AAMkD001");
    assert_eq!(
        sparse.fields.times.start(),
        &TimeValue::zoned(
            date(2026, 11, 20).at(10, 0, 0, 0),
            TimeZoneId::new("Europe/Berlin")
        )
    );
    finish(client);
}

#[test]
fn an_expired_delta_link_starts_over() {
    let fixture = load("token-reset.json");
    let mut client = replaying(&fixture, GraphSeam::new(window()));
    assert_eq!(
        client.perform(&Request::List {
            from: ListFrom::Since(delta("$deltatoken=delta002")),
        }),
        Outcome::Failed(Failure::CursorExpired)
    );
    let (changes, next) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(changes.len(), 1);
    assert_eq!(next, Next::Done(delta("$deltatoken=delta003")));
    finish(client);
}

#[test]
fn a_create_is_found_again_by_its_key() {
    let fixture = load("create-and-lookup.json");
    let mut client = replaying(&fixture, GraphSeam::new(window()).with_calendar("work"));
    let key = ResourceId::new("cseditckog");
    let lookup = Request::Lookup { key: key.clone() };
    assert_eq!(client.perform(&lookup), Outcome::Read(None));

    let mut fields = NormalizedEvent::new(
        EventTimes::new(
            new_york(11, 12, 9),
            Some(TimeValue::zoned(
                date(2026, 11, 12).at(9, 30, 0, 0),
                TimeZoneId::new("America/New_York"),
            )),
        )
        .unwrap(),
    );
    fields.title = Some("Synthetic text 9".to_owned());
    let event = ProviderEvent {
        ids: ProviderIds::new(Provider::Graph, key.as_str()),
        revision: None,
        mapper_version: calendar_sync_graph::MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    };
    let call = calendar_sync_engine::ProviderSeam::call(
        client.seam(),
        &Request::Create {
            resource: key.clone(),
            event: event.clone(),
        },
    );
    let body: serde_json::Value =
        serde_json::from_str(call.request().unwrap().body.as_deref().unwrap()).unwrap();
    assert_eq!(body["transactionId"], "cseditckog");
    assert_eq!(
        body["singleValueExtendedProperties"][0]["value"],
        "cseditckog"
    );
    assert_eq!(body["start"]["timeZone"], "Eastern Standard Time");

    let made = written(client.perform(&Request::Create {
        resource: key.clone(),
        event,
    }));
    assert_eq!(made.ids.id, "AAMkN001");
    let Outcome::Read(Some(found)) = client.perform(&lookup) else {
        panic!("the create is not found by its key")
    };
    assert_eq!(found.ids.id, made.ids.id);
    finish(client);
}

#[test]
fn writes_compare_the_change_key_and_patch_what_changed() {
    let fixture = load("conditional-writes.json");
    let resource = ResourceId::new("AAMkA001");
    let base = {
        let mut fields = NormalizedEvent::new(
            EventTimes::new(new_york(11, 5, 11), Some(new_york(11, 5, 12))).unwrap(),
        );
        fields.title = Some("Synthetic text 1".to_owned());
        ProviderEvent {
            ids: ProviderIds::new(Provider::Graph, resource.as_str()),
            revision: Some(ProviderRevision::new("ck-a2")),
            mapper_version: calendar_sync_graph::MAPPER_VERSION,
            fields,
            passthrough: Passthrough::new(),
        }
    };

    let mut client = replaying(&fixture, GraphSeam::new(window()));
    let mut edited = base.clone();
    edited.fields.title = Some("Synthetic text 10".to_owned());
    let update = Request::Update {
        resource: resource.clone(),
        if_match: base.revision.clone(),
        event: edited,
    };
    // The replayer checks the PATCH body is the subject alone.
    let updated = written(client.perform(&update));
    assert_eq!(updated.revision, Some(ProviderRevision::new("ck-a3")));
    // Untouched properties stay, server-side and in the record.
    assert_eq!(
        updated.passthrough.get("importance").next(),
        Some("\"high\"")
    );
    // Planned from ck-a2, but the event is at ck-a3: no PATCH is sent.
    assert_eq!(
        client.perform(&update),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    // A read and a PATCH, then a read alone.
    assert_used(&mut client, 3);

    let mut client = client.into_transport();
    let mut if_match = Client::new(
        GraphSeam::new(window()).with_precondition(Precondition::ReadCompareIfMatch),
        &mut client,
    );
    let mut again = updated.clone();
    again.fields.title = Some("Synthetic text 11".to_owned());
    assert_eq!(
        if_match.perform(&Request::Update {
            resource: resource.clone(),
            if_match: updated.revision.clone(),
            event: again,
        }),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    let mut plain = Client::new(GraphSeam::new(window()), &mut client);
    let delete = Request::Delete {
        resource,
        if_match: updated.revision.clone(),
    };
    assert_eq!(plain.perform(&delete), Outcome::Deleted);
    assert_eq!(plain.perform(&delete), Outcome::Failed(Failure::NotFound));
    client.replay.finish().expect("every exchange used");
}

/// Asserts `client` has used exactly `used` exchanges so far.
fn assert_used(client: &mut Client<Replayer<'_>>, used: usize) {
    let transport = client.transport_mut();
    assert_eq!(
        transport.fixture.exchanges.len() - transport.replay.remaining(),
        used
    );
}

#[test]
fn a_series_groups_its_exceptions_by_original_start() {
    let fixture = load("recurring-series.json");
    let mut client = replaying(&fixture, GraphSeam::new(window()));
    let (changes, _) = listed(client.perform(&Request::List {
        from: ListFrom::Start,
    }));
    assert_eq!(changes.len(), 4);
    let events: Vec<ProviderEvent> = changes.iter().map(upserted).cloned().collect();
    let series = group(events);
    let [series] = &series[..] else {
        panic!("{series:?}")
    };
    let master = series.master.as_ref().expect("master");
    assert_eq!(master.ids.id, "AAMkS001");
    let rule = &master.fields.recurrence.as_ref().expect("recurrence").rules[0];
    assert_eq!(rule.frequency, Frequency::Weekly);
    assert_eq!(
        rule.end,
        RuleEnd::Until {
            until: TimeValue::instant("2026-11-24T15:00:00Z".parse().unwrap())
        }
    );
    // The unchanged occurrence is the master's pattern, not an override.
    assert_eq!(series.overrides.len(), 2, "{:?}", series.overrides);
    let moved = &series.overrides[&InstanceKey::new(new_york(11, 10, 10))];
    let Override::Modified { event, .. } = moved else {
        panic!("{moved:?}")
    };
    assert_eq!(event.ids.id, "AAMkS001-x1");
    assert_eq!(event.fields.times.start(), &new_york(11, 10, 11));
    let cancelled = &series.overrides[&InstanceKey::new(new_york(11, 17, 10))];
    assert!(
        matches!(cancelled, Override::Cancelled { ids: Some(ids), .. } if ids.id == "AAMkS001-x2"),
        "{cancelled:?}"
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
    let mut client = Client::new(GraphSeam::new(window()), Lost);
    assert_eq!(
        client.perform(&Request::List {
            from: ListFrom::Start
        }),
        Outcome::Failed(Failure::Transient)
    );
}

/// The engine itself, driven through the fixtures in order: a first round, an incremental round
/// with confirmed and refuted removals, and an expired link that starts the round over.
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
    let mut client = replaying(&initial, GraphSeam::new(window()).with_page_size(2));
    pass(&mut state, &mut client);
    finish(client);
    assert_eq!(state.records().len(), 3);
    assert_eq!(state.cursor(), Some(&delta("$deltatoken=delta001")));

    let incremental = load("incremental-sync.json");
    let mut client = replaying(&incremental, GraphSeam::new(window()));
    pass(&mut state, &mut client);
    finish(client);
    let ids: Vec<&str> = state.records().keys().map(ResourceId::as_str).collect();
    assert_eq!(ids, ["AAMkA001", "AAMkC001", "AAMkD001"]);
    let a = &state.records()[&ResourceId::new("AAMkA001")];
    assert_eq!(a.fields.times.start(), &new_york(11, 5, 11));

    let reset = load("token-reset.json");
    let mut client = replaying(&reset, GraphSeam::new(window()));
    pass(&mut state, &mut client);
    finish(client);
    assert_eq!(state.cursor(), Some(&delta("$deltatoken=delta003")));
    let ids: Vec<&str> = state.records().keys().map(ResourceId::as_str).collect();
    assert_eq!(ids, ["AAMkA001"]);
}
