use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, Page, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::{
    EventTimes, Frequency, NDay, NormalizedEvent, Passthrough, Provider, ProviderEvent,
    ProviderIds, ProviderRevision, Recurrence, RecurrenceRule, TimeValue, Weekday,
};
use serde_json::{Value, json};

use super::{Exchange, GraphSeam, IDEMPOTENCY_PROPERTY, Precondition, Window};
use crate::http::{HttpRequest, HttpResponse, Method, TransportError};

fn seam() -> GraphSeam {
    GraphSeam::new(Window {
        start: "2026-11-01T00:00:00Z".parse().unwrap(),
        end: "2026-12-01T00:00:00Z".parse().unwrap(),
    })
}

fn json_response(status: u16, body: &Value) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(status, body.to_string()))
}

fn event_json(id: &str, change_key: &str, subject: &str) -> Value {
    json!({
        "id": id,
        "changeKey": change_key,
        "subject": subject,
        "start": { "dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC" },
        "end": { "dateTime": "2026-11-10T16:00:00.0000000", "timeZone": "UTC" },
        "originalStartTimeZone": "Eastern Standard Time",
        "originalEndTimeZone": "Eastern Standard Time",
        "showAs": "busy",
        "sensitivity": "normal",
        "isReminderOn": false
    })
}

fn read_event(value: &Value) -> ProviderEvent {
    match crate::event::read(value).unwrap().item {
        crate::event::Item::Event(event) => event,
        crate::event::Item::Removed { .. } => unreachable!(),
    }
}

/// Drives `exchange` with `responses` in order, returning the requests it made.
fn drive(
    exchange: &mut Exchange,
    responses: Vec<Result<HttpResponse, TransportError>>,
) -> Vec<HttpRequest> {
    let mut sent = Vec::new();
    for response in responses {
        let request = exchange.request().expect("a request to send").clone();
        sent.push(request);
        exchange.receive(response);
    }
    assert!(exchange.request().is_none(), "requests left unsent");
    sent
}

fn page(outcome: Option<&Outcome>) -> &Page {
    match outcome {
        Some(Outcome::Listed(page)) => page,
        other => panic!("not a page: {other:?}"),
    }
}

#[test]
fn a_first_list_asks_for_the_window_with_immutable_ids() {
    let seam = seam().with_calendar("cal=1").with_page_size(25);
    let call = seam
        .call(&Request::List {
            from: ListFrom::Start,
        })
        .request()
        .unwrap()
        .clone();
    assert_eq!(call.method, Method::Get);
    assert_eq!(
        call.path,
        "/v1.0/me/calendars/cal%3D1/calendarView/delta?startDateTime=2026-11-01T00%3A00%3A00Z&endDateTime=2026-12-01T00%3A00%3A00Z"
    );
    let prefer = call.header("prefer").unwrap();
    assert!(prefer.contains("IdType=\"ImmutableId\""), "{prefer}");
    assert!(prefer.contains("outlook.timezone=\"UTC\""), "{prefer}");
    assert!(prefer.ends_with("odata.maxpagesize=25"), "{prefer}");
    // A link is followed as Graph wrote it.
    let link = "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=abc";
    let call = seam
        .call(&Request::List {
            from: ListFrom::Since(Cursor::new(link)),
        })
        .request()
        .unwrap()
        .clone();
    assert_eq!(call.path, "/v1.0/me/calendarView/delta?$deltatoken=abc");
    assert_eq!(call.url(), link);
    // A read never asks for a page size.
    let read = seam
        .with_user("person1@example.com")
        .call(&Request::Read {
            resource: ResourceId::new("AAMk"),
        })
        .request()
        .unwrap()
        .clone();
    assert_eq!(read.path, "/v1.0/users/person1%40example.com/events/AAMk");
    assert!(!read.header("prefer").unwrap().contains("maxpagesize"));
}

