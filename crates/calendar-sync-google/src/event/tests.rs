use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, AttendeeRole, EventStatus, Field, ParticipantKind,
    Participation, Provider, ProviderEvent, RelativeTo, TimeValue, TimeZoneId, Transparency,
    Visibility,
};
use jiff::SignedDuration;
use jiff::civil::date;
use serde_json::{Value, json};

use super::{Item, read, write};

fn event(value: &Value) -> ProviderEvent {
    match read(value).expect("reads").item {
        Item::Event(event) => event,
        Item::Deleted { id } => panic!("{id} read as deleted"),
    }
}

/// An event as Google returns it, with fields this mapping does not read at the top level, in the
/// organizer, and in an attendee.
fn full() -> Value {
    json!({
        "kind": "calendar#event",
        "etag": "\"3181161784712000\"",
        "id": "evt001",
        "status": "confirmed",
        "htmlLink": "https://www.google.com/calendar/event?eid=ZXZ0MDAx",
        "created": "2026-10-01T12:00:00.000Z",
        "updated": "2026-10-02T12:00:00.000Z",
        "summary": "Planning",
        "description": "Quarterly planning",
        "location": "Room 4",
        "colorId": "5",
        "creator": { "email": "person1@example.com", "self": true },
        "organizer": { "email": "person1@example.com", "displayName": "Person One", "self": true },
        "start": { "dateTime": "2026-11-05T10:00:00-05:00", "timeZone": "America/Toronto" },
        "end": { "dateTime": "2026-11-05T11:00:00-05:00", "timeZone": "America/Toronto" },
        "transparency": "transparent",
        "visibility": "private",
        "iCalUID": "evt001@google.com",
        "sequence": 2,
        "attendees": [
            {
                "email": "person2@example.com",
                "displayName": "Person Two",
                "optional": true,
                "responseStatus": "accepted",
                "comment": "Running late",
                "additionalGuests": 1
            },
            { "email": "room@example.com", "resource": true, "responseStatus": "tentative" }
        ],
        "extendedProperties": { "private": { "calendarSyncEdit": "e1" } },
        "conferenceData": { "conferenceId": "abc-defg-hij", "futureField": [1, 2, 3] },
        "reminders": {
            "useDefault": false,
            "overrides": [ { "method": "popup", "minutes": 10 }, { "method": "email", "minutes": 1440 } ]
        },
        "eventType": "default",
        "someFieldFromTheFuture": { "nested": true }
    })
}

#[test]
fn reads_every_mapped_field() {
    let read = read(&full()).unwrap();
    assert!(read.losses.is_empty(), "{:?}", read.losses);
    let Item::Event(event) = read.item else {
        panic!("deleted");
    };
    assert_eq!(event.ids.provider, Provider::Google);
    assert_eq!(event.ids.id, "evt001");
    assert_eq!(event.ids.ical_uid.as_deref(), Some("evt001@google.com"));
    assert_eq!(event.revision.unwrap().as_str(), "\"3181161784712000\"");
    let fields = event.fields;
    assert_eq!(fields.title.as_deref(), Some("Planning"));
    assert_eq!(fields.location.as_deref(), Some("Room 4"));
    assert_eq!(
        fields.times.start(),
        &TimeValue::zoned(
            date(2026, 11, 5).at(10, 0, 0, 0),
            TimeZoneId::new("America/Toronto")
        )
    );
    assert_eq!(fields.transparency, Transparency::Transparent);
    assert_eq!(fields.visibility, Visibility::Private);
    assert_eq!(
        fields.organizer.unwrap().name.as_deref(),
        Some("Person One")
    );
    assert_eq!(fields.attendees.len(), 2);
    assert_eq!(fields.attendees[0].role, AttendeeRole::Optional);
    assert_eq!(fields.attendees[0].participation, Participation::Accepted);
    assert_eq!(fields.attendees[1].kind, ParticipantKind::Resource);
    assert_eq!(
        fields.reminders.alerts,
        vec![
            Alert {
                trigger: AlertTrigger::Offset {
                    offset: SignedDuration::from_mins(-10),
                    relative_to: RelativeTo::Start
                },
                action: AlertAction::Display
            },
            Alert {
                trigger: AlertTrigger::Offset {
                    offset: SignedDuration::from_mins(-1440),
                    relative_to: RelativeTo::Start
                },
                action: AlertAction::Email
            },
        ]
    );
    let kept: Vec<&str> = event
        .passthrough
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    for name in [
        "kind",
        "htmlLink",
        "colorId",
        "conferenceData",
        "extendedProperties",
        "someFieldFromTheFuture",
        "organizer.self",
        "attendees[person2@example.com].comment",
        "attendees[person2@example.com].additionalGuests",
    ] {
        assert!(kept.contains(&name), "{name} not kept in {kept:?}");
    }
}

#[test]
fn unknown_fields_survive_a_read_modify_write() {
    let original = full();
    let mut event = event(&original);
    event.fields.title = Some("Planning, moved".to_owned());
    let written = write(&event);
    assert!(written.losses.is_empty(), "{:?}", written.losses);
    let body = Value::Object(written.body);

    // The edit landed.
    assert_eq!(body["summary"], "Planning, moved");
    // Every top-level field the mapping does not read is back, unchanged.
    for name in [
        "kind",
        "htmlLink",
        "created",
        "updated",
        "colorId",
        "creator",
        "sequence",
        "extendedProperties",
        "conferenceData",
        "eventType",
        "someFieldFromTheFuture",
    ] {
        assert_eq!(body[name], original[name], "{name}");
    }
    // So are the organizer's and each attendee's.
    assert_eq!(body["organizer"], original["organizer"]);
    assert_eq!(body["attendees"], original["attendees"]);
    assert_eq!(body["reminders"], original["reminders"]);
    // `id` and `etag` travel in the path and `If-Match`, not the body.
    assert!(body.get("id").is_none());
    assert!(body.get("etag").is_none());

    // Reading the written body back gives the edited event.
    let mut again = body.clone();
    again["id"] = json!("evt001");
    again["etag"] = original["etag"].clone();
    again["iCalUID"] = original["iCalUID"].clone();
    assert_eq!(self::event(&again), event);
}

