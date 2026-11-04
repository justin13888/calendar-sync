//! The three-way merge: every combination of base, ours and theirs for every unit, ownership, and
//! passthrough.

use calendar_sync_engine::merge::merge;
use calendar_sync_engine::{FieldValue, MergeOutcome, Refused, Role};
use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, EventStatus, EventTimes, Field, Frequency,
    MapperVersion, NormalizedEvent, Organizer, Participation, Provider, ProviderEvent, ProviderIds,
    ProviderRevision, Recurrence, RecurrenceRule, RelativeTo, Reminders, TimeValue, Transparency,
    Visibility,
};
use jiff::{SignedDuration, Timestamp};

const ME: &str = "me@example.com";
const OTHER: &str = "other@example.com";

fn times(hour: i64) -> EventTimes {
    let start = Timestamp::from_second(1_700_000_000 + hour * 3600).unwrap();
    let end = Timestamp::from_second(1_700_000_000 + (hour + 1) * 3600).unwrap();
    EventTimes::new(TimeValue::instant(start), Some(TimeValue::instant(end))).unwrap()
}

fn event() -> NormalizedEvent {
    let mut event = NormalizedEvent::new(times(0));
    event.attendees = vec![Attendee::new(ME), Attendee::new(OTHER)];
    event
}

fn provider(fields: NormalizedEvent) -> ProviderEvent {
    let mut record = ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "evt1"),
        revision: Some(ProviderRevision::new("\"42\"")),
        mapper_version: MapperVersion::new(3),
        fields,
        passthrough: Default::default(),
    };
    record.passthrough.push("colorId", "\"7\"");
    record.passthrough.push("X-UNKNOWN", "X-UNKNOWN:kept");
    record
}

fn owner() -> Role {
    Role::Owner
}

fn attendee() -> Role {
    Role::Attendee {
        address: ME.to_uppercase(),
    }
}

/// One merge unit: how to set it to the `n`th of three distinct values, and how to read it back.
struct Unit {
    name: &'static str,
    field: Field,
    attendee: Option<&'static str>,
    set: fn(&mut NormalizedEvent, usize),
    attendee_owns: bool,
}

fn text(n: usize) -> Option<String> {
    [None, Some("a".to_owned()), Some("b".to_owned())][n].clone()
}

fn participation(n: usize) -> Participation {
    [
        Participation::NeedsAction,
        Participation::Accepted,
        Participation::Declined,
    ][n]
}

fn units() -> Vec<Unit> {
    vec![
        Unit {
            name: "title",
            field: Field::Title,
            attendee: None,
            set: |e, n| e.title = text(n),
            attendee_owns: false,
        },
        Unit {
            name: "times",
            field: Field::Times,
            attendee: None,
            set: |e, n| e.times = times(i64::try_from(n).unwrap()),
            attendee_owns: false,
        },
        Unit {
            name: "recurrence",
            field: Field::Recurrence,
            attendee: None,
            set: |e, n| {
                e.recurrence = [
                    None,
                    Some(Recurrence::rule(RecurrenceRule::new(Frequency::Daily))),
                    Some(Recurrence::rule(RecurrenceRule::new(Frequency::Weekly))),
                ][n]
                    .clone();
            },
            attendee_owns: false,
        },
        Unit {
            name: "organizer",
            field: Field::Organizer,
            attendee: None,
            set: |e, n| {
                e.organizer = text(n).map(|address| Organizer {
                    address,
                    name: None,
                });
            },
            attendee_owns: false,
        },
        Unit {
            name: "location",
            field: Field::Location,
            attendee: None,
            set: |e, n| e.location = text(n),
            attendee_owns: false,
        },
        Unit {
            name: "description",
            field: Field::Description,
            attendee: None,
            set: |e, n| e.description = text(n),
            attendee_owns: false,
        },
        Unit {
            name: "status",
            field: Field::Status,
            attendee: None,
            set: |e, n| {
                e.status = [
                    EventStatus::Confirmed,
                    EventStatus::Tentative,
                    EventStatus::Cancelled,
                ][n];
            },
            attendee_owns: false,
        },
        Unit {
            name: "transparency",
            field: Field::Transparency,
            attendee: None,
            set: |e, n| {
                // Two values only: the third repeats the first, which the oracle accounts for.
                e.transparency = [
                    Transparency::Opaque,
                    Transparency::Transparent,
                    Transparency::Opaque,
                ][n];
            },
            attendee_owns: false,
        },
        Unit {
            name: "visibility",
            field: Field::Visibility,
            attendee: None,
            set: |e, n| {
                e.visibility = [Visibility::Default, Visibility::Public, Visibility::Private][n];
            },
            attendee_owns: false,
        },
        Unit {
            name: "reminders",
            field: Field::Reminders,
            attendee: None,
            set: |e, n| {
                e.reminders = Reminders {
                    use_default: n == 0,
                    alerts: (0..n)
                        .map(|minutes| Alert {
                            trigger: AlertTrigger::Offset {
                                offset: SignedDuration::from_mins(
                                    -10 * i64::try_from(minutes).unwrap(),
                                ),
                                relative_to: RelativeTo::Start,
                            },
                            action: AlertAction::Display,
                        })
                        .collect(),
                };
            },
            attendee_owns: true,
        },
        Unit {
            name: "my reply",
            field: Field::Attendees,
            attendee: Some(ME),
            set: |e, n| e.attendees[0].participation = participation(n),
            attendee_owns: true,
        },
        Unit {
            name: "my entry's name",
            field: Field::Attendees,
            attendee: Some(ME),
            set: |e, n| e.attendees[0].name = text(n),
            attendee_owns: false,
        },
        Unit {
            name: "another attendee",
            field: Field::Attendees,
            attendee: Some(OTHER),
            set: |e, n| e.attendees[1].participation = participation(n),
            attendee_owns: false,
        },
    ]
}