#[test]
fn removals_and_sparse_rows_are_confirmed_with_a_read() {
    let seam = seam();
    let request = Request::List {
        from: ListFrom::Since(Cursor::new(
            "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=d1",
        )),
    };
    let mut exchange = seam.call(&request);
    let body = json!({
        "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=d2",
        "value": [
            event_json("full", "ck2", "Full row"),
            { "id": "gone", "@removed": { "reason": "deleted" } },
            { "id": "moved", "@removed": { "reason": "deleted" } },
            { "id": "sparse", "changeKey": "ck9", "subject": "only the subject" }
        ]
    });
    let sent = drive(
        &mut exchange,
        vec![
            json_response(200, &body),
            json_response(404, &json!({ "error": { "code": "ErrorItemNotFound" } })),
            json_response(200, &event_json("moved", "ck3", "Outside the window")),
            json_response(200, &event_json("sparse", "ck9", "Sparse, read in full")),
        ],
    );
    let paths: Vec<&str> = sent.iter().map(|call| call.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/v1.0/me/calendarView/delta?$deltatoken=d1",
            "/v1.0/me/events/gone",
            "/v1.0/me/events/moved",
            "/v1.0/me/events/sparse",
        ]
    );
    let page = page(exchange.outcome());
    assert_eq!(
        page.next,
        Next::Done(Cursor::new(
            "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=d2"
        ))
    );
    let summary: Vec<(String, Option<String>)> = page
        .changes
        .iter()
        .map(|change| match change {
            Change::Upsert {
                resource,
                revision,
                event,
            } => {
                assert_eq!(event.as_ref().unwrap().revision.as_ref(), Some(revision));
                (
                    resource.as_str().to_owned(),
                    event.as_ref().unwrap().fields.title.clone(),
                )
            }
            Change::Remove { resource } => (resource.as_str().to_owned(), None),
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("full".to_owned(), Some("Full row".to_owned())),
            ("gone".to_owned(), None),
            ("moved".to_owned(), Some("Outside the window".to_owned())),
            ("sparse".to_owned(), Some("Sparse, read in full".to_owned())),
        ]
    );
}

#[test]
fn a_failed_confirmation_fails_the_page() {
    let body = json!({
        "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/calendarView/delta?$skiptoken=s1",
        "value": [{ "id": "gone", "@removed": { "reason": "deleted" } }]
    });
    for (response, expected) in [
        (Err(TransportError::new("reset")), Failure::Transient),
        (json_response(503, &json!({})), Failure::Transient),
        (
            json_response(
                403,
                &json!({ "error": { "code": "ErrorAccessDenied", "message": "no" } }),
            ),
            Failure::Rejected("403 ErrorAccessDenied: no".to_owned()),
        ),
    ] {
        let mut exchange = seam().call(&Request::List {
            from: ListFrom::Start,
        });
        drive(&mut exchange, vec![json_response(200, &body), response]);
        assert_eq!(exchange.into_outcome(), Outcome::Failed(expected));
    }
}

#[test]
fn list_failures() {
    let list = Request::List {
        from: ListFrom::Since(Cursor::new("/v1.0/me/calendarView/delta?$deltatoken=old")),
    };
    let cases = [
        (json_response(410, &json!({})), Failure::CursorExpired),
        (
            json_response(400, &json!({ "error": { "code": "syncStateNotFound" } })),
            Failure::CursorExpired,
        ),
        (
            json_response(400, &json!({ "error": { "code": "resyncRequired" } })),
            Failure::CursorExpired,
        ),
        (json_response(429, &json!({})), Failure::Transient),
        (json_response(401, &json!({})), Failure::Transient),
        (Err(TransportError::new("timeout")), Failure::Transient),
        (Ok(HttpResponse::new(200, "not json")), Failure::Transient),
        (
            json_response(200, &json!({ "value": [] })),
            Failure::Rejected(
                "a delta page carries neither @odata.nextLink nor @odata.deltaLink".to_owned(),
            ),
        ),
        (
            json_response(
                200,
                &json!({ "value": [{ "id": "x", "changeKey": "k", "start": {}, "end": {} }], "@odata.deltaLink": "d" }),
            ),
            Failure::Rejected("not a Graph event: start has no dateTime".to_owned()),
        ),
    ];
    for (response, expected) in cases {
        let mut exchange = seam().call(&list);
        drive(&mut exchange, vec![response]);
        assert_eq!(exchange.into_outcome(), Outcome::Failed(expected));
    }
}

