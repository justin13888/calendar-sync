//! Reading, round-tripping and editing whole documents.

use calendar_sync_ical::{IcalDocument, MAPPER_VERSION, Source, WriteError, read_feed};
use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventSeries, EventStatus, EventTimes,
    Field, Frequency, InstanceKey, InstanceRange, LossReason, NormalizedEvent, Override,
    Participation, Provider, ProviderEvent, ProviderIds, ProviderRevision, Recurrence,
    RecurrenceRule, RelativeTo, RuleEnd, TimeValue, TimeZoneId, Visibility,
};
use jiff::civil::{date, datetime};
use jiff::{SignedDuration, Timestamp};

/// A `CalDAV` object: a weekly series in New York with an exception date, a moved instance and a
/// cancelled one, with properties and parameters the model does not map, a folded description, a
/// `VTODO`, and LF-only line endings on one line.
const SERIES: &str = concat!(
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Example Corp//Calendar//EN\r\n",
    "X-WR-CALNAME:Team\r\n",
    "BEGIN:VTIMEZONE\r\n",
    "TZID:America/New_York\r\n",
    "BEGIN:DAYLIGHT\r\n",
    "TZOFFSETFROM:-0500\r\n",
    "TZOFFSETTO:-0400\r\n",
    "TZNAME:EDT\r\n",
    "DTSTART:20070311T020000\r\n",
    "RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n",
    "END:DAYLIGHT\r\n",
    "BEGIN:STANDARD\r\n",
    "TZOFFSETFROM:-0400\r\n",
    "TZOFFSETTO:-0500\r\n",
    "TZNAME:EST\r\n",
    "DTSTART:20071104T020000\r\n",
    "RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n",
    "END:STANDARD\r\n",
    "END:VTIMEZONE\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:standup@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "SEQUENCE:3\r\n",
    "DTSTART;TZID=America/New_York:20260105T093000\r\n",
    "DTEND;TZID=America/New_York:20260105T094500\r\n",
    "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261231T235959Z\r\n",
    "EXDATE;TZID=America/New_York:20260119T093000\r\n",
    "summary:Standup\r\n",
    "DESCRIPTION:A long description that goes on for long enough to be folded ac\r\n",
    " ross two physical lines\\, as RFC 5545 asks.\r\n",
    "LOCATION;X-ROOM-ID=42:Room 1\n",
    "CLASS:PRIVATE\r\n",
    "ORGANIZER;CN=Alice:mailto:alice@example.com\r\n",
    "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED;RSVP=TRUE;X-NOTE=keep:mailto:bob@example.com\r\n",
    "ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:carol@example.com\r\n",
    "X-CUSTOM;X-PARAM=\"a;b\":value\r\n",
    "BEGIN:VALARM\r\n",
    "ACTION:DISPLAY\r\n",
    "TRIGGER:-PT10M\r\n",
    "DESCRIPTION:Standup\r\n",
    "END:VALARM\r\n",
    "BEGIN:VALARM\r\n",
    "ACTION:AUDIO\r\n",
    "TRIGGER:-PT5M\r\n",
    "END:VALARM\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:standup@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "RECURRENCE-ID;TZID=America/New_York:20260107T093000\r\n",
    "DTSTART;TZID=America/New_York:20260107T100000\r\n",
    "DTEND;TZID=America/New_York:20260107T101500\r\n",
    "SUMMARY:Standup (moved)\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:standup@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "RECURRENCE-ID;TZID=America/New_York:20260112T093000\r\n",
    "DTSTART;TZID=America/New_York:20260112T093000\r\n",
    "STATUS:CANCELLED\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VTODO\r\n",
    "UID:todo@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "SUMMARY:Not an event\r\n",
    "END:VTODO\r\n",
    "END:VCALENDAR\r\n",
);

const STAMP: &str = "2026-06-01T12:00:00Z";

fn stamp() -> Timestamp {
    STAMP.parse().unwrap()
}