fn with(unit: &Unit, n: usize) -> NormalizedEvent {
    let mut event = event();
    (unit.set)(&mut event, n);
    event
}

/// What the merge must do with one unit, by the issue's rule.
#[derive(Debug, PartialEq, Eq)]
enum Expected {
    Theirs,
    Ours,
    Conflict,
    Refused,
}

fn expected(
    base: &NormalizedEvent,
    ours: &NormalizedEvent,
    theirs: &NormalizedEvent,
    owns: bool,
) -> Expected {
    if ours == base || ours == theirs {
        Expected::Theirs
    } else if !owns {
        Expected::Refused
    } else if theirs == base {
        Expected::Ours
    } else {
        Expected::Conflict
    }
}

fn check(unit: &Unit, role: &Role, owns: bool, b: usize, o: usize, t: usize) {
    let base = with(unit, b);
    let ours = with(unit, o);
    let theirs = provider(with(unit, t));
    let outcome: MergeOutcome = merge(&base, &ours, &theirs, role);
    let case = format!("{} as {role:?}: base {b}, ours {o}, theirs {t}", unit.name);

    // Unknown fields and the provider's identity survive every merge.
    assert_eq!(outcome.event.passthrough, theirs.passthrough, "{case}");
    assert_eq!(outcome.event.ids, theirs.ids, "{case}");
    assert_eq!(outcome.event.revision, theirs.revision, "{case}");
    assert_eq!(
        outcome.event.mapper_version, theirs.mapper_version,
        "{case}"
    );

    let want = expected(&base, &ours, &theirs.fields, owns);
    let fields = match want {
        Expected::Ours => &ours,
        _ => &theirs.fields,
    };
    assert_eq!(&outcome.event.fields, fields, "{case}");
    assert_eq!(outcome.changed, want == Expected::Ours, "{case}");

    if want == Expected::Conflict {
        assert_eq!(outcome.conflicts.len(), 1, "{case}");
        let conflict = &outcome.conflicts[0];
        assert_eq!(conflict.field, unit.field, "{case}");
        assert_eq!(conflict.attendee.as_deref(), unit.attendee, "{case}");
        assert_ne!(conflict.base, conflict.ours, "{case}");
        assert_ne!(conflict.ours, conflict.theirs, "{case}");
        assert_ne!(conflict.base, conflict.theirs, "{case}");
    } else {
        assert!(
            outcome.conflicts.is_empty(),
            "{case}: {:?}",
            outcome.conflicts
        );
    }

    if want == Expected::Refused {
        assert_eq!(
            outcome.refused,
            vec![Refused {
                field: unit.field.clone(),
                attendee: unit.attendee.map(str::to_owned),
            }],
            "{case}"
        );
    } else {
        assert!(outcome.refused.is_empty(), "{case}: {:?}", outcome.refused);
    }
}

#[test]
fn every_three_way_case_of_every_unit() {
    for unit in units() {
        for b in 0..3 {
            for o in 0..3 {
                for t in 0..3 {
                    check(&unit, &owner(), true, b, o, t);
                    check(&unit, &attendee(), unit.attendee_owns, b, o, t);
                }
            }
        }
    }
}

