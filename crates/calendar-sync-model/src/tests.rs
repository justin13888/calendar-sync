//! Serialization round trips over generated values, and the checks serialization enforces.

use jiff::civil::{Date, DateTime, Time};
use jiff::{SignedDuration, Timestamp};
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::*;

fn timestamp() -> impl Strategy<Value = Timestamp> {
    // Years 1 to 9999, with nanoseconds.
    (-62_135_596_800_i64..253_402_207_200, 0..1_000_000_000_i32)
        .prop_map(|(seconds, nanos)| Timestamp::new(seconds, nanos).unwrap())
}

fn date() -> impl Strategy<Value = Date> {
    (1_i16..=9999, 1_i8..=12, 1_i8..=31)
        .prop_filter_map("a real date", |(y, m, d)| Date::new(y, m, d).ok())
}

fn date_time() -> impl Strategy<Value = DateTime> {
    (date(), 0_i8..24, 0_i8..60, 0_i8..60, 0..1_000_000_000_i32)
        .prop_map(|(d, h, m, s, ns)| d.to_datetime(Time::new(h, m, s, ns).unwrap()))
}

fn text() -> impl Strategy<Value = String> {
    "\\PC{0,16}"
}

fn time_zone() -> impl Strategy<Value = TimeZoneId> {
    prop_oneof![
        Just(TimeZoneId::new("Europe/Berlin")),
        Just(TimeZoneId::new("America/New_York")),
        text().prop_map(TimeZoneId::new),
    ]
}

fn time_value() -> impl Strategy<Value = TimeValue> {
    prop_oneof![
        timestamp().prop_map(TimeValue::instant),
        (date_time(), time_zone()).prop_map(|(dt, tz)| TimeValue::zoned(dt, tz)),
        date_time().prop_map(TimeValue::floating),
        date().prop_map(TimeValue::all_day),
    ]
}

fn event_times() -> impl Strategy<Value = EventTimes> {
    (time_value(), option::of(time_value()))
        .prop_filter_map("a valid start and end", |(start, end)| {
            EventTimes::new(start, end).ok()
        })
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
    let frequency = prop_oneof![
        Just(Frequency::Yearly),
        Just(Frequency::Monthly),
        Just(Frequency::Weekly),
        Just(Frequency::Daily),
        Just(Frequency::Hourly),
        Just(Frequency::Minutely),
        Just(Frequency::Secondly),
    ];
    let skip = prop_oneof![Just(Skip::Omit), Just(Skip::Backward), Just(Skip::Forward)];
    let n_day = (weekday(), option::of(any::<i32>()))
        .prop_map(|(day, nth_of_period)| NDay { day, nth_of_period });
    let month = (1_u8..=13, any::<bool>()).prop_map(|(month, leap)| RuleMonth { month, leap });
    let end = prop_oneof![
        Just(RuleEnd::Never),
        any::<u32>().prop_map(|count| RuleEnd::Count { count }),
        time_value().prop_map(|until| RuleEnd::Until { until }),
    ];
    let head = (
        frequency,
        1_u32..1000,
        option::of(prop_oneof![
            Just("gregorian".to_owned()),
            Just("hebrew".to_owned())
        ]),
        skip,
        weekday(),
        vec(n_day, 0..3),
        vec(any::<i8>(), 0..3),
        vec(month, 0..3),
    );
    let tail = (
        vec(any::<i16>(), 0..3),
        vec(any::<i8>(), 0..3),
        vec(any::<u8>(), 0..3),
        vec(any::<u8>(), 0..3),
        vec(any::<u8>(), 0..3),
        vec(any::<i32>(), 0..3),
        end,
    );
    (head, tail).prop_map(
        |(
            (frequency, interval, rscale, skip, first_day_of_week, by_day, by_month_day, by_month),
            (by_year_day, by_week_no, by_hour, by_minute, by_second, by_set_position, end),
        )| RecurrenceRule {
            frequency,
            interval: interval.try_into().unwrap(),
            rscale,
            skip,
            first_day_of_week,
            by_day,
            by_month_day,
            by_month,
            by_year_day,
            by_week_no,
            by_hour,
            by_minute,
            by_second,
            by_set_position,
            end,
        },
    )
}

fn recurrence() -> impl Strategy<Value = Recurrence> {
    (
        vec(rule(), 0..3),
        vec(rule(), 0..2),
        vec(time_value(), 0..3),
        vec(time_value(), 0..3),
    )
        .prop_map(
            |(rules, exclusion_rules, dates, excluded_dates)| Recurrence {
                rules,
                exclusion_rules,
                dates,
                excluded_dates,
            },
        )
}