#[test]
fn reads_and_lookups() {
    let seam = seam();
    let read = Request::Read {
        resource: ResourceId::new("a"),
    };
    let mut exchange = seam.call(&read);
    drive(
        &mut exchange,
        vec![json_response(200, &event_json("a", "k1", "A"))],
    );
    assert!(matches!(exchange.into_outcome(), Outcome::Read(Some(event)) if event.ids.id == "a"));
    for status in [404, 410] {
        let mut exchange = seam.call(&read);
        drive(&mut exchange, vec![json_response(status, &json!({}))]);
        assert_eq!(exchange.into_outcome(), Outcome::Read(None));
    }

    let lookup = Request::Lookup {
        key: ResourceId::new("cseditckog"),
    };
    let mut exchange = seam.call(&lookup);
    let sent = drive(
        &mut exchange,
        vec![json_response(200, &json!({ "value": [] }))],
    );
    assert_eq!(exchange.into_outcome(), Outcome::Read(None));
    assert_eq!(
        sent[0].path,
        format!(
            "/v1.0/me/events?$filter={}",
            crate::http::encode(&format!(
                "singleValueExtendedProperties/any(ep: ep/id eq '{IDEMPOTENCY_PROPERTY}' and ep/value eq 'cseditckog')"
            ))
        )
    );
    let mut exchange = seam.call(&lookup);
    drive(
        &mut exchange,
        vec![json_response(
            200,
            &json!({ "value": [event_json("made", "k1", "Made")] }),
        )],
    );
    assert!(
        matches!(exchange.into_outcome(), Outcome::Read(Some(event)) if event.ids.id == "made")
    );
    // A lookup in a calendar that is gone is not "nothing was created".
    let mut exchange = seam.call(&lookup);
    drive(&mut exchange, vec![json_response(404, &json!({}))]);
    assert!(matches!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::Rejected(_))
    ));
}

fn new_event(title: &str) -> ProviderEvent {
    let mut fields = NormalizedEvent::new(
        EventTimes::new(
            TimeValue::instant("2026-11-12T14:00:00Z".parse().unwrap()),
            Some(TimeValue::instant("2026-11-12T15:00:00Z".parse().unwrap())),
        )
        .unwrap(),
    );
    fields.title = Some(title.to_owned());
    ProviderEvent {
        ids: ProviderIds::new(Provider::Graph, "cseditckog"),
        revision: None,
        mapper_version: crate::MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    }
}

#[test]
fn a_create_carries_the_key_twice() {
    let seam = seam().with_calendar("work");
    let create = Request::Create {
        resource: ResourceId::new("cseditckog"),
        event: new_event("New"),
    };
    let mut exchange = seam.call(&create);
    let sent = drive(
        &mut exchange,
        vec![json_response(201, &event_json("AAMknew", "k1", "New"))],
    );
    let call = &sent[0];
    assert_eq!(call.method, Method::Post);
    assert_eq!(call.path, "/v1.0/me/calendars/work/events");
    assert_eq!(call.header("content-type"), Some("application/json"));
    let body: Value = serde_json::from_str(call.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["transactionId"], "cseditckog");
    assert_eq!(
        body["singleValueExtendedProperties"],
        json!([{ "id": IDEMPOTENCY_PROPERTY, "value": "cseditckog" }])
    );
    assert_eq!(body["subject"], "New");
    let Outcome::Written(made) = exchange.into_outcome() else {
        panic!("not written")
    };
    assert_eq!(made.ids.id, "AAMknew");

    let mut exchange = seam.call(&create);
    drive(&mut exchange, vec![json_response(409, &json!({}))]);
    assert_eq!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::AlreadyExists)
    );
}