#[test]
fn independent_changes_to_different_fields_both_land() {
    let base = event();
    let mut ours = event();
    ours.title = Some("ours".to_owned());
    let mut theirs = event();
    theirs.location = Some("theirs".to_owned());

    let outcome = merge(&base, &ours, &provider(theirs), &owner());

    assert_eq!(outcome.event.fields.title.as_deref(), Some("ours"));
    assert_eq!(outcome.event.fields.location.as_deref(), Some("theirs"));
    assert!(outcome.changed);
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn a_conflict_reports_all_three_values_and_keeps_theirs() {
    let mut base = event();
    base.title = Some("base".to_owned());
    let mut ours = base.clone();
    ours.title = Some("ours".to_owned());
    let mut theirs = base.clone();
    theirs.title = Some("theirs".to_owned());

    let outcome = merge(&base, &ours, &provider(theirs), &owner());

    assert_eq!(outcome.event.fields.title.as_deref(), Some("theirs"));
    assert!(!outcome.changed);
    assert_eq!(outcome.conflicts.len(), 1);
    let conflict = &outcome.conflicts[0];
    assert_eq!(conflict.field, Field::Title);
    assert_eq!(conflict.base, FieldValue::Title(Some("base".to_owned())));
    assert_eq!(conflict.ours, FieldValue::Title(Some("ours".to_owned())));
    assert_eq!(
        conflict.theirs,
        FieldValue::Title(Some("theirs".to_owned()))
    );
}

#[test]
fn attendees_changed_on_both_sides_merge_entry_by_entry() {
    let base = event();
    let mut ours = event();
    ours.attendees[0].participation = Participation::Accepted;
    ours.attendees
        .push(Attendee::new("added-by-us@example.com"));
    let mut theirs = event();
    theirs.attendees.remove(1);
    theirs
        .attendees
        .push(Attendee::new("added-by-them@example.com"));

    let outcome = merge(&base, &ours, &provider(theirs), &owner());

    let addresses: Vec<_> = outcome
        .event
        .fields
        .attendees
        .iter()
        .map(|a| (a.address.as_str(), a.participation))
        .collect();
    assert_eq!(
        addresses,
        [
            (ME, Participation::Accepted),
            ("added-by-them@example.com", Participation::NeedsAction),
            ("added-by-us@example.com", Participation::NeedsAction),
        ]
    );
    assert!(outcome.conflicts.is_empty());
    assert!(outcome.refused.is_empty());
}

#[test]
fn attendee_addresses_match_case_insensitively() {
    let base = event();
    let mut ours = event();
    ours.attendees[1].participation = Participation::Tentative;
    let mut theirs = event();
    theirs.attendees[1].address = OTHER.to_uppercase();

    let outcome = merge(&base, &ours, &provider(theirs), &owner());

    // Theirs changed the spelling and ours the reply: different parts of one entry, so the entry
    // as a whole is in conflict, and theirs is kept.
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(
        outcome.conflicts[0].attendee.as_deref(),
        Some(OTHER.to_uppercase().as_str())
    );
    assert_eq!(outcome.event.fields.attendees.len(), 2);
}

#[test]
fn an_attendee_may_reply_while_the_organizer_edits_its_entry() {
    let base = event();
    let mut ours = event();
    ours.attendees[0].participation = Participation::Accepted;
    let mut theirs = event();
    theirs.attendees[0].name = Some("Me".to_owned());

    let outcome = merge(&base, &ours, &provider(theirs), &attendee());

    let mine = &outcome.event.fields.attendees[0];
    assert_eq!(mine.participation, Participation::Accepted);
    assert_eq!(mine.name.as_deref(), Some("Me"));
    assert!(outcome.changed);
    assert!(outcome.conflicts.is_empty());
    assert!(outcome.refused.is_empty());
}

#[test]
fn an_attendee_cannot_add_or_remove_attendees() {
    let base = event();
    let mut ours = event();
    ours.attendees.remove(0);
    ours.attendees.push(Attendee::new("guest@example.com"));

    let outcome = merge(&base, &ours, &provider(event()), &attendee());

    assert_eq!(outcome.event.fields, event());
    assert!(!outcome.changed);
    assert_eq!(
        outcome.refused,
        [
            Refused {
                field: Field::Attendees,
                attendee: Some(ME.to_owned()),
            },
            Refused {
                field: Field::Attendees,
                attendee: Some("guest@example.com".to_owned()),
            },
        ]
    );
}

#[test]
fn repeated_addresses_are_told_apart_by_occurrence() {
    let mut base = event();
    base.attendees.push(Attendee::new(OTHER));
    let mut ours = base.clone();
    ours.attendees[2].participation = Participation::Declined;
    let theirs = base.clone();

    let outcome = merge(&base, &ours, &provider(theirs), &owner());

    assert_eq!(outcome.event.fields, ours);
}

#[test]
fn an_edit_already_on_the_provider_writes_nothing() {
    let base = event();
    let mut ours = event();
    ours.title = Some("same".to_owned());
    ours.attendees[0].participation = Participation::Accepted;

    let outcome = merge(&base, &ours, &provider(ours.clone()), &attendee());

    assert!(!outcome.changed);
    assert!(outcome.refused.is_empty());
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn every_device_reaches_the_same_result() {
    let base = event();
    let mut ours = event();
    ours.title = Some("ours".to_owned());
    ours.location = Some("ours".to_owned());
    let mut theirs = event();
    theirs.location = Some("theirs".to_owned());
    let theirs = provider(theirs);

    let first = merge(&base, &ours, &theirs, &owner());
    let second = merge(&base.clone(), &ours.clone(), &theirs.clone(), &owner());

    assert_eq!(first, second);
}
