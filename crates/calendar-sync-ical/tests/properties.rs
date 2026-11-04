//! Properties: parsing never panics and keeps every byte, writing back what was read changes
//! nothing, and an event written reads back as itself, also when written over another.

use core::num::NonZeroU32;

use calendar_sync_ical::{IcalDocument, MAPPER_VERSION, Source};
use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventSeries, EventStatus, EventTimes,
    Field, Frequency, NDay, NormalizedEvent, Organizer, ParticipantKind, Participation,
    Passthrough, Provider, ProviderEvent, ProviderIds, Recurrence, RecurrenceRule, RelativeTo,
    Reminders, RuleEnd, TimeValue, TimeZoneId, Transparency, Visibility, Weekday,
};
use jiff::civil::{Date, DateTime};
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;

fn stamp() -> Timestamp {
    "2026-06-01T12:00:00Z".parse().unwrap()
}

/// Writes every series a document reads back into it.
fn write_back(bytes: &[u8]) -> Vec<u8> {
    let mut document = IcalDocument::parse(bytes);
    for series in document.read(&Source::feed()).series {
        // A series with no event left to carry a UID cannot be written; that is not a change.
        let _ = document.write(&series.series, stamp());
    }
    document.to_bytes()
}

/// Content lines a calendar is assembled from: well formed, malformed, and in between.
fn content_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("UID:a".to_owned()),
        Just("UID:b".to_owned()),
        Just("DTSTART:20260301T090000Z".to_owned()),
        Just("DTSTART;VALUE=DATE:20260301".to_owned()),
        Just("DTSTART;TZID=Europe/Berlin:20260301T090000".to_owned()),
        Just("DTSTART;TZID=Nowhere/Special:20260301T090000".to_owned()),
        Just("DTSTART:2026".to_owned()),
        Just("DTEND:20260301T100000Z".to_owned()),
        Just("DTEND;VALUE=DATE:20260228".to_owned()),
        Just("DURATION:PT1H".to_owned()),
        Just("DURATION:-P1D".to_owned()),
        Just("DURATION:nonsense".to_owned()),
        Just("RECURRENCE-ID:20260308T090000Z".to_owned()),
        Just("RECURRENCE-ID;RANGE=THISANDFUTURE:20260315T090000Z".to_owned()),
        Just("RECURRENCE-ID:garbage".to_owned()),
        Just("RRULE:FREQ=WEEKLY;COUNT=5;BYDAY=MO,-1FR".to_owned()),
        Just("RRULE:FREQ=MONTHLY;UNTIL=20270101;COUNT=3;X-Y=1".to_owned()),
        Just("RRULE:BYDAY=MO".to_owned()),
        Just("EXRULE:FREQ=DAILY;INTERVAL=2".to_owned()),
        Just("RDATE;VALUE=PERIOD:20260302T090000Z/PT1H,20260303T090000Z".to_owned()),
        Just("EXDATE:20260308T090000Z,bad".to_owned()),
        Just("STATUS:CANCELLED".to_owned()),
        Just("STATUS:TENTATIVE".to_owned()),
        Just("TRANSP:TRANSPARENT".to_owned()),
        Just("CLASS:X-SECRET".to_owned()),
        Just("ORGANIZER;CN=\"A, B\":mailto:a@example.com".to_owned()),
        Just("ATTENDEE;PARTSTAT=X-MAYBE;ROLE=CHAIR:mailto:b@example.com".to_owned()),
        Just("ATTENDEE:urn:uuid:1234".to_owned()),
        Just("X-THING;X-P=^n^':value".to_owned()),
        Just("no colon here".to_owned()),
        "[A-Z-]{1,8}(;[A-Z]{1,4}=[a-z\"]{0,4}){0,2}:[ -~]{0,90}",
        "SUMMARY:[ -~]{0,120}",
    ]
}

/// A calendar of one or two `VEVENT`s built from random content lines, with random line endings.
fn calendar() -> impl Strategy<Value = Vec<u8>> {
    let event = (
        prop::collection::vec(content_line(), 0..12),
        prop::option::of(prop::collection::vec(content_line(), 0..4)),
    );
    (prop::collection::vec(event, 1..4), any::<bool>()).prop_map(|(events, lf)| {
        let eol = if lf { "\n" } else { "\r\n" };
        let mut out = format!("BEGIN:VCALENDAR{eol}VERSION:2.0{eol}");
        for (lines, alarm) in events {
            out.push_str(&format!("BEGIN:VEVENT{eol}"));
            for line in lines {
                out.push_str(&line);
                out.push_str(eol);
            }
            if let Some(alarm) = alarm {
                out.push_str(&format!(
                    "BEGIN:VALARM{eol}ACTION:DISPLAY{eol}TRIGGER:-PT5M{eol}"
                ));
                for line in alarm {
                    out.push_str(&line);
                    out.push_str(eol);
                }
                out.push_str(&format!("END:VALARM{eol}"));
            }
            out.push_str(&format!("END:VEVENT{eol}"));
        }
        out.push_str(&format!("END:VCALENDAR{eol}"));
        out.into_bytes()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_bytes_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        prop_assert_eq!(IcalDocument::parse(&bytes).to_bytes(), bytes.clone());
        prop_assert_eq!(write_back(&bytes), bytes);
    }

    #[test]
    fn any_calendar_round_trips_and_writes_back_unchanged(bytes in calendar()) {
        prop_assert_eq!(IcalDocument::parse(&bytes).to_bytes(), bytes.clone());
        prop_assert_eq!(write_back(&bytes), bytes);
    }
}

fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ;,:\"\\\\^\n\u{e9}\u{1f600}]{0,90}"
}