fn source() -> Source {
    Source::caldav(
        "/calendars/alice/team/standup.ics",
        Some(ProviderRevision::new("\"etag-1\"")),
    )
}

fn new_york(date_time: jiff::civil::DateTime) -> TimeValue {
    TimeValue::zoned(date_time, TimeZoneId::new("America/New_York"))
}

fn text(document: &IcalDocument) -> String {
    String::from_utf8(document.to_bytes()).unwrap()
}

/// The lines of `before` that differ from `after`, position by position, as (before, after).
fn changed_lines(before: &str, after: &str) -> Vec<(String, String)> {
    let before: Vec<&str> = before.split_inclusive('\n').collect();
    let after: Vec<&str> = after.split_inclusive('\n').collect();
    assert_eq!(
        before.len(),
        after.len(),
        "the edit added or removed lines:\n{}",
        after.concat()
    );
    before
        .iter()
        .zip(&after)
        .filter(|(before, after)| before != after)
        .map(|(before, after)| ((*before).to_owned(), (*after).to_owned()))
        .collect()
}

fn read_one(document: &IcalDocument) -> calendar_sync_ical::ReadSeries {
    let mut read = document.read(&source());
    assert_eq!(read.series.len(), 1, "{read:#?}");
    read.series.remove(0)
}

#[test]
fn parsing_keeps_every_byte() {
    let document = IcalDocument::parse(SERIES.as_bytes());
    assert!(document.is_clean());
    assert_eq!(text(&document), SERIES);
}

#[test]
fn writing_back_what_was_read_changes_nothing() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let read = read_one(&document);
    let losses = document.write(&read.series, stamp()).unwrap();
    assert!(losses.is_empty(), "{losses:#?}");
    assert_eq!(text(&document), SERIES);
}

#[test]
fn reads_the_master() {
    let document = IcalDocument::parse(SERIES.as_bytes());
    let read = read_one(&document);
    assert_eq!(read.uid, "standup@example.com");
    let master = read.series.master.as_ref().unwrap();
    assert_eq!(
        master.ids,
        ProviderIds {
            provider: Provider::CalDav,
            id: "/calendars/alice/team/standup.ics".to_owned(),
            ical_uid: Some("standup@example.com".to_owned()),
            series_id: None,
        }
    );
    assert_eq!(master.revision, Some(ProviderRevision::new("\"etag-1\"")));
    assert_eq!(master.mapper_version, MAPPER_VERSION);

    let fields = &master.fields;
    assert_eq!(fields.title.as_deref(), Some("Standup"));
    assert_eq!(fields.location.as_deref(), Some("Room 1"));
    assert_eq!(
        fields.description.as_deref(),
        Some(
            "A long description that goes on for long enough to be folded across two physical \
             lines, as RFC 5545 asks."
        )
    );
    assert_eq!(fields.visibility, Visibility::Private);
    assert_eq!(fields.status, EventStatus::Confirmed);
    assert_eq!(
        fields.times,
        EventTimes::new(
            new_york(datetime(2026, 1, 5, 9, 30, 0, 0)),
            Some(new_york(datetime(2026, 1, 5, 9, 45, 0, 0)))
        )
        .unwrap()
    );
    let recurrence = fields.recurrence.as_ref().unwrap();
    assert_eq!(recurrence.rules.len(), 1);
    assert_eq!(recurrence.rules[0].frequency, Frequency::Weekly);
    assert_eq!(
        recurrence.rules[0].end,
        RuleEnd::Until {
            until: TimeValue::instant("2026-12-31T23:59:59Z".parse().unwrap())
        }
    );
    assert_eq!(
        recurrence.excluded_dates,
        vec![new_york(datetime(2026, 1, 19, 9, 30, 0, 0))]
    );
    let organizer = fields.organizer.as_ref().unwrap();
    assert_eq!(organizer.address, "alice@example.com");
    assert_eq!(organizer.name.as_deref(), Some("Alice"));
    assert_eq!(fields.attendees.len(), 2);
    assert_eq!(fields.attendees[0].address, "bob@example.com");
    assert_eq!(fields.attendees[0].name.as_deref(), Some("Bob"));
    assert_eq!(fields.attendees[0].participation, Participation::Accepted);
    assert_eq!(fields.attendees[1].role, AttendeeRole::Optional);
    assert_eq!(
        fields.reminders.alerts,
        vec![Alert {
            trigger: AlertTrigger::Offset {
                offset: SignedDuration::from_mins(-10),
                relative_to: RelativeTo::Start
            },
            action: AlertAction::Display,
        }]
    );

    // What the model has no place for is carried as written.
    let names: Vec<&str> = master
        .passthrough
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(names, ["DTSTAMP", "SEQUENCE", "X-CUSTOM", "VALARM"]);
    assert_eq!(
        master.passthrough.get("X-CUSTOM").collect::<Vec<_>>(),
        ["X-CUSTOM;X-PARAM=\"a;b\":value"]
    );

    // The audio alarm is the one thing that could not be carried.
    assert_eq!(read.losses.len(), 1, "{:#?}", read.losses);
    assert_eq!(read.losses[0].field, Field::Reminders);
    assert_eq!(read.losses[0].reason, LossReason::Unrepresentable);
}