fn attendee() -> impl Strategy<Value = Attendee> {
    let role = prop_oneof![
        Just(AttendeeRole::Required),
        Just(AttendeeRole::Optional),
        Just(AttendeeRole::Chair),
        Just(AttendeeRole::NonParticipant),
    ];
    let participation = prop_oneof![
        Just(Participation::NeedsAction),
        Just(Participation::Accepted),
        Just(Participation::Declined),
        Just(Participation::Tentative),
        Just(Participation::Delegated),
    ];
    let kind = prop_oneof![
        Just(ParticipantKind::Individual),
        Just(ParticipantKind::Group),
        Just(ParticipantKind::Resource),
        Just(ParticipantKind::Room),
        Just(ParticipantKind::Unknown),
    ];
    (text(), option::of(text()), role, participation, kind).prop_map(
        |(address, name, role, participation, kind)| Attendee {
            address,
            name,
            role,
            participation,
            kind,
        },
    )
}

fn alert() -> impl Strategy<Value = Alert> {
    let trigger = prop_oneof![
        (
            any::<i64>(),
            prop_oneof![Just(RelativeTo::Start), Just(RelativeTo::End)]
        )
            .prop_map(|(seconds, relative_to)| AlertTrigger::Offset {
                offset: SignedDuration::from_secs(seconds % 10_000_000),
                relative_to,
            }),
        timestamp().prop_map(|at| AlertTrigger::Absolute { at }),
    ];
    let action = prop_oneof![Just(AlertAction::Display), Just(AlertAction::Email)];
    (trigger, action).prop_map(|(trigger, action)| Alert { trigger, action })
}

fn normalized_event() -> impl Strategy<Value = NormalizedEvent> {
    let status = prop_oneof![
        Just(EventStatus::Confirmed),
        Just(EventStatus::Tentative),
        Just(EventStatus::Cancelled),
    ];
    let transparency = prop_oneof![Just(Transparency::Opaque), Just(Transparency::Transparent)];
    let visibility = prop_oneof![
        Just(Visibility::Default),
        Just(Visibility::Public),
        Just(Visibility::Private),
        Just(Visibility::Confidential),
    ];
    let organizer =
        (text(), option::of(text())).prop_map(|(address, name)| Organizer { address, name });
    let reminders =
        (any::<bool>(), vec(alert(), 0..3)).prop_map(|(use_default, alerts)| Reminders {
            use_default,
            alerts,
        });
    (
        (option::of(text()), event_times(), option::of(recurrence())),
        (option::of(organizer), vec(attendee(), 0..3)),
        (option::of(text()), option::of(text())),
        (status, transparency, visibility, reminders),
    )
        .prop_map(
            |(
                (title, times, recurrence),
                (organizer, attendees),
                (location, description),
                (status, transparency, visibility, reminders),
            )| NormalizedEvent {
                title,
                times,
                recurrence,
                organizer,
                attendees,
                location,
                description,
                status,
                transparency,
                visibility,
                reminders,
            },
        )
}

fn provider_ids() -> impl Strategy<Value = ProviderIds> {
    let provider = prop_oneof![
        Just(Provider::Google),
        Just(Provider::Graph),
        Just(Provider::CalDav),
        Just(Provider::Ics),
    ];
    (provider, text(), option::of(text()), option::of(text())).prop_map(
        |(provider, id, ical_uid, series_id)| ProviderIds {
            provider,
            id,
            ical_uid,
            series_id,
        },
    )
}

fn provider_event() -> impl Strategy<Value = ProviderEvent> {
    (
        provider_ids(),
        option::of(text().prop_map(ProviderRevision::new)),
        any::<u32>().prop_map(MapperVersion::new),
        normalized_event(),
        vec((text(), text()), 0..3),
    )
        .prop_map(
            |(ids, revision, mapper_version, fields, passthrough)| ProviderEvent {
                ids,
                revision,
                mapper_version,
                fields,
                passthrough: passthrough
                    .into_iter()
                    .map(|(name, raw)| PassthroughField { name, raw })
                    .collect(),
            },
        )
}