fn date() -> impl Strategy<Value = Date> {
    (1990_i16..2060, 1_i8..=12, 1_i8..=28).prop_map(|(y, m, d)| Date::new(y, m, d).unwrap())
}

fn date_time() -> impl Strategy<Value = DateTime> {
    (date(), 0_i8..24, 0_i8..60, 0_i8..60).prop_map(|(date, h, m, s)| date.at(h, m, s, 0))
}

fn instant() -> impl Strategy<Value = Timestamp> {
    (631_152_000_i64..2_840_140_800).prop_map(|second| Timestamp::from_second(second).unwrap())
}

fn zone() -> impl Strategy<Value = TimeZoneId> {
    prop_oneof![
        Just("Europe/Berlin"),
        Just("America/New_York"),
        Just("Asia/Kolkata"),
        Just("UTC")
    ]
    .prop_map(TimeZoneId::new)
}

fn times() -> impl Strategy<Value = EventTimes> {
    prop_oneof![
        (date(), 1_i64..10).prop_map(|(start, days)| {
            EventTimes::new(
                TimeValue::all_day(start),
                Some(TimeValue::all_day(
                    start.checked_add(jiff::Span::new().days(days)).unwrap(),
                )),
            )
            .unwrap()
        }),
        (date_time(), prop::option::of(0_i64..100_000)).prop_map(|(start, length)| {
            let end = length.map(|length| {
                TimeValue::floating(
                    start
                        .checked_add(SignedDuration::from_secs(length))
                        .unwrap(),
                )
            });
            EventTimes::new(TimeValue::floating(start), end).unwrap()
        }),
        (instant(), prop::option::of(0_i64..100_000)).prop_map(|(start, length)| {
            let end = length.map(|length| {
                TimeValue::instant(
                    start
                        .checked_add(SignedDuration::from_secs(length))
                        .unwrap(),
                )
            });
            EventTimes::new(TimeValue::instant(start), end).unwrap()
        }),
        (date_time(), zone(), 0_i64..100_000).prop_map(|(start, zone, length)| {
            let end = start
                .checked_add(SignedDuration::from_secs(length))
                .unwrap();
            EventTimes::new(
                TimeValue::zoned(start, zone.clone()),
                Some(TimeValue::zoned(end, zone)),
            )
            .unwrap()
        }),
    ]
}

fn weekday() -> impl Strategy<Value = Weekday> {
    prop_oneof![
        Just(Weekday::Monday),
        Just(Weekday::Tuesday),
        Just(Weekday::Wednesday),
        Just(Weekday::Thursday),
        Just(Weekday::Friday),
        Just(Weekday::Saturday),
        Just(Weekday::Sunday),
    ]
}

fn rule() -> impl Strategy<Value = RecurrenceRule> {
    (
        prop_oneof![
            Just(Frequency::Yearly),
            Just(Frequency::Monthly),
            Just(Frequency::Weekly),
            Just(Frequency::Daily),
        ],
        1_u32..5,
        prop::collection::vec(
            (
                weekday(),
                prop::option::of(prop_oneof![-5_i32..=-1, 1_i32..=5]),
            ),
            0..3,
        ),
        prop::collection::vec(prop_oneof![-31_i8..=-1, 1_i8..=31], 0..3),
        weekday(),
        prop_oneof![
            Just(RuleEnd::Never),
            (1_u32..50).prop_map(|count| RuleEnd::Count { count }),
            instant().prop_map(|until| RuleEnd::Until {
                until: TimeValue::instant(until)
            }),
            date().prop_map(|until| RuleEnd::Until {
                until: TimeValue::all_day(until)
            }),
        ],
    )
        .prop_map(
            |(frequency, interval, by_day, by_month_day, first_day_of_week, end)| {
                let mut rule = RecurrenceRule::new(frequency);
                rule.interval = NonZeroU32::new(interval).unwrap();
                rule.by_day = by_day
                    .into_iter()
                    .map(|(day, nth)| NDay {
                        day,
                        nth_of_period: nth,
                    })
                    .collect();
                rule.by_month_day = by_month_day;
                rule.first_day_of_week = first_day_of_week;
                rule.end = end;
                rule
            },
        )
}