#[test]
fn reads_overrides_by_recurrence_id() {
    let document = IcalDocument::parse(SERIES.as_bytes());
    let read = read_one(&document);
    let overrides = &read.series.overrides;
    assert_eq!(overrides.len(), 2);

    let moved = InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0)));
    let Override::Modified { event, range } = &overrides[&moved] else {
        panic!("the moved instance is modified");
    };
    assert_eq!(*range, InstanceRange::ThisInstance);
    assert_eq!(event.fields.title.as_deref(), Some("Standup (moved)"));
    assert_eq!(
        event.fields.times.start(),
        &new_york(datetime(2026, 1, 7, 10, 0, 0, 0))
    );
    assert!(event.fields.recurrence.is_none());

    let cancelled = InstanceKey::new(new_york(datetime(2026, 1, 12, 9, 30, 0, 0)));
    assert!(matches!(overrides[&cancelled], Override::Cancelled { .. }));
}

#[test]
fn an_edit_changes_only_the_edited_property() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    series.master.as_mut().unwrap().fields.title = Some("Daily sync".to_owned());
    let losses = document.write(&series, stamp()).unwrap();
    assert!(losses.is_empty(), "{losses:#?}");
    assert_eq!(
        changed_lines(SERIES, &text(&document)),
        [(
            "summary:Standup\r\n".to_owned(),
            "summary:Daily sync\r\n".to_owned()
        )]
    );
}

#[test]
fn an_attendee_edit_keeps_the_parameters_the_model_does_not_map() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    series.master.as_mut().unwrap().fields.attendees[0].participation = Participation::Declined;
    document.write(&series, stamp()).unwrap();
    assert_eq!(
        changed_lines(SERIES, &text(&document)),
        [(
            "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED;RSVP=TRUE;X-NOTE=keep:mailto:bob@example.com\r\n"
                .to_owned(),
            "ATTENDEE;CN=Bob;PARTSTAT=DECLINED;RSVP=TRUE;X-NOTE=keep:mailto:bob@example.com\r\n"
                .to_owned()
        )]
    );
}

#[test]
fn an_override_edit_touches_only_that_override() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    let moved = InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0)));
    let Some(Override::Modified { event, .. }) = series.overrides.get_mut(&moved) else {
        panic!("the moved instance is modified");
    };
    event.fields.times = EventTimes::new(
        new_york(datetime(2026, 1, 7, 11, 0, 0, 0)),
        Some(new_york(datetime(2026, 1, 7, 11, 15, 0, 0))),
    )
    .unwrap();
    document.write(&series, stamp()).unwrap();
    assert_eq!(
        changed_lines(SERIES, &text(&document)),
        [
            (
                "DTSTART;TZID=America/New_York:20260107T100000\r\n".to_owned(),
                "DTSTART;TZID=America/New_York:20260107T110000\r\n".to_owned()
            ),
            (
                "DTEND;TZID=America/New_York:20260107T101500\r\n".to_owned(),
                "DTEND;TZID=America/New_York:20260107T111500\r\n".to_owned()
            )
        ]
    );
}

