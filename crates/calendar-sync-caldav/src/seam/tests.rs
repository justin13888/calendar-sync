use calendar_sync_engine::{
    Change, Cursor, Failure, ListFrom, Next, Outcome, ProviderSeam, Request, ResourceId,
};
use calendar_sync_model::ProviderRevision;

use super::{CalDavSeam, Exchange, Listing, precondition, refuses_report, transient, truncated};
use crate::http::{HttpResponse, TransportError};
use crate::object;

const OBJECT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
BEGIN:VEVENT\r\nUID:a@example.com\r\nDTSTAMP:20261101T120000Z\r\n\
DTSTART:20261110T140000Z\r\nDTEND:20261110T150000Z\r\nSUMMARY:Planning\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

fn seam() -> CalDavSeam {
    CalDavSeam::new(
        "https://dav.example.com",
        "/cal",
        "2026-11-20T00:00:00Z".parse().unwrap(),
    )
    .unwrap()
}

/// Drives `exchange` with `responses` in order, checking each request's method.
fn drive(mut exchange: Exchange, responses: Vec<(&str, HttpResponse)>) -> Outcome {
    for (method, response) in responses {
        let request = exchange.request().expect("a request is pending");
        assert_eq!(request.method, method, "{}", request.url);
        exchange.receive(Ok(response));
    }
    assert!(exchange.request().is_none(), "requests left");
    exchange.into_outcome()
}

fn multistatus(responses: &str, token: &str) -> HttpResponse {
    HttpResponse::new(
        207,
        format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\" \
             xmlns:C=\"urn:ietf:params:xml:ns:caldav\">{responses}\
             <D:sync-token>{token}</D:sync-token></D:multistatus>"
        ),
    )
}

fn member(href: &str, etag: &str) -> String {
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:getetag>{etag}</D:getetag>\
         </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
    )
}

fn multiget(href: &str, etag: &str, data: &str) -> HttpResponse {
    HttpResponse::new(
        207,
        format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\" \
             xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:response><D:href>{href}</D:href>\
             <D:propstat><D:prop><D:getetag>{etag}</D:getetag><C:calendar-data>{data}\
             </C:calendar-data></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\
             </D:response></D:multistatus>"
        ),
    )
}

#[test]
fn the_collection_gets_its_trailing_slash() {
    assert_eq!(seam().collection(), "/cal/");
    assert!(
        CalDavSeam::new(
            "not a url",
            "/cal/",
            "2026-11-20T00:00:00Z".parse().unwrap()
        )
        .is_err()
    );
    assert!(CalDavSeam::new("/cal", "/cal/", "2026-11-20T00:00:00Z".parse().unwrap()).is_err());
}

#[test]
fn a_full_listing_reads_the_changed_objects_with_multiget() {
    let seam = seam();
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Start,
    });
    let report = exchange.request().unwrap();
    assert_eq!(report.url, "https://dav.example.com/cal/");
    assert!(report.body.as_deref().unwrap().contains("<D:sync-token/>"));
    let outcome = drive(
        exchange,
        vec![
            ("REPORT", multistatus(&member("/cal/a.ics", "\"1\""), "s1")),
            ("REPORT", multiget("/cal/a.ics", "\"1\"", OBJECT)),
        ],
    );
    let Outcome::Listed(page) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(page.next, Next::Done(Cursor::new("sync:s1")));
    let [
        Change::Upsert {
            resource,
            revision,
            event: Some(event),
        },
    ] = page.changes.as_slice()
    else {
        panic!("{:?}", page.changes)
    };
    assert_eq!(resource.as_str(), "/cal/a.ics");
    assert_eq!(revision.as_str(), "\"1\"");
    assert_eq!(event.fields.title.as_deref(), Some("Planning"));
    assert_eq!(object::object_text(event), Some(OBJECT));
}