fn attendee(address: String) -> impl Strategy<Value = Attendee> {
    (
        prop::option::of(text()),
        prop_oneof![
            Just(AttendeeRole::Required),
            Just(AttendeeRole::Optional),
            Just(AttendeeRole::Chair),
            Just(AttendeeRole::NonParticipant),
        ],
        prop_oneof![
            Just(Participation::NeedsAction),
            Just(Participation::Accepted),
            Just(Participation::Declined),
            Just(Participation::Tentative),
            Just(Participation::Delegated),
        ],
        prop_oneof![
            Just(ParticipantKind::Individual),
            Just(ParticipantKind::Group),
            Just(ParticipantKind::Resource),
            Just(ParticipantKind::Room),
            Just(ParticipantKind::Unknown),
        ],
    )
        .prop_map(move |(name, role, participation, kind)| Attendee {
            address: address.clone(),
            name,
            role,
            participation,
            kind,
        })
}

fn attendees() -> impl Strategy<Value = Vec<Attendee>> {
    prop::collection::btree_set("[a-z]{1,6}@example\\.com", 0..4)
        .prop_flat_map(|addresses| addresses.into_iter().map(attendee).collect::<Vec<_>>())
}

fn alert() -> impl Strategy<Value = Alert> {
    (
        prop_oneof![
            (-100_000_i64..100_000, any::<bool>()).prop_map(|(seconds, end)| {
                AlertTrigger::Offset {
                    offset: SignedDuration::from_secs(seconds),
                    relative_to: if end {
                        RelativeTo::End
                    } else {
                        RelativeTo::Start
                    },
                }
            }),
            instant().prop_map(|at| AlertTrigger::Absolute { at }),
        ],
        prop_oneof![Just(AlertAction::Display), Just(AlertAction::Email)],
    )
        .prop_map(|(trigger, action)| Alert { trigger, action })
}

prop_compose! {
    fn event()(
        times in times(),
        title in prop::option::of(text()),
        location in prop::option::of(text()),
        description in prop::option::of(text()),
        rules in prop::collection::vec(rule(), 0..2),
        excluded in prop::collection::vec(instant(), 0..3),
        status in prop_oneof![
            Just(EventStatus::Confirmed),
            Just(EventStatus::Tentative),
            Just(EventStatus::Cancelled),
        ],
        transparency in prop_oneof![Just(Transparency::Opaque), Just(Transparency::Transparent)],
        visibility in prop_oneof![
            Just(Visibility::Default),
            Just(Visibility::Public),
            Just(Visibility::Private),
            Just(Visibility::Confidential),
        ],
        organizer in prop::option::of(("[a-z]{1,6}@example\\.com", prop::option::of(text()))),
        attendees in attendees(),
        alerts in prop::collection::vec(alert(), 0..3),
    ) -> NormalizedEvent {
        let mut event = NormalizedEvent::new(times);
        event.title = title;
        event.location = location;
        event.description = description;
        let recurrence = Recurrence {
            rules,
            excluded_dates: excluded.into_iter().map(TimeValue::instant).collect(),
            ..Recurrence::default()
        };
        event.recurrence = (recurrence != Recurrence::default()).then_some(recurrence);
        event.status = status;
        event.transparency = transparency;
        event.visibility = visibility;
        event.organizer = organizer.map(|(address, name)| Organizer { address, name });
        event.attendees = attendees;
        event.reminders = Reminders { use_default: false, alerts };
        event
    }
}

fn series(fields: NormalizedEvent) -> EventSeries {
    let mut ids = ProviderIds::new(Provider::Google, "google-id");
    ids.ical_uid = Some("generated@example.com".to_owned());
    EventSeries::single(ProviderEvent {
        ids,
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields,
        passthrough: Passthrough::new(),
    })
}

fn read_back(document: &IcalDocument) -> NormalizedEvent {
    let read = IcalDocument::parse(&document.to_bytes()).read(&Source::feed());
    assert_eq!(read.series.len(), 1);
    assert!(
        read.series[0].losses.is_empty(),
        "{:#?}",
        read.series[0].losses
    );
    read.series[0].series.master.clone().unwrap().fields
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn an_event_written_reads_back_as_itself(event in event()) {
        let mut document = IcalDocument::new("-//calendar-sync//test//EN");
        let losses = document.write(&series(event.clone()), stamp()).unwrap();
        // The one approximation: an EMAIL alarm has no attendee to send to.
        prop_assert!(losses.iter().all(|loss| loss.field == Field::Reminders), "{:#?}", losses);
        prop_assert_eq!(read_back(&document), event.clone());

        // Writing it again changes nothing.
        let before = document.to_bytes();
        document.write(&series(event), stamp()).unwrap();
        prop_assert_eq!(document.to_bytes(), before);
    }

    #[test]
    fn an_event_written_over_another_reads_back_as_itself(first in event(), second in event()) {
        let mut document = IcalDocument::new("-//calendar-sync//test//EN");
        document.write(&series(first), stamp()).unwrap();
        document.write(&series(second.clone()), stamp()).unwrap();
        prop_assert_eq!(read_back(&document), second);
    }
}
