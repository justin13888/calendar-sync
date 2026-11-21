//! The probes `live.rs` runs against a real server and `fixtures.rs` replays from what they
//! recorded. They are the same code, so a replay sends exactly the requests the recording did.
//!
//! Every probe starts by discovering the collection, whose `OPTIONS` request names it, so a
//! replay finds the collection in the fixture's first exchange. Probes assert relations between
//! what the server returned (a title changed, an `ETag` moved), never recorded text: a recording
//! replaces free text with synthetic text.

#![allow(
    dead_code,
    reason = "each test target that includes this module uses its own part"
)]

use calendar_sync_caldav::{Capabilities, Client, Feature, Listing, Transport, object};
use calendar_sync_engine::{
    Change, Cursor, DerivedIds, EditId, Failure, ListFrom, Next, Outcome, Request, ResourceId,
};
use calendar_sync_model::{
    EventSeries, EventTimes, InstanceKey, NormalizedEvent, Override, Passthrough, Provider,
    ProviderEvent, ProviderIds, ProviderRevision, TimeValue,
};

/// Every probe, by the name its fixture is recorded under.
pub(crate) const PROBES: [&str; 5] = ["discover", "lifecycle", "etags", "series", "expired"];

/// The `DTSTAMP` of every `VEVENT` a probe writes, so a replay sends the recorded bodies.
pub(crate) const STAMP: &str = "2026-11-20T00:00:00Z";

/// Runs the probe `name`.
pub(crate) fn run<T: Transport>(name: &str, client: &mut Client<T>) {
    let capabilities = discover(client);
    client.seam_mut().set_listing(capabilities.listing());
    match name {
        "discover" => {}
        "lifecycle" => lifecycle(client, &capabilities),
        "etags" => etags(client),
        "series" => series(client),
        "expired" => expired(client, &capabilities),
        other => panic!("no probe named {other}"),
    }
}

fn discover<T: Transport>(client: &mut Client<T>) -> Capabilities {
    let capabilities = client.discover().expect("discovery");
    eprintln!(
        "capabilities: missing {:?}, DAV {:?}",
        capabilities.missing(),
        capabilities.dav
    );
    assert!(
        !capabilities.missing().contains(&Feature::CalendarAccess),
        "not a CalDAV server"
    );
    assert!(
        !capabilities.missing().contains(&Feature::Events),
        "the collection holds no events"
    );
    capabilities
}

fn href<T: Transport>(client: &Client<T>, edit: &str) -> ResourceId {
    DerivedIds::of(&EditId::new(edit)).caldav_href(client.seam().collection())
}

/// Deletes what an earlier, interrupted run may have left at `href`.
fn clear<T: Transport>(client: &mut Client<T>, href: &ResourceId) {
    match client.perform(&Request::Delete {
        resource: href.clone(),
        if_match: None,
    }) {
        Outcome::Deleted | Outcome::Failed(Failure::NotFound) => {}
        other => panic!("clearing {}: {other:?}", href.as_str()),
    }
}

/// Lists a whole pass, returning its changes and the cursor the next pass starts from.
pub(crate) fn pass<T: Transport>(client: &mut Client<T>, from: ListFrom) -> (Vec<Change>, Cursor) {
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
                        };
                    }
                    Next::Done(cursor) => return (changes, cursor),
                }
            }
            other => panic!("listing failed: {other:?}"),
        }
    }
}

/// The change to `href` among `changes`.
fn change<'c>(changes: &'c [Change], href: &ResourceId) -> Option<&'c Change> {
    changes.iter().rev().find(|change| match change {
        Change::Upsert { resource, .. } | Change::Remove { resource } => resource == href,
    })
}

fn upserted<'c>(changes: &'c [Change], href: &ResourceId) -> &'c ProviderEvent {
    match change(changes, href) {
        Some(Change::Upsert {
            event: Some(event), ..
        }) => event,
        other => panic!("{} not upserted with its event: {other:?}", href.as_str()),
    }
}

fn read<T: Transport>(client: &mut Client<T>, href: &ResourceId) -> Option<ProviderEvent> {
    match client.perform(&Request::Read {
        resource: href.clone(),
    }) {
        Outcome::Read(event) => event,
        other => panic!("reading {}: {other:?}", href.as_str()),
    }
}

fn written(outcome: Outcome) -> ProviderEvent {
    match outcome {
        Outcome::Written(event) => event,
        other => panic!("not written: {other:?}"),
    }
}