#[test]
fn adding_cancelling_and_removing_overrides() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    let master = series.master.clone().unwrap();

    // Remove the moved instance, cancel a new one, and move another.
    let moved = InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0)));
    series.overrides.remove(&moved);
    let cancelled = InstanceKey::new(new_york(datetime(2026, 1, 14, 9, 30, 0, 0)));
    series.overrides.insert(
        cancelled.clone(),
        Override::Cancelled {
            ids: None,
            revision: None,
        },
    );
    let added = InstanceKey::new(new_york(datetime(2026, 1, 21, 9, 30, 0, 0)));
    let mut event = master.clone();
    event.fields.recurrence = None;
    event.fields.title = Some("Standup, later".to_owned());
    event.fields.times = EventTimes::new(
        new_york(datetime(2026, 1, 21, 15, 0, 0, 0)),
        Some(new_york(datetime(2026, 1, 21, 15, 15, 0, 0))),
    )
    .unwrap();
    series.overrides.insert(
        added.clone(),
        Override::Modified {
            event: Box::new(event.clone()),
            range: InstanceRange::ThisAndFuture,
        },
    );
    let losses = document.write(&series, stamp()).unwrap();
    assert!(losses.is_empty(), "{losses:#?}");

    let written = text(&document);
    assert!(!written.contains("SUMMARY:Standup (moved)"));
    assert!(
        written.contains(
            "RECURRENCE-ID;TZID=America/New_York;RANGE=THISANDFUTURE:20260121T093000\r\n"
        )
    );
    assert!(written.contains("DTSTAMP:20260601T120000Z\r\n"));
    // The VTIMEZONE was already there.
    assert_eq!(written.matches("BEGIN:VTIMEZONE").count(), 1);

    let reread = read_one(&IcalDocument::parse(written.as_bytes()));
    assert_eq!(reread.series.master.unwrap().fields, master.fields);
    assert_eq!(reread.series.overrides.len(), 3);
    assert!(matches!(
        reread.series.overrides[&cancelled],
        Override::Cancelled { .. }
    ));
    let Override::Modified {
        event: reread_event,
        range,
    } = &reread.series.overrides[&added]
    else {
        panic!("the added instance is modified");
    };
    assert_eq!(*range, InstanceRange::ThisAndFuture);
    assert_eq!(reread_event.fields, event.fields);
}

#[test]
fn a_range_change_rewrites_only_the_recurrence_id() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    let moved = InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0)));
    let Some(Override::Modified { range, .. }) = series.overrides.get_mut(&moved) else {
        panic!("the moved instance is modified");
    };
    *range = InstanceRange::ThisAndFuture;
    let losses = document.write(&series, stamp()).unwrap();
    assert!(losses.is_empty(), "{losses:#?}");
    let widened = text(&document);
    assert_eq!(
        changed_lines(SERIES, &widened),
        [(
            "RECURRENCE-ID;TZID=America/New_York:20260107T093000\r\n".to_owned(),
            "RECURRENCE-ID;TZID=America/New_York;RANGE=THISANDFUTURE:20260107T093000\r\n"
                .to_owned()
        )]
    );
    let reread = read_one(&IcalDocument::parse(widened.as_bytes()));
    assert!(matches!(
        reread.series.overrides[&moved],
        Override::Modified {
            range: InstanceRange::ThisAndFuture,
            ..
        }
    ));

    // And back: the parameter is dropped.
    let Some(Override::Modified { range, .. }) = series.overrides.get_mut(&moved) else {
        panic!("the moved instance is modified");
    };
    *range = InstanceRange::ThisInstance;
    document.write(&series, stamp()).unwrap();
    assert_eq!(text(&document), SERIES);
}