fn event_override() -> impl Strategy<Value = Override> {
    let range = prop_oneof![
        Just(InstanceRange::ThisInstance),
        Just(InstanceRange::ThisAndFuture)
    ];
    prop_oneof![
        (provider_event(), range).prop_map(|(event, range)| Override::Modified {
            event: Box::new(event),
            range
        }),
        (
            option::of(provider_ids()),
            option::of(text().prop_map(ProviderRevision::new))
        )
            .prop_map(|(ids, revision)| Override::Cancelled { ids, revision }),
    ]
}

fn event_series() -> impl Strategy<Value = EventSeries> {
    (
        option::of(provider_event()),
        proptest::collection::btree_map(
            time_value().prop_map(InstanceKey::new),
            event_override(),
            0..3,
        ),
    )
        .prop_map(|(master, overrides)| EventSeries { master, overrides })
}

fn loss() -> impl Strategy<Value = Loss> {
    let field = prop_oneof![
        Just(Field::Title),
        Just(Field::Times),
        Just(Field::Recurrence),
        Just(Field::Attendees),
        Just(Field::Overrides),
        text().prop_map(Field::Other),
    ];
    let reason = prop_oneof![
        Just(LossReason::Unrepresentable),
        Just(LossReason::Approximated),
        Just(LossReason::Unsupported),
        Just(LossReason::Malformed),
    ];
    (field, reason, text()).prop_map(|(field, reason, detail)| Loss {
        field,
        reason,
        detail,
    })
}

/// Serializes `value` to JSON and back, and returns what came back.
fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> T {
    let json = serde_json::to_string(value).unwrap();
    serde_json::from_str(&json).unwrap_or_else(|error| panic!("{error}: {json}"))
}

