use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, EventTimes, Field,
    Frequency, LossReason, NDay, NormalizedEvent, Organizer, ParticipantKind, Participation,
    Passthrough, Provider, ProviderEvent, ProviderIds, ProviderRevision, Recurrence,
    RecurrenceRule, RelativeTo, TimeValue, TimeZoneId, Transparency, Visibility, Weekday,
};
use jiff::SignedDuration;
use jiff::civil::date;
use serde_json::{Value, json};

use super::{Item, MAPPER_VERSION, read, write};

fn new_york(d: i8, h: i8) -> TimeValue {
    TimeValue::zoned(
        date(2026, 11, d).at(h, 0, 0, 0),
        TimeZoneId::new("America/New_York"),
    )
}

fn sample() -> Value {
    json!({
        "@odata.etag": "W/\"DwAAABYAAABQ\"",
        "id": "AAMkAGI2TGuLAAA=",
        "changeKey": "DwAAABYAAABQ",
        "iCalUId": "040000008200E00074C5B7101A82E008",
        "subject": "Planning",
        "body": { "contentType": "text", "content": "Agenda" },
        "location": { "displayName": "Room 1", "locationType": "default" },
        "start": { "dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC" },
        "end": { "dateTime": "2026-11-10T16:00:00.0000000", "timeZone": "UTC" },
        "originalStartTimeZone": "Eastern Standard Time",
        "originalEndTimeZone": "Eastern Standard Time",
        "isAllDay": false,
        "isCancelled": false,
        "showAs": "busy",
        "sensitivity": "private",
        "isReminderOn": true,
        "reminderMinutesBeforeStart": 15,
        "importance": "high",
        "categories": ["Blue category"],
        "type": "singleInstance",
        "organizer": { "emailAddress": { "name": "Person One", "address": "person1@example.com" } },
        "attendees": [
            {
                "type": "required",
                "status": { "response": "accepted", "time": "2026-11-01T12:00:00Z" },
                "emailAddress": { "name": "Person Two", "address": "person2@example.com" }
            },
            {
                "type": "optional",
                "status": { "response": "tentativelyAccepted", "time": "0001-01-01T00:00:00Z" },
                "emailAddress": { "address": "person3@example.com" },
                "proposedNewTime": null
            },
            {
                "type": "resource",
                "status": { "response": "none" },
                "emailAddress": { "name": "Projector", "address": "projector@example.com" }
            }
        ]
    })
}

fn event(read: super::ReadEvent) -> ProviderEvent {
    match read.item {
        Item::Event(event) => event,
        Item::Removed { id } => panic!("removed {id}"),
    }
}

#[test]
fn reads_every_mapped_property() {
    let read = read(&sample()).unwrap();
    assert!(read.losses.is_empty(), "{:?}", read.losses);
    let event = event(read);
    assert_eq!(event.ids.provider, Provider::Graph);
    assert_eq!(event.ids.id, "AAMkAGI2TGuLAAA=");
    assert_eq!(
        event.ids.ical_uid.as_deref(),
        Some("040000008200E00074C5B7101A82E008")
    );
    assert_eq!(event.ids.series_id, None);
    assert_eq!(event.revision, Some(ProviderRevision::new("DwAAABYAAABQ")));
    assert_eq!(event.mapper_version, MAPPER_VERSION);
    let fields = &event.fields;
    assert_eq!(fields.title.as_deref(), Some("Planning"));
    assert_eq!(fields.description.as_deref(), Some("Agenda"));
    assert_eq!(fields.location.as_deref(), Some("Room 1"));
    assert_eq!(fields.times.start(), &new_york(10, 10));
    assert_eq!(fields.times.end(), Some(&new_york(10, 11)));
    assert_eq!(fields.transparency, Transparency::Opaque);
    assert_eq!(fields.status, EventStatus::Confirmed);
    assert_eq!(fields.visibility, Visibility::Private);
    assert_eq!(
        fields.organizer,
        Some(Organizer {
            address: "person1@example.com".to_owned(),
            name: Some("Person One".to_owned()),
        })
    );
    let [two, three, projector] = &fields.attendees[..] else {
        panic!("{:?}", fields.attendees)
    };
    assert_eq!(two.participation, Participation::Accepted);
    assert_eq!(two.role, AttendeeRole::Required);
    assert_eq!(three.role, AttendeeRole::Optional);
    assert_eq!(three.participation, Participation::Tentative);
    assert_eq!(three.name, None);
    assert_eq!(projector.kind, ParticipantKind::Resource);
    assert_eq!(projector.participation, Participation::NeedsAction);
    assert_eq!(
        fields.reminders.alerts,
        [Alert {
            trigger: AlertTrigger::Offset {
                offset: SignedDuration::from_mins(-15),
                relative_to: RelativeTo::Start,
            },
            action: AlertAction::Display,
        }]
    );
    // Unmapped properties are kept, nested ones under their parent.
    let kept = |name: &str| event.passthrough.get(name).next().map(str::to_owned);
    assert_eq!(kept("importance").as_deref(), Some("\"high\""));
    assert_eq!(kept("type").as_deref(), Some("\"singleInstance\""));
    assert_eq!(
        kept("originalStartTimeZone").as_deref(),
        Some("\"Eastern Standard Time\"")
    );
    assert_eq!(
        kept("location.locationType").as_deref(),
        Some("\"default\"")
    );
    assert_eq!(
        kept("attendees[person3@example.com].proposedNewTime").as_deref(),
        Some("null")
    );
    assert!(kept("subject").is_none() && kept("attendees").is_none());
}