/// A series whose master and one override have no `DTSTART`, so neither reads.
const UNREADABLE: &str = concat!(
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Example Corp//Calendar//EN\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:broken@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "SUMMARY:No start\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:broken@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "RECURRENCE-ID:20260107T093000Z\r\n",
    "SUMMARY:No start either\r\n",
    "END:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

fn unreadable_series() -> (EventSeries, InstanceKey) {
    let mut fields = NormalizedEvent::new(
        EventTimes::new(
            TimeValue::instant("2026-01-05T09:30:00Z".parse().unwrap()),
            Some(TimeValue::instant("2026-01-05T09:45:00Z".parse().unwrap())),
        )
        .unwrap(),
    );
    fields.title = Some("Fixed".to_owned());
    let mut ids = ProviderIds::new(Provider::CalDav, "/calendars/alice/team/standup.ics");
    ids.ical_uid = Some("broken@example.com".to_owned());
    let master = ProviderEvent {
        ids,
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields,
        passthrough: calendar_sync_model::Passthrough::new(),
    };
    let key = InstanceKey::new(TimeValue::instant("2026-01-07T09:30:00Z".parse().unwrap()));
    (
        EventSeries {
            master: Some(master),
            overrides: std::collections::BTreeMap::new(),
        },
        key,
    )
}

#[test]
fn writing_a_master_replaces_an_unreadable_one() {
    let mut document = IcalDocument::parse(UNREADABLE.as_bytes());
    let (series, _) = unreadable_series();
    document.write(&series, stamp()).unwrap();
    let written = text(&document);
    assert!(!written.contains("SUMMARY:No start\r\n"), "{written}");
    // The unreadable override is not the master's to replace.
    assert!(written.contains("SUMMARY:No start either\r\n"), "{written}");
    assert_eq!(written.matches("BEGIN:VEVENT").count(), 2, "{written}");
    let reread = read_one(&IcalDocument::parse(written.as_bytes()));
    assert_eq!(
        reread.series.master.unwrap().fields.title.as_deref(),
        Some("Fixed")
    );
}

#[test]
fn writing_an_override_replaces_an_unreadable_one() {
    for cancelled in [false, true] {
        let mut document = IcalDocument::parse(UNREADABLE.as_bytes());
        let (mut series, key) = unreadable_series();
        let instance = if cancelled {
            Override::Cancelled {
                ids: None,
                revision: None,
            }
        } else {
            let mut event = series.master.clone().unwrap();
            event.fields.title = Some("Moved".to_owned());
            Override::Modified {
                event: Box::new(event),
                range: InstanceRange::ThisInstance,
            }
        };
        series.overrides.insert(key.clone(), instance);
        document.write(&series, stamp()).unwrap();
        let written = text(&document);
        assert!(!written.contains("No start"), "{written}");
        assert_eq!(
            written.matches("RECURRENCE-ID:20260107T093000Z").count(),
            1,
            "{written}"
        );
        let reread = read_one(&IcalDocument::parse(written.as_bytes()));
        assert_eq!(
            matches!(reread.series.overrides[&key], Override::Cancelled { .. }),
            cancelled
        );
    }
}

#[test]
fn removing_the_series_removes_only_its_events() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let uid = read_one(&document).series.master.unwrap().ids;
    // A series known only by an override's ids, with no master and no overrides left.
    let empty = EventSeries {
        master: None,
        overrides: [(
            InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0))),
            Override::Cancelled {
                ids: Some(uid),
                revision: None,
            },
        )]
        .into(),
    };
    document.write(&empty, stamp()).unwrap();
    let written = text(&document);
    assert_eq!(written.matches("BEGIN:VEVENT").count(), 1);
    assert!(written.contains("BEGIN:VTODO"));
    assert!(written.contains("RECURRENCE-ID;TZID=America/New_York:20260107T093000"));
}