proptest! {
    #[test]
    fn time_values_round_trip(value in time_value()) {
        prop_assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn recurrences_round_trip(value in recurrence()) {
        prop_assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn normalized_events_round_trip(value in normalized_event()) {
        prop_assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn event_series_round_trip(value in event_series()) {
        prop_assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn losses_round_trip(value in loss()) {
        prop_assert_eq!(round_trip(&value), value);
    }
}

fn civil(text: &str) -> DateTime {
    text.parse().unwrap()
}

#[test]
fn event_times_pair_forms() {
    let day = TimeValue::all_day(jiff::civil::date(2026, 3, 1));
    let floating = TimeValue::floating(civil("2026-03-01T09:00"));
    let instant = TimeValue::instant("2026-03-01T09:00Z".parse().unwrap());
    let zoned = TimeValue::zoned(civil("2026-03-01T10:00"), TimeZoneId::new("Europe/Berlin"));

    assert!(EventTimes::new(instant.clone(), Some(zoned.clone())).is_ok());
    assert!(EventTimes::new(zoned, Some(instant.clone())).is_ok());
    assert_eq!(
        EventTimes::new(day.clone(), Some(floating.clone())),
        Err(EventTimesError::Unpaired {
            start: TimeKind::AllDay,
            end: TimeKind::Floating,
        })
    );
    assert!(EventTimes::new(floating, Some(instant.clone())).is_err());
    assert!(EventTimes::new(instant, Some(day)).is_err());
}

#[test]
fn event_times_refuse_an_end_before_the_start() {
    let berlin = TimeZoneId::new("Europe/Berlin");
    let cases = [
        (
            TimeValue::all_day(jiff::civil::date(2026, 3, 2)),
            TimeValue::all_day(jiff::civil::date(2026, 3, 1)),
        ),
        (
            TimeValue::floating(civil("2026-03-01T10:00")),
            TimeValue::floating(civil("2026-03-01T09:00")),
        ),
        (
            TimeValue::instant("2026-03-01T10:00Z".parse().unwrap()),
            TimeValue::instant("2026-03-01T09:00Z".parse().unwrap()),
        ),
        (
            TimeValue::zoned(civil("2026-03-01T10:00"), berlin.clone()),
            TimeValue::zoned(civil("2026-03-01T09:00"), berlin),
        ),
    ];
    for (start, end) in cases {
        assert_eq!(
            EventTimes::new(start.clone(), Some(end.clone())),
            Err(EventTimesError::EndBeforeStart)
        );
        assert!(EventTimes::new(end.clone(), Some(start)).is_ok());
        assert!(EventTimes::new(end.clone(), Some(end)).is_ok());
    }
    // Two zones cannot be compared without a time zone database, so the check does not guess.
    let tokyo = TimeValue::zoned(civil("2026-03-01T09:00"), TimeZoneId::new("Asia/Tokyo"));
    let berlin = TimeValue::zoned(civil("2026-03-01T10:00"), TimeZoneId::new("Europe/Berlin"));
    assert!(EventTimes::new(berlin, Some(tokyo)).is_ok());
}

#[test]
fn deserializing_checks_event_times() {
    let json = r#"{"start":{"type":"allDay","date":"2026-03-02"},"end":{"type":"allDay","date":"2026-03-01"}}"#;
    assert!(serde_json::from_str::<EventTimes>(json).is_err());
    let json = r#"{"start":{"type":"allDay","date":"2026-03-01"},"end":{"type":"floating","dateTime":"2026-03-01T09:00:00"}}"#;
    assert!(serde_json::from_str::<EventTimes>(json).is_err());
}

#[test]
fn serialized_shape_is_camel_case_and_jscalendar_like() {
    let rule = RecurrenceRule {
        by_day: vec![NDay::nth(Weekday::Thursday, -1)],
        end: RuleEnd::Count { count: 5 },
        ..RecurrenceRule::new(Frequency::Monthly)
    };
    let value = serde_json::to_value(&rule).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "frequency": "monthly",
            "interval": 1,
            "skip": "omit",
            "firstDayOfWeek": "mo",
            "byDay": [{"day": "th", "nthOfPeriod": -1}],
            "end": {"type": "count", "count": 5},
        })
    );
    let zoned = TimeValue::zoned(civil("2026-03-01T09:00"), TimeZoneId::new("Europe/Berlin"));
    assert_eq!(
        serde_json::to_value(&zoned).unwrap(),
        serde_json::json!({
            "type": "zoned",
            "dateTime": "2026-03-01T09:00:00",
            "timeZone": "Europe/Berlin",
        })
    );
}

#[test]
fn a_minimal_rule_takes_jscalendar_defaults() {
    let rule: RecurrenceRule = serde_json::from_str(r#"{"frequency":"weekly"}"#).unwrap();
    assert_eq!(rule, RecurrenceRule::new(Frequency::Weekly));
    assert!(
        serde_json::from_str::<RecurrenceRule>(r#"{"frequency":"weekly","interval":0}"#).is_err()
    );
}

#[test]
fn duplicate_overrides_are_refused() {
    let key = r#"{"type":"instant","timestamp":"2026-03-01T09:00:00Z"}"#;
    let cancelled = r#"{"type":"cancelled"}"#;
    let json = format!(
        r#"{{"overrides":[{{"instance":{key},"override":{cancelled}}},{{"instance":{key},"override":{cancelled}}}]}}"#
    );
    let error = serde_json::from_str::<EventSeries>(&json).unwrap_err();
    assert!(error.to_string().contains("two overrides"), "{error}");
}

#[test]
fn a_single_event_is_a_master_without_recurrence() {
    let start = TimeValue::instant("2026-03-01T09:00Z".parse().unwrap());
    let event = ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "abc"),
        revision: Some(ProviderRevision::new("\"1\"")),
        mapper_version: MapperVersion::new(1),
        fields: NormalizedEvent::new(EventTimes::at(start)),
        passthrough: Passthrough::new(),
    };
    let mut series = EventSeries::single(event);
    assert!(!series.is_empty());
    assert!(!series.is_recurring());
    if let Some(master) = &mut series.master {
        master.fields.recurrence = Some(Recurrence::rule(RecurrenceRule::new(Frequency::Daily)));
    }
    assert!(series.is_recurring());
}

#[test]
fn passthrough_keeps_order_and_repeats() {
    let mut passthrough = Passthrough::new();
    passthrough.push("X-A", "X-A:1");
    passthrough.push("X-B", "X-B:2");
    passthrough.push("X-A", "X-A:3");
    assert_eq!(passthrough.len(), 3);
    assert_eq!(
        passthrough.get("X-A").collect::<Vec<_>>(),
        ["X-A:1", "X-A:3"]
    );
    assert_eq!(
        passthrough
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["X-A", "X-B", "X-A"]
    );
}

#[test]
fn weekdays_convert_to_and_from_jiff() {
    for day in [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ] {
        assert_eq!(Weekday::from(jiff::civil::Weekday::from(day)), day);
    }
}

#[test]
fn losses_display_field_reason_and_detail() {
    let loss = Loss::new(Field::Recurrence, LossReason::Unrepresentable, "BYWEEKNO");
    assert_eq!(loss.to_string(), "recurrence: unrepresentable (BYWEEKNO)");
}