#[test]
fn a_removed_row_is_a_removal() {
    let read = read(&json!({ "id": "AAMk=", "@removed": { "reason": "deleted" } })).unwrap();
    assert_eq!(
        read.item,
        Item::Removed {
            id: "AAMk=".to_owned()
        }
    );
}

#[test]
fn malformed_resources_are_refused() {
    assert!(read(&json!([])).is_err());
    assert!(read(&json!({ "subject": "no id" })).is_err());
    assert!(read(&json!({ "id": "a" })).is_err());
    assert!(
        read(&json!({ "id": "a", "start": { "dateTime": "soon", "timeZone": "UTC" } })).is_err()
    );
}

#[test]
fn status_transparency_and_sensitivity_values() {
    let with = |extra: Value| {
        let mut value = sample();
        for (name, field) in extra.as_object().unwrap() {
            value[name] = field.clone();
        }
        read(&value).unwrap()
    };
    let free = event(with(json!({ "showAs": "free" })));
    assert_eq!(free.fields.transparency, Transparency::Transparent);
    let tentative = event(with(json!({ "showAs": "tentative" })));
    assert_eq!(tentative.fields.status, EventStatus::Tentative);
    assert_eq!(tentative.fields.transparency, Transparency::Opaque);
    let cancelled = event(with(json!({ "isCancelled": true })));
    assert_eq!(cancelled.fields.status, EventStatus::Cancelled);
    let personal = with(json!({ "sensitivity": "personal" }));
    assert_eq!(personal.losses[0].reason, LossReason::Approximated);
    assert_eq!(event(personal).fields.visibility, Visibility::Private);
    let odd = with(json!({ "showAs": "sometimes", "sensitivity": "secret" }));
    assert_eq!(odd.losses.len(), 2);
    let html = with(json!({ "body": { "contentType": "html", "content": "<p>x</p>" } }));
    assert_eq!(html.losses[0].field, Field::Description);
    let silent = event(with(json!({ "isReminderOn": false })));
    assert!(silent.fields.reminders.alerts.is_empty());
    let empty = event(with(
        json!({ "subject": "", "body": { "contentType": "text", "content": "" } }),
    ));
    assert_eq!(empty.fields.title, None);
    assert_eq!(empty.fields.description, None);
}

#[test]
fn an_all_day_series_master_reads_its_pattern() {
    let value = json!({
        "id": "master",
        "changeKey": "ck1",
        "type": "seriesMaster",
        "isAllDay": true,
        "start": { "dateTime": "2026-11-02T00:00:00.0000000", "timeZone": "UTC" },
        "end": { "dateTime": "2026-11-03T00:00:00.0000000", "timeZone": "UTC" },
        "recurrence": {
            "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["monday"], "firstDayOfWeek": "sunday" },
            "range": { "type": "numbered", "numberOfOccurrences": 4, "startDate": "2026-11-02" }
        }
    });
    let event = event(read(&value).unwrap());
    assert!(event.fields.times.is_all_day());
    let rule = &event.fields.recurrence.as_ref().unwrap().rules[0];
    assert_eq!(rule.frequency, Frequency::Weekly);
    assert_eq!(rule.by_day, [NDay::every(Weekday::Monday)]);
}

#[test]
fn writes_every_mapped_property_and_nothing_kept() {
    let event = event(read(&sample()).unwrap());
    let written = write(&event);
    assert!(written.losses.is_empty(), "{:?}", written.losses);
    let body = Value::Object(written.body);
    assert_eq!(body["subject"], "Planning");
    assert_eq!(
        body["body"],
        json!({ "contentType": "text", "content": "Agenda" })
    );
    assert_eq!(body["location"], json!({ "displayName": "Room 1" }));
    assert_eq!(
        body["start"],
        json!({ "dateTime": "2026-11-10T10:00:00", "timeZone": "Eastern Standard Time" })
    );
    assert_eq!(body["isAllDay"], false);
    assert_eq!(body["recurrence"], Value::Null);
    assert_eq!(body["showAs"], "busy");
    assert_eq!(body["sensitivity"], "private");
    assert_eq!(body["isReminderOn"], true);
    assert_eq!(body["reminderMinutesBeforeStart"], 15);
    assert_eq!(
        body["attendees"][1],
        json!({ "emailAddress": { "address": "person3@example.com" }, "type": "optional" })
    );
    assert_eq!(body["attendees"][2]["type"], "resource");
    // Nothing from the passthrough, and no attendee reply.
    assert!(body.get("importance").is_none());
    assert!(body.get("id").is_none() && body.get("changeKey").is_none());
    assert!(body["attendees"][0].get("status").is_none());
    // The written body reads back to the same fields.
    let mut again = body.clone();
    again["id"] = json!("x");
    again["originalStartTimeZone"] = json!("Eastern Standard Time");
    again["originalEndTimeZone"] = json!("Eastern Standard Time");
    let reread = read(&again).unwrap();
    let mut fields = reread_fields(reread);
    // Replies are not written, so they do not come back.
    for attendee in &mut fields.attendees {
        attendee.participation = Participation::NeedsAction;
    }
    let mut expected = event.fields.clone();
    for attendee in &mut expected.attendees {
        attendee.participation = Participation::NeedsAction;
    }
    assert_eq!(fields, expected);
}