#[test]
fn a_cancelled_event_outside_a_series_is_a_deletion() {
    let read = read(&json!({ "id": "evt9", "status": "cancelled", "etag": "\"9\"" })).unwrap();
    assert_eq!(
        read.item,
        Item::Deleted {
            id: "evt9".to_owned()
        }
    );
}

#[test]
fn a_cancelled_instance_is_an_event_at_its_original_start() {
    let event = event(&json!({
        "kind": "calendar#event",
        "id": "series1_20261112T150000Z",
        "etag": "\"7\"",
        "status": "cancelled",
        "recurringEventId": "series1",
        "originalStartTime": { "dateTime": "2026-11-12T10:00:00-05:00", "timeZone": "America/Toronto" }
    }));
    assert_eq!(event.ids.series_id.as_deref(), Some("series1"));
    assert_eq!(event.fields.status, EventStatus::Cancelled);
    assert_eq!(
        event.fields.times.start(),
        &TimeValue::zoned(
            date(2026, 11, 12).at(10, 0, 0, 0),
            TimeZoneId::new("America/Toronto")
        )
    );
    assert_eq!(event.passthrough.get("originalStartTime").count(), 1);
}

#[test]
fn a_live_event_needs_an_id_and_a_start() {
    assert!(read(&json!({ "summary": "x" })).is_err());
    assert!(read(&json!({ "id": "x", "status": "confirmed" })).is_err());
    assert!(read(&json!([])).is_err());
}

#[test]
fn unknown_enumerations_and_reminder_methods_are_reported() {
    let read = read(&json!({
        "id": "x",
        "status": "postponed",
        "start": { "date": "2026-11-05" },
        "end": { "date": "2026-11-06" },
        "reminders": { "useDefault": false, "overrides": [ { "method": "sms", "minutes": 5 } ] }
    }))
    .unwrap();
    let fields: Vec<&Field> = read.losses.iter().map(|loss| &loss.field).collect();
    assert_eq!(fields, vec![&Field::Status, &Field::Reminders]);
}

#[test]
fn what_google_cannot_hold_is_approximated_and_reported() {
    let mut event = event(&full());
    event.fields.attendees[0].role = AttendeeRole::Chair;
    event.fields.attendees[0].participation = Participation::Delegated;
    event.fields.reminders.alerts.push(Alert {
        trigger: AlertTrigger::Offset {
            offset: SignedDuration::from_mins(5),
            relative_to: RelativeTo::Start,
        },
        action: AlertAction::Display,
    });
    event.fields.times =
        calendar_sync_model::EventTimes::at(TimeValue::floating(date(2026, 11, 5).at(9, 0, 0, 0)));
    let written = write(&event);
    assert_eq!(written.losses.len(), 5, "{:?}", written.losses);
    let body = Value::Object(written.body);
    assert_eq!(body["attendees"][0]["responseStatus"], "needsAction");
    assert!(body["attendees"][0].get("optional").is_none());
    assert_eq!(body["reminders"]["overrides"].as_array().unwrap().len(), 2);
    assert_eq!(body["start"], body["end"]);
}

#[test]
fn another_providers_passthrough_is_not_written() {
    let mut event = event(&full());
    event.ids.provider = Provider::CalDav;
    let body = write(&event).body;
    assert!(body.get("htmlLink").is_none());
    assert!(body.get("summary").is_some());
}

#[test]
fn a_recurring_master_writes_its_rule() {
    let original = json!({
        "id": "series1",
        "etag": "\"1\"",
        "summary": "Standup",
        "start": { "dateTime": "2026-11-03T10:00:00-05:00", "timeZone": "America/Toronto" },
        "end": { "dateTime": "2026-11-03T10:15:00-05:00", "timeZone": "America/Toronto" },
        "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU,TH"]
    });
    let event = event(&original);
    assert!(event.fields.recurrence.is_some());
    let body = Value::Object(write(&event).body);
    assert_eq!(body["recurrence"], json!(["RRULE:FREQ=WEEKLY;BYDAY=TU,TH"]));
}

#[test]
fn a_recurring_instant_is_written_with_a_zone() {
    let original = json!({
        "id": "series2",
        "etag": "\"1\"",
        "start": { "dateTime": "2026-11-03T15:00:00Z" },
        "end": { "dateTime": "2026-11-03T15:15:00Z" },
        "recurrence": ["RRULE:FREQ=DAILY;COUNT=3"]
    });
    let event = event(&original);
    assert!(matches!(
        event.fields.times.start(),
        TimeValue::Instant { .. }
    ));
    let written = write(&event);
    assert_eq!(written.losses.len(), 2, "{:?}", written.losses);
    let body = Value::Object(written.body);
    assert_eq!(
        body["start"],
        json!({ "dateTime": "2026-11-03T15:00:00Z", "timeZone": "UTC" })
    );
    assert_eq!(
        body["end"],
        json!({ "dateTime": "2026-11-03T15:15:00Z", "timeZone": "UTC" })
    );

    let mut single = event;
    single.fields.recurrence = None;
    let body = Value::Object(write(&single).body);
    assert_eq!(body["start"], json!({ "dateTime": "2026-11-03T15:00:00Z" }));
}