/// A new one-hour event in January 2030, under the ids the engine derives from `edit`.
fn new_event<T: Transport>(
    client: &Client<T>,
    edit: &str,
    title: &str,
) -> (ResourceId, ProviderEvent) {
    let ids = DerivedIds::of(&EditId::new(edit));
    let href = ids.caldav_href(client.seam().collection());
    let start = TimeValue::instant("2030-01-07T15:00:00Z".parse().expect("start"));
    let end = TimeValue::instant("2030-01-07T16:00:00Z".parse().expect("end"));
    let mut fields = NormalizedEvent::new(EventTimes::new(start, Some(end)).expect("times"));
    fields.title = Some(title.to_owned());
    fields.description = Some("Written by the calendar-sync CalDAV probe".to_owned());
    let event = ProviderEvent {
        ids: ProviderIds {
            provider: Provider::CalDav,
            id: href.as_str().to_owned(),
            ical_uid: Some(ids.ical_uid()),
            series_id: None,
        },
        revision: None,
        mapper_version: calendar_sync_caldav::MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    };
    (href, event)
}

fn revision(event: &ProviderEvent) -> ProviderRevision {
    event.revision.clone().expect("a revision")
}

/// Create, list, update and delete one event, with each precondition refused once.
fn lifecycle<T: Transport>(client: &mut Client<T>, capabilities: &Capabilities) {
    let (href, event) = new_event(client, "probe-lifecycle", "Probe event");
    clear(client, &href);
    let (_, cursor) = pass(client, ListFrom::Start);
    assert_eq!(Listing::of(&cursor), Some(capabilities.listing()));

    let create = Request::Create {
        resource: href.clone(),
        event: event.clone(),
    };
    let created = written(client.perform(&create));
    assert_eq!(created.ids.id, href.as_str());
    assert_eq!(
        client.perform(&create),
        Outcome::Failed(Failure::AlreadyExists),
        "a repeated create is refused by If-None-Match: *"
    );

    let (changes, cursor) = pass(client, ListFrom::Since(cursor));
    let listed = upserted(&changes, &href).clone();
    let first = revision(&listed);
    let held = read(client, &href).expect("the created event");
    assert_eq!(revision(&held), first);
    assert_eq!(held.ids, listed.ids);
    assert_eq!(held.fields.times, listed.fields.times);

    let mut renamed = held.clone();
    renamed.fields.title = Some("Probe event, renamed".to_owned());
    let stale = Request::Update {
        resource: href.clone(),
        if_match: Some(ProviderRevision::new("\"calendar-sync-stale\"")),
        event: renamed.clone(),
    };
    assert_eq!(
        client.perform(&stale),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    let updated = written(client.perform(&Request::Update {
        resource: href.clone(),
        if_match: Some(first.clone()),
        event: renamed,
    }));
    let second = revision(&updated);
    assert_ne!(second, first, "an update moves the ETag");

    let (changes, cursor) = pass(client, ListFrom::Since(cursor));
    let relisted = upserted(&changes, &href);
    assert_eq!(revision(relisted), second);
    // Text is compared between two listings, never between a listing and a read: the recorder
    // scrubs a multistatus body and a plain object body apart, so one title may get two
    // synthetic replacements.
    assert_ne!(
        relisted.fields.title, listed.fields.title,
        "the title changed"
    );
    assert_eq!(
        relisted.fields.description, listed.fields.description,
        "only the title changed"
    );

    assert_eq!(
        client.perform(&Request::Delete {
            resource: href.clone(),
            if_match: Some(first),
        }),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    assert_eq!(
        client.perform(&Request::Delete {
            resource: href.clone(),
            if_match: Some(second),
        }),
        Outcome::Deleted
    );
    let (changes, _) = pass(client, ListFrom::Since(cursor));
    assert!(
        matches!(change(&changes, &href), Some(Change::Remove { .. })),
        "the removal is listed: {changes:?}"
    );
    assert_eq!(read(client, &href), None);
}

/// The `ETag` listing, which a server without `sync-collection` gets, against any server.
fn etags<T: Transport>(client: &mut Client<T>) {
    client.seam_mut().set_listing(Listing::Etags);
    let (href, event) = new_event(client, "probe-etags", "Probe event, by ETag");
    clear(client, &href);
    let (_, cursor) = pass(client, ListFrom::Start);
    assert_eq!(Listing::of(&cursor), Some(Listing::Etags));
    let created = written(client.perform(&Request::Create {
        resource: href.clone(),
        event,
    }));
    let (changes, cursor) = pass(client, ListFrom::Since(cursor));
    let listed = upserted(&changes, &href).clone();
    assert_eq!(listed.ids.ical_uid, created.ids.ical_uid);

    let (changes, cursor) = pass(client, ListFrom::Since(cursor));
    assert!(
        change(&changes, &href).is_none(),
        "an unchanged object is not listed again"
    );

    assert_eq!(
        client.perform(&Request::Delete {
            resource: href.clone(),
            if_match: Some(revision(&listed)),
        }),
        Outcome::Deleted
    );
    let (changes, _) = pass(client, ListFrom::Since(cursor));
    assert!(matches!(
        change(&changes, &href),
        Some(Change::Remove { .. })
    ));
}

/// A series with an override: updating the master keeps the override's bytes.
fn series<T: Transport>(client: &mut Client<T>) {
    let ids = DerivedIds::of(&EditId::new("probe-series"));
    let href = ids.caldav_href(client.seam().collection());
    clear(client, &href);
    let uid = ids.ical_uid();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//calendar-sync//probe//EN\r\n\
BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261120T000000Z\r\n\
DTSTART:20300107T150000Z\r\nDTEND:20300107T153000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
SUMMARY:Probe series\r\nX-CALENDAR-SYNC-PROBE:kept\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261120T000000Z\r\n\
RECURRENCE-ID:20300109T150000Z\r\nDTSTART:20300109T160000Z\r\nDTEND:20300109T163000Z\r\n\
SUMMARY:Probe series, moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let event = object::read(href.as_str(), None, None, text.as_bytes()).expect("a series");
    written(client.perform(&Request::Create {
        resource: href.clone(),
        event,
    }));
    let held = read(client, &href).expect("the series");
    let before = object::series(&held).expect("the whole series");
    assert_eq!(before.series.overrides.len(), 1, "the override is held");
    assert!(held.fields.recurrence.is_some());

    let mut renamed = held.clone();
    renamed.fields.title = Some("Probe series, renamed".to_owned());
    written(client.perform(&Request::Update {
        resource: href.clone(),
        if_match: held.revision.clone(),
        event: renamed,
    }));
    let after = read(client, &href).expect("the series");
    assert_ne!(after.fields.title, held.fields.title);
    let whole = object::series(&after).expect("the whole series");
    assert_eq!(
        overrides(&whole.series),
        overrides(&before.series),
        "the override is untouched"
    );
    assert!(
        object::object_text(&after)
            .expect("text")
            .contains("X-CALENDAR-SYNC-PROBE:kept"),
        "an unknown property survives"
    );
    assert_eq!(
        client.perform(&Request::Delete {
            resource: href,
            if_match: after.revision,
        }),
        Outcome::Deleted
    );
}

/// Each override's instance and fields, without the object's revision, which every write moves.
fn overrides(series: &EventSeries) -> Vec<(InstanceKey, Option<NormalizedEvent>)> {
    series
        .overrides
        .iter()
        .map(|(key, kept)| {
            let fields = match kept {
                Override::Modified { event, .. } => Some(event.fields.clone()),
                Override::Cancelled { .. } => None,
            };
            (key.clone(), fields)
        })
        .collect()
}

/// A sync token the server never issued. RFC 6578 section 3.2 has the server refuse it with
/// `DAV:valid-sync-token`, which expires the cursor; a server that answers a listing instead is
/// reported, not failed, since the engine is then simply listed changes.
fn expired<T: Transport>(client: &mut Client<T>, capabilities: &Capabilities) {
    if !capabilities.sync_collection {
        eprintln!("skipped: the server lacks sync-collection");
        return;
    }
    pass(client, ListFrom::Start);
    let forged = Cursor::new("sync:http://calendar-sync.example.com/forged-token");
    match client.perform(&Request::List {
        from: ListFrom::Since(forged),
    }) {
        Outcome::Failed(Failure::CursorExpired) => eprintln!("a forged sync token expires"),
        Outcome::Listed(page) => eprintln!(
            "finding: the server accepted a sync token it never issued, listing {} changes",
            page.changes.len()
        ),
        other => panic!("a forged sync token: {other:?}"),
    }
}