fn reread_fields(read: super::ReadEvent) -> NormalizedEvent {
    event(read).fields
}

fn new_event(fields: NormalizedEvent) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "elsewhere"),
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    }
}

#[test]
fn what_graph_cannot_hold_is_approximated_and_reported() {
    let mut fields = NormalizedEvent::new(EventTimes::at(TimeValue::floating(
        date(2026, 11, 10).at(9, 0, 0, 0),
    )));
    fields.status = EventStatus::Cancelled;
    fields.visibility = Visibility::Public;
    let mut chair = Attendee::new("chair@example.com");
    chair.role = AttendeeRole::Chair;
    let mut room = Attendee::new("room@example.com");
    room.kind = ParticipantKind::Room;
    let mut group = Attendee::new("group@example.com");
    group.kind = ParticipantKind::Group;
    let mut observer = Attendee::new("observer@example.com");
    observer.role = AttendeeRole::NonParticipant;
    fields.attendees = vec![chair, room, group, observer];
    let at = |minutes: i64, action: AlertAction| Alert {
        trigger: AlertTrigger::Offset {
            offset: SignedDuration::from_mins(minutes),
            relative_to: RelativeTo::Start,
        },
        action,
    };
    fields.reminders.use_default = true;
    fields.reminders.alerts = vec![
        at(-10, AlertAction::Email),
        at(-5, AlertAction::Display),
        at(5, AlertAction::Display),
        Alert {
            trigger: AlertTrigger::Offset {
                offset: SignedDuration::from_mins(-5),
                relative_to: RelativeTo::End,
            },
            action: AlertAction::Display,
        },
    ];
    let written = write(&new_event(fields));
    let body = Value::Object(written.body.clone());
    assert_eq!(body["start"]["timeZone"], "UTC");
    assert_eq!(body["end"], body["start"]);
    assert_eq!(body["sensitivity"], "normal");
    assert_eq!(body["attendees"][0]["type"], "required");
    assert_eq!(body["attendees"][1]["type"], "resource");
    assert_eq!(body["attendees"][2]["type"], "required");
    assert_eq!(body["attendees"][3]["type"], "optional");
    assert_eq!(body["reminderMinutesBeforeStart"], 10);
    let count = |field: Field| {
        written
            .losses
            .iter()
            .filter(|loss| loss.field == field)
            .count()
    };
    assert_eq!(count(Field::Times), 2, "{:?}", written.losses);
    assert_eq!(count(Field::Status), 1);
    assert_eq!(count(Field::Visibility), 1);
    assert_eq!(count(Field::Attendees), 4);
    // Default reminders, an email, a second reminder, one after the start, one from the end.
    assert_eq!(count(Field::Reminders), 5, "{:?}", written.losses);
    assert!(!written.refuses_recurrence());
}

#[test]
fn a_recurrence_graph_cannot_hold_is_refused() {
    let start = TimeValue::all_day(date(2026, 11, 2));
    let mut fields = NormalizedEvent::new(
        EventTimes::new(start.clone(), Some(TimeValue::all_day(date(2026, 11, 3)))).unwrap(),
    );
    let mut rule = RecurrenceRule::new(Frequency::Monthly);
    rule.by_day = vec![NDay::nth(Weekday::Monday, 5)];
    fields.recurrence = Some(Recurrence::rule(rule));
    let written = write(&new_event(fields.clone()));
    assert!(written.refuses_recurrence());
    assert_eq!(written.body["recurrence"], Value::Null);
    assert_eq!(written.body["isAllDay"], true);
    fields.recurrence = Some(Recurrence::rule(RecurrenceRule::new(Frequency::Daily)));
    let written = write(&new_event(fields));
    assert!(!written.refuses_recurrence());
    assert_eq!(written.body["recurrence"]["pattern"]["type"], "daily");
    assert_eq!(
        written.body["recurrence"]["range"]["startDate"],
        "2026-11-02"
    );
}