#[test]
fn a_new_document_reads_back_what_was_written() {
    let mut document = IcalDocument::parse(b"");
    let mut event = NormalizedEvent::new(
        EventTimes::new(
            TimeValue::zoned(
                datetime(2026, 3, 2, 18, 0, 0, 0),
                TimeZoneId::new("Europe/Berlin"),
            ),
            Some(TimeValue::zoned(
                datetime(2026, 3, 2, 19, 30, 0, 0),
                TimeZoneId::new("Europe/Berlin"),
            )),
        )
        .unwrap(),
    );
    event.title = Some("Choir; tenors, basses\nand altos".to_owned());
    event.recurrence = Some(Recurrence::rule(RecurrenceRule::new(Frequency::Weekly)));
    event.attendees = vec![Attendee::new("dave@example.com")];
    event.reminders.alerts = vec![Alert {
        trigger: AlertTrigger::Offset {
            offset: SignedDuration::from_hours(-1),
            relative_to: RelativeTo::End,
        },
        action: AlertAction::Display,
    }];
    let mut ids = ProviderIds::new(Provider::Google, "google-id");
    ids.ical_uid = Some("choir@example.com".to_owned());
    let series = EventSeries::single(ProviderEvent {
        ids,
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields: event.clone(),
        passthrough: calendar_sync_model::Passthrough::new(),
    });

    // A document with no VCALENDAR cannot take it.
    assert_eq!(
        document.write(&series, stamp()),
        Err(WriteError::NoCalendar)
    );

    document = IcalDocument::new("-//calendar-sync//test//EN");
    let losses = document.write(&series, stamp()).unwrap();
    assert!(losses.is_empty(), "{losses:#?}");
    let written = text(&document);
    assert!(
        written
            .starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//calendar-sync//test//EN\r\n")
    );
    assert!(written.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n"));
    assert!(written.contains("SUMMARY:Choir\\; tenors\\, basses\\nand altos\r\n"));

    let reread = read_one(&IcalDocument::parse(written.as_bytes()));
    assert!(reread.losses.is_empty(), "{:#?}", reread.losses);
    assert_eq!(reread.series.master.unwrap().fields, event);
}

#[test]
fn passthrough_from_an_icalendar_provider_is_written_back() {
    let document = IcalDocument::parse(SERIES.as_bytes());
    let master = read_one(&document).series.master.unwrap();
    let mut fresh = IcalDocument::new("-//calendar-sync//test//EN");
    fresh
        .write(&EventSeries::single(master.clone()), stamp())
        .unwrap();
    let written = text(&fresh);
    assert!(written.contains("X-CUSTOM;X-PARAM=\"a;b\":value\r\n"));
    assert!(written.contains("SEQUENCE:3\r\n"));
    // The passthrough's DTSTAMP is kept, not replaced.
    assert!(written.contains("DTSTAMP:20260101T000000Z\r\n"));
    assert!(!written.contains("DTSTAMP:20260601T120000Z"));
    assert!(written.contains("ACTION:AUDIO\r\n"));
    let reread = read_one(&IcalDocument::parse(written.as_bytes()));
    assert_eq!(reread.series.master.unwrap().fields, master.fields);
}

#[test]
fn a_series_without_a_uid_is_refused() {
    let mut document = IcalDocument::new("-//calendar-sync//test//EN");
    let series = EventSeries::single(ProviderEvent {
        ids: ProviderIds::new(Provider::Graph, "graph-id"),
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields: NormalizedEvent::new(EventTimes::at(TimeValue::all_day(date(2026, 1, 1)))),
        passthrough: calendar_sync_model::Passthrough::new(),
    });
    assert_eq!(document.write(&series, stamp()), Err(WriteError::NoUid));
}

const VARIANTS: &str = concat!(
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Example//EN\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:all-day\r\n",
    "DTSTART;VALUE=DATE:20260301\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:floating\r\n",
    "DTSTART:20260301T090000\r\n",
    "DURATION:PT1H30M\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:instant\r\n",
    "DTSTART:20260301T090000Z\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:prefixed\r\n",
    "DTSTART;TZID=/mozilla.org/20050126_1/Europe/Paris:20260301T090000\r\n",
    "DURATION:P1D\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:unknown-zone\r\n",
    "DTSTART;TZID=America-SanJose:20260301T090000\r\n",
    "RDATE;VALUE=PERIOD:20260302T090000Z/PT1H\r\n",
    "RDATE:20260303T090000Z\r\n",
    "END:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

#[test]
fn reads_each_time_variant() {
    let read = IcalDocument::parse(VARIANTS.as_bytes()).read(&Source::feed());
    let times: Vec<(&str, EventTimes)> = read
        .series
        .iter()
        .map(|series| {
            (
                series.uid.as_str(),
                series.series.master.as_ref().unwrap().fields.times.clone(),
            )
        })
        .collect();
    let paris = |hour| {
        TimeValue::zoned(
            datetime(2026, 3, 1, hour, 0, 0, 0),
            TimeZoneId::new("Europe/Paris"),
        )
    };
    assert_eq!(
        times,
        [
            (
                "all-day",
                EventTimes::new(
                    TimeValue::all_day(date(2026, 3, 1)),
                    Some(TimeValue::all_day(date(2026, 3, 2)))
                )
                .unwrap()
            ),
            (
                "floating",
                EventTimes::new(
                    TimeValue::floating(datetime(2026, 3, 1, 9, 0, 0, 0)),
                    Some(TimeValue::floating(datetime(2026, 3, 1, 10, 30, 0, 0)))
                )
                .unwrap()
            ),
            (
                "instant",
                EventTimes::at(TimeValue::instant("2026-03-01T09:00:00Z".parse().unwrap()))
            ),
            (
                "prefixed",
                EventTimes::new(
                    paris(9),
                    Some(TimeValue::zoned(
                        datetime(2026, 3, 2, 9, 0, 0, 0),
                        TimeZoneId::new("Europe/Paris")
                    ))
                )
                .unwrap()
            ),
            (
                "unknown-zone",
                EventTimes::at(TimeValue::zoned(
                    datetime(2026, 3, 1, 9, 0, 0, 0),
                    TimeZoneId::new("America-SanJose")
                ))
            ),
        ]
    );
    // A feed identifies each event by its UID.
    let feed_ids = &read.series[0].series.master.as_ref().unwrap().ids;
    assert_eq!(feed_ids.provider, Provider::Ics);
    assert_eq!(feed_ids.id, "all-day");

    let unknown = &read.series[4];
    let reasons: Vec<(Field, LossReason)> = unknown
        .losses
        .iter()
        .map(|loss| (loss.field.clone(), loss.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (Field::Recurrence, LossReason::Unrepresentable),
            (Field::Times, LossReason::Unsupported)
        ],
        "{:#?}",
        unknown.losses
    );
}

#[test]
fn an_rdate_period_survives_an_edit_of_the_other_dates() {
    let mut document = IcalDocument::parse(VARIANTS.as_bytes());
    let mut series = document.read(&Source::feed()).series.remove(4).series;
    let recurrence = series
        .master
        .as_mut()
        .unwrap()
        .fields
        .recurrence
        .as_mut()
        .unwrap();
    recurrence.dates = vec![TimeValue::instant("2026-03-04T09:00:00Z".parse().unwrap())];
    document.write(&series, stamp()).unwrap();
    let written = text(&document);
    assert!(written.contains("RDATE;VALUE=PERIOD:20260302T090000Z/PT1H\r\n"));
    assert!(written.contains("RDATE:20260304T090000Z\r\n"));
    assert!(!written.contains("RDATE:20260303T090000Z"));
}

#[test]
fn a_moved_start_moves_the_end_a_duration_describes() {
    let mut document = IcalDocument::parse(VARIANTS.as_bytes());
    let mut series = document.read(&Source::feed()).series.remove(1).series;
    let fields = &mut series.master.as_mut().unwrap().fields;
    // The start moves; the end, which `DURATION:PT1H30M` placed at 10:30, does not.
    let end = fields.times.end().cloned();
    assert_eq!(
        end,
        Some(TimeValue::floating(datetime(2026, 3, 1, 10, 30, 0, 0)))
    );
    fields.times =
        EventTimes::new(TimeValue::floating(datetime(2026, 3, 1, 10, 0, 0, 0)), end).unwrap();
    let wanted = fields.times.clone();
    document.write(&series, stamp()).unwrap();
    let written = text(&document);
    assert!(!written.contains("DURATION:PT1H30M"));
    assert!(written.contains("DTEND:20260301T103000\r\n"));
    let reread = IcalDocument::parse(&document.to_bytes())
        .read(&Source::feed())
        .series
        .remove(1);
    assert_eq!(reread.series.master.unwrap().fields.times, wanted);
}

#[test]
fn a_feed_with_broken_lines_is_still_read() {
    let feed = concat!(
        "BEGIN:VCALENDAR\r\n",
        "VERSION:2.0\r\n",
        "this line has no colon\r\n",
        "BEGIN:VEVENT\r\n",
        "UID:one\r\n",
        "DTSTART:20260301T090000Z\r\n",
        "STATUS:MAYBE\r\n",
        "END:VEVENT\r\n",
        "BEGIN:VEVENT\r\n",
        "DTSTART:20260301T090000Z\r\n",
        "SUMMARY:No UID\r\n",
        "END:VEVENT\r\n",
        "BEGIN:VEVENT\r\n",
        "UID:two\r\n",
        "SUMMARY:No start\r\n",
        "END:VEVENT\r\n",
        "END:VCALENDAR\r\n",
    );
    let read = read_feed(feed.as_bytes());
    assert_eq!(read.series.len(), 2);
    assert_eq!(read.series[0].uid, "one");
    assert_eq!(read.series[0].losses[0].reason, LossReason::Approximated);
    assert!(read.series[1].series.is_empty());
    assert_eq!(read.series[1].losses[0].reason, LossReason::Malformed);
    let document_losses: Vec<&str> = read
        .losses
        .iter()
        .map(|loss| loss.detail.as_str())
        .collect();
    assert_eq!(document_losses.len(), 2, "{document_losses:#?}");
    assert!(document_losses[1].contains("no UID"));
    assert_eq!(
        IcalDocument::parse(feed.as_bytes()).to_bytes(),
        feed.as_bytes()
    );
}

#[test]
fn an_override_cannot_carry_its_own_recurrence() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    let moved = InstanceKey::new(new_york(datetime(2026, 1, 7, 9, 30, 0, 0)));
    let Some(Override::Modified { event, .. }) = series.overrides.get_mut(&moved) else {
        panic!("the moved instance is modified");
    };
    event.fields.recurrence = Some(Recurrence::rule(RecurrenceRule::new(Frequency::Daily)));
    let losses = document.write(&series, stamp()).unwrap();
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].field, Field::Recurrence);
    assert_eq!(text(&document), SERIES);
}

#[test]
fn control_characters_in_text_are_reported() {
    let mut document = IcalDocument::parse(SERIES.as_bytes());
    let mut series = read_one(&document).series;
    series.master.as_mut().unwrap().fields.title = Some("bell\u{7} and\r\nbreak".to_owned());
    let losses = document.write(&series, stamp()).unwrap();
    assert_eq!(losses.len(), 1, "{losses:#?}");
    assert_eq!(
        (losses[0].field.clone(), losses[0].reason),
        (Field::Title, LossReason::Approximated)
    );
    assert!(text(&document).contains("summary:bell and\\nbreak\r\n"));
}