#[test]
fn an_incremental_listing_reports_removals_and_a_truncation() {
    let seam = seam();
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Since(Cursor::new("sync:s1")),
    });
    assert!(
        exchange
            .request()
            .unwrap()
            .body
            .as_deref()
            .unwrap()
            .contains("<D:sync-token>s1</D:sync-token>")
    );
    let removed = "<D:response><D:href>/cal/b.ics</D:href>\
                   <D:status>HTTP/1.1 404 Not Found</D:status></D:response>\
                   <D:response><D:href>/cal/</D:href>\
                   <D:status>HTTP/1.1 507 Insufficient Storage</D:status></D:response>";
    let Outcome::Listed(page) = drive(exchange, vec![("REPORT", multistatus(removed, "s2"))])
    else {
        panic!()
    };
    assert_eq!(
        page.changes,
        vec![Change::Remove {
            resource: ResourceId::new("/cal/b.ics")
        }]
    );
    assert_eq!(page.next, Next::More(Cursor::new("sync:s2")));
}

#[test]
fn an_invalid_token_expires_the_cursor() {
    let seam = seam();
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Since(Cursor::new("sync:old")),
    });
    let body = "<D:error xmlns:D=\"DAV:\"><D:valid-sync-token/></D:error>";
    assert_eq!(
        drive(exchange, vec![("REPORT", HttpResponse::new(403, body))]),
        Outcome::Failed(Failure::CursorExpired)
    );
    for cursor in ["etags:not json", "other"] {
        let exchange = seam.exchange(&Request::List {
            from: ListFrom::Since(Cursor::new(cursor)),
        });
        assert_eq!(
            exchange.into_outcome(),
            Outcome::Failed(Failure::CursorExpired)
        );
    }
}

#[test]
fn a_refused_report_falls_back_to_etags() {
    let seam = seam();
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Start,
    });
    let listing = format!(
        "{}{}",
        member("/cal/a.ics", "\"1\""),
        member("/cal/b.ics", "\"2\"")
    );
    let propfind = HttpResponse::new(
        207,
        format!("<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">{listing}</D:multistatus>"),
    );
    let outcome = drive(
        exchange,
        vec![
            ("REPORT", HttpResponse::new(501, "")),
            ("PROPFIND", propfind.clone()),
            ("REPORT", multiget("/cal/a.ics", "\"1\"", OBJECT)),
        ],
    );
    let Outcome::Listed(page) = outcome else {
        panic!("{outcome:?}")
    };
    // b.ics was not in the multiget's answer: the engine reads it on its own.
    assert_eq!(page.changes.len(), 2);
    assert!(
        matches!(&page.changes[1], Change::Upsert { event: None, resource, .. } if resource.as_str() == "/cal/b.ics")
    );
    let Next::Done(cursor) = page.next else {
        panic!()
    };
    assert_eq!(Listing::of(&cursor), Some(Listing::Etags));

    // The next pass compares: b.ics changed, a.ics was removed.
    let changed = HttpResponse::new(
        207,
        format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">{}</D:multistatus>",
            member("/cal/b.ics", "\"3\"")
        ),
    );
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Since(cursor),
    });
    let request = exchange.request().unwrap();
    assert_eq!(request.method, "PROPFIND");
    assert_eq!(request.header("Depth"), Some("1"));
    let mut exchange = exchange;
    exchange.receive(Ok(changed));
    let multiget = exchange.request().unwrap();
    assert!(multiget.body.as_deref().unwrap().contains("/cal/b.ics"));
    assert!(!multiget.body.as_deref().unwrap().contains("/cal/a.ics"));
    // A server error on the multiget fails the page, to be listed again.
    exchange.receive(Ok(HttpResponse::new(500, "")));
    assert_eq!(exchange.into_outcome(), Outcome::Failed(Failure::Transient));
}

#[test]
fn an_etag_pass_reports_removals_and_changes() {
    let seam = seam();
    let cursor = Cursor::new(r#"etags:{"/cal/a.ics":"\"1\"","/cal/b.ics":"\"2\""}"#);
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Since(cursor),
    });
    let changed = HttpResponse::new(
        207,
        format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">{}{}</D:multistatus>",
            member("/cal/b.ics", "\"2\""),
            member("/cal/c.ics", "\"5\"")
        ),
    );
    let outcome = drive(
        exchange,
        vec![
            ("PROPFIND", changed),
            ("REPORT", multiget("/cal/c.ics", "\"5\"", OBJECT)),
        ],
    );
    let Outcome::Listed(page) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(page.changes.len(), 2);
    assert_eq!(
        page.changes[0],
        Change::Remove {
            resource: ResourceId::new("/cal/a.ics")
        }
    );
    assert!(
        matches!(&page.changes[1], Change::Upsert { event: Some(_), resource, .. } if resource.as_str() == "/cal/c.ics")
    );
    assert_eq!(
        page.next,
        Next::Done(Cursor::new(
            r#"etags:{"/cal/b.ics":"\"2\"","/cal/c.ics":"\"5\""}"#
        ))
    );
}