#[test]
fn a_recurrence_graph_cannot_hold_is_never_sent() {
    let mut event = new_event("Fifth Mondays");
    let mut rule = RecurrenceRule::new(Frequency::Monthly);
    rule.by_day = vec![NDay::nth(Weekday::Monday, 5)];
    event.fields.recurrence = Some(Recurrence::rule(rule));
    for precondition in [Precondition::ReadCompare, Precondition::IfMatch] {
        let seam = seam().with_precondition(precondition);
        let create = seam.call(&Request::Create {
            resource: ResourceId::new("k"),
            event: event.clone(),
        });
        assert!(create.request().is_none());
        assert!(matches!(
            create.into_outcome(),
            Outcome::Failed(Failure::Rejected(reason)) if reason.starts_with("Graph cannot hold this recurrence")
        ));
    }
    // Read-compare refuses it only once the read shows the recurrence would change.
    let update = Request::Update {
        resource: ResourceId::new("a"),
        if_match: Some(ProviderRevision::new("k1")),
        event,
    };
    let mut exchange = seam().call(&update);
    drive(
        &mut exchange,
        vec![json_response(200, &event_json("a", "k1", "A"))],
    );
    assert!(matches!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::Rejected(_))
    ));
    let mut exchange = seam()
        .with_precondition(Precondition::IfMatch)
        .call(&update);
    assert!(exchange.request().is_none());
    exchange.receive(Err(TransportError::new("ignored")));
    assert!(matches!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::Rejected(_))
    ));
}

fn update(current: &Value, title: &str) -> Request {
    let mut event = read_event(current);
    let revision = event.revision.clone();
    event.fields.title = Some(title.to_owned());
    Request::Update {
        resource: ResourceId::new(event.ids.id.clone()),
        if_match: revision,
        event,
    }
}

#[test]
fn an_update_reads_compares_and_patches_only_what_changed() {
    let current = event_json("a", "k1", "Before");
    let request = update(&current, "After");
    let mut exchange = seam().call(&request);
    let sent = drive(
        &mut exchange,
        vec![
            json_response(200, &current),
            json_response(200, &event_json("a", "k2", "After")),
        ],
    );
    assert_eq!(sent[0].method, Method::Get);
    assert_eq!(sent[1].method, Method::Patch);
    assert_eq!(sent[1].path, "/v1.0/me/events/a");
    assert_eq!(sent[1].header("if-match"), None);
    let patch: Value = serde_json::from_str(sent[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(patch, json!({ "subject": "After" }));
    let Outcome::Written(written) = exchange.into_outcome() else {
        panic!("not written")
    };
    assert_eq!(written.revision, Some(ProviderRevision::new("k2")));

    // With If-Match as well.
    let mut exchange = seam()
        .with_precondition(Precondition::ReadCompareIfMatch)
        .call(&request);
    let sent = drive(
        &mut exchange,
        vec![json_response(200, &current), json_response(412, &json!({}))],
    );
    assert_eq!(sent[1].header("if-match"), Some("k1"));
    assert_eq!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::PreconditionFailed)
    );
}

#[test]
fn a_moved_change_key_stops_the_write() {
    let request = update(&event_json("a", "k1", "Before"), "After");
    let mut exchange = seam().call(&request);
    let sent = drive(
        &mut exchange,
        vec![json_response(200, &event_json("a", "k5", "Someone else's"))],
    );
    assert_eq!(sent.len(), 1, "no PATCH after a mismatch");
    assert_eq!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    // Graph's own conflict answer, and a vanished event.
    for (status, expected) in [(409, Failure::PreconditionFailed), (404, Failure::NotFound)] {
        let mut exchange = seam().call(&request);
        drive(
            &mut exchange,
            vec![
                json_response(200, &event_json("a", "k1", "Before")),
                json_response(
                    status,
                    &json!({ "error": { "code": "ErrorIrresolvableConflict" } }),
                ),
            ],
        );
        assert_eq!(exchange.into_outcome(), Outcome::Failed(expected));
    }
    let mut exchange = seam().call(&request);
    drive(&mut exchange, vec![json_response(404, &json!({}))]);
    assert_eq!(exchange.into_outcome(), Outcome::Failed(Failure::NotFound));
}

#[test]
fn an_update_that_changes_nothing_sends_nothing() {
    let current = event_json("a", "k1", "Same");
    let request = update(&current, "Same");
    let mut exchange = seam().call(&request);
    let sent = drive(&mut exchange, vec![json_response(200, &current)]);
    assert_eq!(sent.len(), 1);
    assert!(
        matches!(exchange.into_outcome(), Outcome::Written(event) if event.revision == Some(ProviderRevision::new("k1")))
    );
}

