use calendar_sync_engine::{
    Cursor, Failure, ListFrom, Next, Outcome, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::{ProviderEvent, ProviderRevision};
use serde_json::{Value, json};

use super::GoogleSeam;
use crate::event::{self, Item};
use crate::http::{HttpResponse, Method, TransportError};

fn seam() -> GoogleSeam {
    GoogleSeam::new("primary")
}

fn resource() -> Value {
    json!({
        "id": "evt001",
        "etag": "\"1\"",
        "summary": "Planning",
        "start": { "dateTime": "2026-11-05T15:00:00Z" },
        "end": { "dateTime": "2026-11-05T16:00:00Z" }
    })
}

fn provider_event() -> ProviderEvent {
    match event::read(&resource()).expect("reads").item {
        Item::Event(event) => event,
        Item::Deleted { id } => panic!("{id} read as deleted"),
    }
}

/// One request of every kind.
fn every_request() -> Vec<Request> {
    let id = ResourceId::new("evt001");
    let revision = Some(ProviderRevision::new("\"1\""));
    vec![
        Request::List {
            from: ListFrom::Since(Cursor::new("sync1")),
        },
        Request::Read {
            resource: id.clone(),
        },
        Request::Lookup { key: id.clone() },
        Request::Create {
            resource: id.clone(),
            event: provider_event(),
        },
        Request::Update {
            resource: id.clone(),
            if_match: revision.clone(),
            event: provider_event(),
        },
        Request::Delete {
            resource: id,
            if_match: revision,
        },
    ]
}

fn outcome(request: &Request, status: u16, body: &Value) -> Outcome {
    seam().outcome(
        request,
        Ok(HttpResponse::new(status, body.to_string().into_bytes())),
    )
}

fn list_page(body: &Value) -> Outcome {
    outcome(
        &Request::List {
            from: ListFrom::Start,
        },
        200,
        body,
    )
}

#[test]
fn unauthorized_timeouts_throttling_and_server_errors_are_transient() {
    let body = json!({ "error": { "code": 0, "message": "try again" } });
    for request in every_request() {
        for status in [401, 408, 429, 500, 502, 503, 599] {
            assert_eq!(
                outcome(&request, status, &body),
                Outcome::Failed(Failure::Transient),
                "{status} to {request:?}"
            );
        }
    }
}

#[test]
fn other_client_errors_are_rejected() {
    let body = json!({
        "error": {
            "code": 403,
            "message": "Forbidden",
            "errors": [ { "reason": "forbidden" } ]
        }
    });
    for request in every_request() {
        for status in [400, 403] {
            assert!(
                matches!(
                    outcome(&request, status, &body),
                    Outcome::Failed(Failure::Rejected(_))
                ),
                "{status} to {request:?}"
            );
        }
    }
}

#[test]
fn no_response_and_an_unreadable_success_are_transient() {
    for request in every_request() {
        assert_eq!(
            seam().outcome(&request, Err(TransportError::new("reset"))),
            Outcome::Failed(Failure::Transient),
            "{request:?}"
        );
        let unreadable = seam().outcome(&request, Ok(HttpResponse::new(200, "<html>")));
        if matches!(request, Request::Delete { .. }) {
            assert_eq!(unreadable, Outcome::Deleted);
        } else {
            assert_eq!(
                unreadable,
                Outcome::Failed(Failure::Transient),
                "{request:?}"
            );
        }
    }
}

#[test]
fn a_lookup_is_a_read_of_the_id() {
    let lookup = Request::Lookup {
        key: ResourceId::new("evt 001"),
    };
    let call = seam().call(&lookup);
    assert_eq!(call.method, Method::Get);
    assert_eq!(call.path, "/calendar/v3/calendars/primary/events/evt%20001");
    assert!(call.body.is_none());
    assert_eq!(
        outcome(&lookup, 200, &resource()),
        Outcome::Read(Some(provider_event()))
    );
    for status in [404, 410] {
        assert_eq!(outcome(&lookup, status, &json!({})), Outcome::Read(None));
    }
}

#[test]
fn a_page_ends_with_its_next_page_or_sync_token() {
    let more = list_page(&json!({ "items": [], "nextPageToken": "p2", "nextSyncToken": "s" }));
    let Outcome::Listed(page) = more else {
        panic!("{more:?}");
    };
    assert_eq!(page.next, Next::More(Cursor::new("p2")));
    let done = list_page(&json!({ "items": [], "nextSyncToken": "s2" }));
    let Outcome::Listed(page) = done else {
        panic!("{done:?}");
    };
    assert_eq!(page.next, Next::Done(Cursor::new("s2")));
}

#[test]
fn a_last_page_without_a_sync_token_is_rejected() {
    assert_eq!(
        list_page(&json!({ "items": [resource()] })),
        Outcome::Failed(Failure::Rejected(
            "the last page carries no nextSyncToken".to_owned()
        ))
    );
}

#[test]
fn a_page_item_without_an_etag_rejects_the_page() {
    let mut item = resource();
    item.as_object_mut().unwrap().remove("etag");
    assert_eq!(
        list_page(&json!({ "items": [resource(), item], "nextSyncToken": "s" })),
        Outcome::Failed(Failure::Rejected("event evt001 has no etag".to_owned()))
    );
}

#[test]
fn a_page_item_that_does_not_map_rejects_the_page() {
    let unmappable = json!({ "id": "evt002", "etag": "\"1\"" });
    assert!(matches!(
        list_page(&json!({ "items": [resource(), unmappable], "nextSyncToken": "s" })),
        Outcome::Failed(Failure::Rejected(_))
    ));
}