#[test]
fn a_failed_multiget_leaves_each_object_to_a_read() {
    let seam = seam().with_listing(Listing::Etags);
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Start,
    });
    let listing = HttpResponse::new(
        207,
        format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">{}</D:multistatus>",
            member("/cal/a.ics", "\"1\"")
        ),
    );
    let outcome = drive(
        exchange,
        vec![
            ("PROPFIND", listing),
            ("REPORT", HttpResponse::new(403, "")),
        ],
    );
    let Outcome::Listed(page) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        page.changes,
        vec![Change::Upsert {
            resource: ResourceId::new("/cal/a.ics"),
            revision: ProviderRevision::new("\"1\""),
            event: None,
        }]
    );
}

#[test]
fn a_create_is_refused_where_the_href_is_taken() {
    let seam = seam();
    let event = object::read("/cal/a.ics", None, None, OBJECT.as_bytes()).unwrap();
    let request = Request::Create {
        resource: ResourceId::new("/cal/a.ics"),
        event,
    };
    let exchange = seam.exchange(&request);
    let put = exchange.request().unwrap();
    assert_eq!(put.header("If-None-Match"), Some("*"));
    assert_eq!(put.body.as_deref(), Some(OBJECT));
    assert_eq!(
        drive(exchange, vec![("PUT", HttpResponse::new(412, ""))]),
        Outcome::Failed(Failure::AlreadyExists)
    );
    let exchange = seam.exchange(&request);
    let Outcome::Written(written) = drive(
        exchange,
        vec![(
            "PUT",
            HttpResponse::new(201, "").with_header("ETag", "\"9\""),
        )],
    ) else {
        panic!()
    };
    assert_eq!(written.revision, Some(ProviderRevision::new("\"9\"")));
}

#[test]
fn an_update_without_an_etag_in_its_response_reads_back() {
    let seam = seam();
    let mut event = object::read("/cal/a.ics", Some("\"1\""), None, OBJECT.as_bytes()).unwrap();
    event.fields.title = Some("Planning, moved".to_owned());
    let request = Request::Update {
        resource: ResourceId::new("/cal/a.ics"),
        if_match: Some(ProviderRevision::new("\"1\"")),
        event,
    };
    let exchange = seam.exchange(&request);
    assert_eq!(
        exchange.request().unwrap().header("If-Match"),
        Some("\"1\"")
    );
    let stored = OBJECT.replace("SUMMARY:Planning", "SUMMARY:Planning, moved");
    let Outcome::Written(written) = drive(
        exchange,
        vec![
            ("PUT", HttpResponse::new(204, "")),
            (
                "GET",
                HttpResponse::new(200, stored.clone())
                    .with_header("ETag", "\"2\"")
                    .with_header("Schedule-Tag", "\"s2\""),
            ),
        ],
    ) else {
        panic!()
    };
    assert_eq!(written.revision, Some(ProviderRevision::new("\"2\"")));
    assert_eq!(object::schedule_tag(&written), Some("\"s2\""));
    assert_eq!(object::object_text(&written), Some(stored.as_str()));

    let exchange = seam.exchange(&request);
    assert_eq!(
        drive(exchange, vec![("PUT", HttpResponse::new(412, ""))]),
        Outcome::Failed(Failure::PreconditionFailed)
    );
}

#[test]
fn an_unconditional_update_sends_no_precondition() {
    let event = object::read("/cal/a.ics", None, None, OBJECT.as_bytes()).unwrap();
    let exchange = seam().exchange(&Request::Update {
        resource: ResourceId::new("/cal/a.ics"),
        if_match: None,
        event,
    });
    let put = exchange.request().unwrap();
    assert_eq!(put.method, "PUT");
    assert_eq!(put.header("If-Match"), None);
    assert_eq!(
        put.header("Content-Type"),
        Some("text/calendar; charset=utf-8")
    );
}