#[test]
fn if_match_alone_patches_everything_in_one_call() {
    let request = update(&event_json("a", "k1", "Before"), "After");
    let mut exchange = seam()
        .with_precondition(Precondition::IfMatch)
        .call(&request);
    let sent = drive(
        &mut exchange,
        vec![json_response(200, &event_json("a", "k2", "After"))],
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, Method::Patch);
    assert_eq!(sent[0].header("if-match"), Some("k1"));
    let patch: Value = serde_json::from_str(sent[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(patch["subject"], "After");
    assert!(patch.get("start").is_some() && patch.get("showAs").is_some());
}

#[test]
fn a_delete_reads_compares_and_deletes() {
    let delete = Request::Delete {
        resource: ResourceId::new("a"),
        if_match: Some(ProviderRevision::new("k1")),
    };
    let mut exchange = seam().call(&delete);
    let sent = drive(
        &mut exchange,
        vec![
            json_response(200, &event_json("a", "k1", "A")),
            Ok(HttpResponse::new(204, "")),
        ],
    );
    assert_eq!(sent[1].method, Method::Delete);
    assert_eq!(sent[1].header("if-match"), None);
    assert_eq!(exchange.into_outcome(), Outcome::Deleted);

    let mut exchange = seam().call(&delete);
    drive(
        &mut exchange,
        vec![json_response(200, &event_json("a", "k2", "A"))],
    );
    assert_eq!(
        exchange.into_outcome(),
        Outcome::Failed(Failure::PreconditionFailed)
    );

    let mut exchange = seam()
        .with_precondition(Precondition::IfMatch)
        .call(&delete);
    let sent = drive(&mut exchange, vec![json_response(404, &json!({}))]);
    assert_eq!(sent[0].header("if-match"), Some("k1"));
    assert_eq!(exchange.into_outcome(), Outcome::Failed(Failure::NotFound));

    // An unconditional delete skips the comparison but still reads.
    let mut exchange = seam().call(&Request::Delete {
        resource: ResourceId::new("a"),
        if_match: None,
    });
    drive(
        &mut exchange,
        vec![
            json_response(200, &event_json("a", "k9", "A")),
            Ok(HttpResponse::new(204, "")),
        ],
    );
    assert_eq!(exchange.into_outcome(), Outcome::Deleted);
}

#[test]
fn transient_and_rejected_statuses() {
    let requests = [
        Request::Read {
            resource: ResourceId::new("a"),
        },
        Request::Create {
            resource: ResourceId::new("k"),
            event: new_event("x"),
        },
        Request::Delete {
            resource: ResourceId::new("a"),
            if_match: None,
        },
    ];
    for request in &requests {
        for status in [401, 408, 429, 500, 503] {
            let mut exchange = seam().call(request);
            drive(&mut exchange, vec![json_response(status, &json!({}))]);
            assert_eq!(
                exchange.into_outcome(),
                Outcome::Failed(Failure::Transient),
                "{status} {request:?}"
            );
        }
        let mut exchange = seam().call(request);
        drive(
            &mut exchange,
            vec![json_response(
                400,
                &json!({ "error": { "message": "bad" } }),
            )],
        );
        assert_eq!(
            exchange.into_outcome(),
            Outcome::Failed(Failure::Rejected("400: bad".to_owned()))
        );
    }
    // A write whose 2xx cannot be read may have taken effect.
    let mut exchange = seam().call(&requests[1]);
    drive(&mut exchange, vec![Ok(HttpResponse::new(201, "{"))]);
    assert_eq!(exchange.into_outcome(), Outcome::Failed(Failure::Transient));
}

#[test]
fn an_abandoned_exchange_is_transient_and_extra_responses_are_ignored() {
    let seam = seam();
    let request = Request::Read {
        resource: ResourceId::new("a"),
    };
    let exchange = seam.call(&request);
    assert_eq!(exchange.outcome(), None);
    assert_eq!(
        seam.outcome(&request, exchange),
        Outcome::Failed(Failure::Transient)
    );
    let mut exchange = seam.call(&request);
    exchange.receive(json_response(404, &json!({})));
    exchange.receive(json_response(200, &event_json("a", "k1", "late")));
    assert_eq!(exchange.into_outcome(), Outcome::Read(None));
}