#[test]
fn deletes_map_their_statuses() {
    let seam = seam();
    let request = Request::Delete {
        resource: ResourceId::new("/cal/a.ics"),
        if_match: Some(ProviderRevision::new("\"1\"")),
    };
    for (status, outcome) in [
        (204, Outcome::Deleted),
        (412, Outcome::Failed(Failure::PreconditionFailed)),
        (404, Outcome::Failed(Failure::NotFound)),
        (503, Outcome::Failed(Failure::Transient)),
    ] {
        let exchange = seam.exchange(&request);
        assert_eq!(
            exchange.request().unwrap().header("If-Match"),
            Some("\"1\"")
        );
        assert_eq!(
            drive(exchange, vec![("DELETE", HttpResponse::new(status, ""))]),
            outcome
        );
    }
    let exchange = seam.exchange(&Request::Delete {
        resource: ResourceId::new("/cal/a.ics"),
        if_match: None,
    });
    assert_eq!(exchange.request().unwrap().header("If-Match"), None);
}

#[test]
fn a_lost_response_is_transient_and_an_abandoned_exchange_too() {
    let seam = seam();
    let request = Request::Read {
        resource: ResourceId::new("/cal/a.ics"),
    };
    let mut exchange = seam.call(&request);
    exchange.receive(Err(TransportError::new("reset")));
    assert_eq!(exchange.into_outcome(), Outcome::Failed(Failure::Transient));
    assert_eq!(
        seam.call(&request).into_outcome(),
        Outcome::Failed(Failure::Transient)
    );
}

#[test]
fn a_rejection_names_the_condition() {
    let body = "<?xml version=\"1.0\"?><D:error xmlns:D=\"DAV:\" \
                xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><C:valid-calendar-data/></D:error>";
    assert_eq!(
        precondition(body).as_deref(),
        Some("CALDAV:valid-calendar-data")
    );
    let event = object::read("/cal/a.ics", None, None, OBJECT.as_bytes()).unwrap();
    let exchange = seam().exchange(&Request::Create {
        resource: ResourceId::new("/cal/a.ics"),
        event,
    });
    assert_eq!(
        drive(exchange, vec![("PUT", HttpResponse::new(403, body))]),
        Outcome::Failed(Failure::Rejected(
            "the server answered 403 (CALDAV:valid-calendar-data)".to_owned()
        ))
    );
}

#[test]
fn only_a_507_response_truncates() {
    assert!(!truncated(b"not xml"));
    assert!(!truncated(
        multistatus(&member("/cal/a.ics", "\"1\""), "s")
            .body
            .as_slice()
    ));
    assert!(refuses_report(501) && !refuses_report(500));
}

#[test]
fn retryable_statuses_are_transient() {
    for status in [401, 408, 425, 429, 500, 502, 503, 504, 505, 507, 511, 599] {
        assert!(transient(status), "{status}");
    }
    for status in [400, 403, 409, 415, 422, 501] {
        assert!(!transient(status), "{status}");
    }
}

#[test]
fn changed_objects_are_read_in_batches() {
    let seam = seam().with_batch(1);
    let exchange = seam.exchange(&Request::List {
        from: ListFrom::Start,
    });
    let listing = format!(
        "{}{}",
        member("/cal/a.ics", "\"1\""),
        member("/cal/b.ics", "\"2\"")
    );
    let b = OBJECT.replace("UID:a@example.com", "UID:b@example.com");
    let outcome = drive(
        exchange,
        vec![
            ("REPORT", multistatus(&listing, "s1")),
            ("REPORT", multiget("/cal/a.ics", "\"1\"", OBJECT)),
            ("REPORT", multiget("/cal/b.ics", "\"2\"", &b)),
        ],
    );
    let Outcome::Listed(page) = outcome else {
        panic!("{outcome:?}")
    };
    let uids: Vec<_> = page
        .changes
        .iter()
        .map(|change| match change {
            Change::Upsert {
                event: Some(event), ..
            } => event.ids.ical_uid.clone().unwrap(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(uids, ["a@example.com", "b@example.com"]);
}
