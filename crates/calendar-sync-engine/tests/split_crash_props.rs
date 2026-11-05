//! Series splits under crashes: a split interrupted after any write, and finished by a device
//! that knows only what the provider holds, ends where an uninterrupted split ends, and no state
//! along the way loses an occurrence or brings back a cancelled one. A split's new series is
//! found from its ids alone.

use std::collections::BTreeMap;

use calendar_sync_engine::split::{Split, SplitError, SplitStep, Splitter};
use calendar_sync_engine::{DerivedIds, EditId, ResourceId};
use calendar_sync_model::{
    EventSeries, EventTimes, Frequency, InstanceKey, MapperVersion, NormalizedEvent, Override,
    Passthrough, Provider, ProviderEvent, ProviderIds, Recurrence, RecurrenceRule, RuleEnd,
    TimeValue, TimeZoneId,
};
use calendar_sync_recurrence::{Expander, Window};
use jiff::civil::DateTime;
use jiff::tz::{TimeZone, TimeZoneDatabase};
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;

/// More writes than any split here needs.
const MAX_STEPS: usize = 200;
const ZONE: &str = "America/New_York";

fn zones() -> TimeZoneDatabase {
    TimeZoneDatabase::bundled()
}

fn floating() -> TimeZone {
    zones().get(ZONE).unwrap()
}

/// The form a series' times take.
#[derive(Clone, Copy, Debug)]
enum Form {
    Instant,
    Zoned,
    Floating,
    AllDay,
}

impl Form {
    /// `civil` in this form. Instants are read in UTC.
    fn value(self, civil: DateTime) -> TimeValue {
        match self {
            Self::Instant => TimeValue::instant(
                TimeZone::UTC
                    .to_ambiguous_timestamp(civil)
                    .unambiguous()
                    .unwrap(),
            ),
            Self::Zoned => TimeValue::zoned(civil, TimeZoneId::new(ZONE)),
            Self::Floating => TimeValue::floating(civil),
            Self::AllDay => TimeValue::all_day(civil.date()),
        }
    }

    fn is_all_day(self) -> bool {
        matches!(self, Self::AllDay)
    }
}

/// A value's civil time in its own terms, to move it.
fn civil(value: &TimeValue) -> DateTime {
    match value {
        TimeValue::Instant { timestamp } => TimeZone::UTC.to_datetime(*timestamp),
        TimeValue::Zoned { date_time, .. } | TimeValue::Floating { date_time } => *date_time,
        TimeValue::AllDay { date } => date.to_datetime(jiff::civil::Time::midnight()),
    }
}

fn moved(value: &TimeValue, form: Form, by: SignedDuration) -> TimeValue {
    form.value(civil(value).checked_add(by).unwrap())
}

fn moved_times(times: &EventTimes, form: Form, by: SignedDuration) -> EventTimes {
    EventTimes::new(
        moved(times.start(), form, by),
        times.end().map(|end| moved(end, form, by)),
    )
    .unwrap()
}

fn record(id: &str, series_id: Option<&str>, fields: NormalizedEvent) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds {
            provider: Provider::Google,
            id: id.to_owned(),
            ical_uid: None,
            series_id: series_id.map(str::to_owned),
        },
        revision: Some(calendar_sync_model::ProviderRevision::new("\"1\"")),
        mapper_version: MapperVersion::new(1),
        fields,
        passthrough: Passthrough::new(),
    }
}

/// What a generated case asks for.
#[derive(Clone, Debug)]
struct Case {
    form: Form,
    frequency: Frequency,
    interval: u32,
    end: u8,
    /// Which instance the split is at, from the second on.
    at: usize,
    /// Per instance: 0 untouched, 1 modified, 2 modified and moved, 3 cancelled, 4 excluded.
    marks: Vec<u8>,
    /// How far the new series starts from the split instance, in hours (days for all-day).
    shift: i64,
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop_oneof![
            Just(Form::Instant),
            Just(Form::Zoned),
            Just(Form::Floating),
            Just(Form::AllDay)
        ],
        prop_oneof![Just(Frequency::Daily), Just(Frequency::Weekly)],
        1u32..=3,
        0u8..3,
        1usize..10,
        proptest::collection::vec(prop_oneof![4 => Just(0u8), 1 => 1u8..=4], 14),
        prop_oneof![Just(0i64), -30i64..30],
    )
        .prop_map(|(form, frequency, interval, end, at, marks, shift)| Case {
            form,
            frequency,
            interval,
            end,
            at,
            marks,
            shift,
        })
}

/// The series a case describes, the split it asks for, and the new series' fields.
struct Built {
    old: EventSeries,
    split: Split,
    tail: NormalizedEvent,
}

fn window() -> Window {
    Window::new(
        "2026-02-20T00:00:00Z".parse().unwrap(),
        "2027-06-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap()
}

/// Where the checks of the new series stop: the window's end, less more than any shift.
fn checked_until() -> Timestamp {
    "2027-05-25T00:00:00Z".parse().unwrap()
}

fn build(case: &Case) -> Option<Built> {
    let form = case.form;
    // Spans the daylight-saving change in New York on 8 March.
    let start: DateTime = "2026-03-01T09:30:00".parse().unwrap();
    let length = if form.is_all_day() {
        SignedDuration::from_hours(24)
    } else {
        SignedDuration::from_mins(45)
    };
    let times = EventTimes::new(
        form.value(start),
        Some(form.value(start.checked_add(length).unwrap())),
    )
    .unwrap();
    let mut rule = RecurrenceRule::new(case.frequency);
    rule.interval = core::num::NonZeroU32::new(case.interval).unwrap();
    rule.end = match case.end {
        0 => RuleEnd::Never,
        1 => RuleEnd::Count { count: 12 },
        _ => RuleEnd::Until {
            until: if form.is_all_day() {
                TimeValue::all_day("2026-05-01".parse().unwrap())
            } else {
                TimeValue::instant("2026-05-01T23:00:00Z".parse().unwrap())
            },
        },
    };
    let mut fields = NormalizedEvent::new(times);
    fields.title = Some("weekly".to_owned());
    fields.recurrence = Some(Recurrence::rule(rule.clone()));

    // The instances, from a series with no exceptions yet.
    let plain = EventSeries::single(record("old", None, fields.clone()));
    let zones = zones();
    let expander = Expander::new(&zones, floating());
    let instances: Vec<(InstanceKey, EventTimes)> = expander
        .expand(&plain, &window())
        .occurrences
        .into_iter()
        .map(|occurrence| (occurrence.key, occurrence.times))
        .collect();
    let (at, _) = instances.get(case.at)?;

    let mut old = plain.clone();
    let day = SignedDuration::from_hours(24);
    for (index, (key, times)) in instances.iter().enumerate() {
        let mark = case.marks.get(index).copied().unwrap_or(0);
        let mut instance = fields.clone();
        instance.recurrence = None;
        instance.title = Some(format!("instance {index}"));
        match mark {
            1 => instance.times = times.clone(),
            2 => {
                let by = if form.is_all_day() {
                    day
                } else {
                    SignedDuration::from_mins(30)
                };
                instance.times = moved_times(times, form, by);
            }
            3 => {
                old.overrides.insert(
                    key.clone(),
                    Override::Cancelled {
                        ids: None,
                        revision: None,
                    },
                );
                continue;
            }
            4 => {
                let master = old.master.as_mut().unwrap();
                let recurrence = master.fields.recurrence.as_mut().unwrap();
                recurrence.excluded_dates.push(key.original_start().clone());
                continue;
            }
            _ => continue,
        }
        old.overrides.insert(
            key.clone(),
            Override::Modified {
                event: Box::new(record(&format!("old_{index}"), Some("old"), instance)),
                range: calendar_sync_model::InstanceRange::ThisInstance,
            },
        );
    }

    let by = if form.is_all_day() {
        day * i32::try_from(case.shift.signum()).unwrap()
    } else {
        SignedDuration::from_hours(case.shift)
    };
    let mut tail = fields.clone();
    tail.title = Some("following".to_owned());
    tail.times = moved_times(
        &EventTimes::new(
            at.original_start().clone(),
            Some(moved(at.original_start(), form, length)),
        )
        .unwrap(),
        form,
        by,
    );
    rule.end = RuleEnd::Never;
    tail.recurrence = Some(Recurrence::rule(rule));
    Some(Built {
        old,
        split: Split::new(
            EditId::new("edit-1"),
            ResourceId::new("old"),
            InstanceKey::new(at.original_start().clone()),
        ),
        tail,
    })
}

/// What the provider holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct World {
    old: EventSeries,
    new: Option<EventSeries>,
}

impl World {
    fn apply(&mut self, step: &SplitStep) {
        match step {
            SplitStep::Create { master, .. } => {
                if self.new.is_none() {
                    self.new = Some(EventSeries::single(master.clone()));
                }
            }
            SplitStep::Carry {
                instance,
                event,
                range,
                ..
            } => {
                self.new.as_mut().unwrap().overrides.insert(
                    instance.clone(),
                    Override::Modified {
                        event: Box::new(event.clone()),
                        range: *range,
                    },
                );
            }
            SplitStep::Trim { ours, .. } => {
                self.old.master.as_mut().unwrap().fields = ours.clone();
            }
            SplitStep::Remove { instance, .. } => {
                self.old.overrides.remove(instance);
            }
            SplitStep::Done => {}
        }
    }
}

/// One occurrence, as a user sees it: the instance's key, when it is, and its title.
type Seen = BTreeMap<InstanceKey, (Timestamp, Option<String>)>;

fn seen(series: Option<&EventSeries>) -> Seen {
    let zones = zones();
    let expander = Expander::new(&zones, floating());
    let Some(series) = series else {
        return Seen::new();
    };
    let expansion = expander.expand(series, &window());
    assert!(expansion.losses.is_empty(), "{:?}", expansion.losses);
    expansion
        .occurrences
        .into_iter()
        .map(|occurrence| {
            (
                occurrence.key,
                (occurrence.start, occurrence.event.fields.title.clone()),
            )
        })
        .collect()
}

/// Runs `splitter` on `world` until it is done, with the tail for the first `with_tail` writes
/// and without it after, as a device that never saw the edit. Checks every state it passes.
fn run(
    splitter: &Splitter<'_, TimeZoneDatabase>,
    built: &Built,
    world: &mut World,
    with_tail: usize,
    check: &mut dyn FnMut(&World),
) -> usize {
    let mut split = Some(built.split.clone());
    for written in 0..MAX_STEPS {
        let tail = (written < with_tail).then_some(&built.tail);
        if written == with_tail {
            // The device that started the split is gone. Another one knows the split only by
            // the new series' ids.
            split = world
                .new
                .as_ref()
                .and_then(|new| new.master.as_ref())
                .and_then(Split::correlate);
            if world.new.is_some() {
                assert_eq!(split.as_ref(), Some(&built.split));
            }
        }
        let Some(split) = &split else {
            return written;
        };
        let step = splitter
            .next(split, tail, &world.old, world.new.as_ref())
            .unwrap();
        if step == SplitStep::Done {
            return written;
        }
        if let SplitStep::Create { uncarried, .. } = &step {
            assert!(uncarried.is_empty());
        }
        world.apply(&step);
        check(world);
    }
    panic!("the split did not finish in {MAX_STEPS} writes");
}

/// The instant `key` names.
fn instant(key: &InstanceKey) -> Timestamp {
    let zone = match key.original_start() {
        TimeValue::Instant { timestamp } => return *timestamp,
        TimeValue::Zoned { time_zone, .. } => zones().get(time_zone.as_str()).unwrap(),
        TimeValue::Floating { .. } | TimeValue::AllDay { .. } => floating(),
    };
    zone.to_ambiguous_timestamp(civil(key.original_start()))
        .compatible()
        .unwrap()
}

/// The key an old instance has in the new series.
fn carried(key: &InstanceKey, built: &Built, form: Form) -> InstanceKey {
    let at = built.split.at.original_start();
    let by = civil(built.tail.times.start()).duration_since(civil(at));
    InstanceKey::new(moved(key.original_start(), form, by))
}

/// No occurrence is lost, none before the split changes, and nothing cancelled comes back.
fn assert_nothing_lost(before: &Seen, built: &Built, form: Form, world: &World) {
    let at = instant(&built.split.at);
    let old = seen(Some(&world.old));
    let new = seen(world.new.as_ref());
    for (key, (start, title)) in before {
        if instant(key) < at {
            assert_eq!(old.get(key), Some(&(*start, title.clone())), "{key:?}");
            continue;
        }
        if instant(key) >= checked_until() {
            // Moved by the split, it may fall past the window's end.
            continue;
        }
        let in_old = old.get(key).is_some_and(|(_, seen)| seen == title);
        let in_new = new
            .get(&carried(key, built, form))
            .is_some_and(|(_, seen)| {
                seen == title
                    || (title.as_deref() == Some("weekly") && seen.as_deref() == Some("following"))
            });
        assert!(in_old || in_new, "lost {key:?} ({title:?})");
    }
    for key in old.keys() {
        assert!(
            before.contains_key(key),
            "{key:?} came back in the old series"
        );
    }
    // Everything the old series excluded or cancelled stays so in the new one.
    for (key, value) in &built.old.overrides {
        if matches!(value, Override::Cancelled { .. }) && instant(key) >= at {
            assert!(
                !new.contains_key(&carried(key, built, form)),
                "{key:?} came back"
            );
        }
    }
    let excluded = &built
        .old
        .master
        .as_ref()
        .unwrap()
        .fields
        .recurrence
        .as_ref()
        .unwrap()
        .excluded_dates;
    for value in excluded {
        let key = InstanceKey::new(value.clone());
        if instant(&key) >= at {
            assert!(
                !new.contains_key(&carried(&key, built, form)),
                "{key:?} came back"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Interrupt the split after every write in turn; a device without the tail finishes it.
    #[test]
    fn a_crash_after_any_write_is_finished_by_any_device(case in case()) {
        let Some(built) = build(&case) else { return Ok(()) };
        let zones = zones();
        let splitter = Splitter::new(&zones, floating());
        let before = seen(Some(&built.old));
        let fresh = World { old: built.old.clone(), new: None };

        let mut finished = fresh.clone();
        let writes = run(&splitter, &built, &mut finished, usize::MAX, &mut |world| {
            assert_nothing_lost(&before, &built, case.form, world);
        });
        prop_assert!(writes >= 2, "a split writes at least the create and the trim");
        // Once finished, the old series ends before the split and the new one has the rest.
        let at = instant(&built.split.at);
        prop_assert!(seen(Some(&finished.old)).keys().all(|key| instant(key) < at));

        for crash in 0..=writes {
            let mut world = fresh.clone();
            run(&splitter, &built, &mut world, crash, &mut |world| {
                assert_nothing_lost(&before, &built, case.form, world);
            });
            if crash == 0 {
                // Nothing was written, so there is nothing to finish.
                prop_assert_eq!(&world, &fresh);
            } else {
                prop_assert_eq!(&world, &finished, "crash after {} writes", crash);
            }
        }
    }

    /// A write repeated after a lost response changes nothing the next plan relies on.
    #[test]
    fn a_repeated_write_is_harmless(case in case(), repeat in 0usize..8) {
        let Some(built) = build(&case) else { return Ok(()) };
        let zones = zones();
        let splitter = Splitter::new(&zones, floating());
        let mut once = World { old: built.old.clone(), new: None };
        run(&splitter, &built, &mut once, usize::MAX, &mut |_| {});

        let mut twice = World { old: built.old.clone(), new: None };
        for written in 0..MAX_STEPS {
            let step = splitter
                .next(&built.split, Some(&built.tail), &twice.old, twice.new.as_ref())
                .unwrap();
            if step == SplitStep::Done {
                break;
            }
            twice.apply(&step);
            if written == repeat {
                twice.apply(&step);
            }
        }
        prop_assert_eq!(twice, once);
    }

    /// The split comes back from the new series' Google id, `CalDAV` href, or iCalendar `UID`.
    #[test]
    fn a_new_series_names_its_split(
        series in "\\PC{0,40}",
        edit in "\\PC{0,40}",
        form in 0u8..4,
        seconds in 0i64..4_000_000_000,
        zone in "[A-Za-z_/]{1,30}",
    ) {
        let civil = TimeZone::UTC.to_datetime(Timestamp::from_second(seconds).unwrap());
        let at = match form {
            0 => TimeValue::instant(Timestamp::from_second(seconds).unwrap()),
            1 => TimeValue::zoned(civil, TimeZoneId::new(zone)),
            2 => TimeValue::floating(civil),
            _ => TimeValue::all_day(civil.date()),
        };
        let split = Split::new(EditId::new(edit), ResourceId::new(series), InstanceKey::new(at));
        let ids = split.ids();
        let fields = NormalizedEvent::new(EventTimes::at(TimeValue::all_day(civil.date())));

        let google = record(ids.google_id().as_str(), None, fields.clone());
        prop_assert_eq!(Split::correlate(&google), Some(split.clone()));

        let mut caldav = record(ids.caldav_href("/calendars/home/").as_str(), None, fields.clone());
        caldav.ids.provider = Provider::CalDav;
        prop_assert_eq!(Split::correlate(&caldav), Some(split.clone()));

        // Graph names the event itself; its UID still carries the key here, as CalDAV's does.
        let mut assigned = record("AAMkAGI2TAAA=", None, fields);
        assigned.ids.ical_uid = Some(ids.ical_uid());
        prop_assert_eq!(Split::correlate(&assigned), Some(split.clone()));
        prop_assert_eq!(Split::from_key(ids.idempotency_key().as_str()), Some(split));
    }

    /// An ordinary create, or any other id, is never taken for a split.
    #[test]
    fn nothing_else_names_a_split(edit in "\\PC{0,60}", noise in "[0-9a-v]{0,80}") {
        prop_assume!(!edit.starts_with("split1:"));
        let ordinary = DerivedIds::of(&EditId::new(edit));
        prop_assert_eq!(Split::from_key(ordinary.key()), None);
        if let Some(split) = Split::from_key(&format!("csedit{noise}")) {
            // Only a key a split derives is read as one.
            prop_assert_eq!(split.ids().key().to_owned(), format!("csedit{noise}"));
        }
    }
}

fn simple(form: Form, end: RuleEnd) -> EventSeries {
    let start: DateTime = "2026-03-01T09:30:00".parse().unwrap();
    let mut fields = NormalizedEvent::new(EventTimes::at(form.value(start)));
    let mut rule = RecurrenceRule::new(Frequency::Daily);
    rule.end = end;
    fields.recurrence = Some(Recurrence::rule(rule));
    EventSeries::single(record("old", None, fields))
}

fn split_at(civil: &str, form: Form) -> Split {
    Split::new(
        EditId::new("e"),
        ResourceId::new("old"),
        InstanceKey::new(form.value(civil.parse().unwrap())),
    )
}

fn trimmed_end(old: &EventSeries, split: &Split) -> RuleEnd {
    let zones = zones();
    let splitter = Splitter::new(&zones, floating());
    let tail = old.master.as_ref().unwrap().fields.clone();
    let mut world = World {
        old: old.clone(),
        new: None,
    };
    loop {
        let step = splitter
            .next(split, Some(&tail), &world.old, world.new.as_ref())
            .unwrap();
        if step == SplitStep::Done {
            break;
        }
        world.apply(&step);
    }
    let master = world.old.master.unwrap();
    master.fields.recurrence.unwrap().rules[0].end.clone()
}

#[test]
fn a_counted_rule_keeps_the_count_before_the_split() {
    let old = simple(Form::Zoned, RuleEnd::Count { count: 10 });
    let split = split_at("2026-03-05T09:30:00", Form::Zoned);
    assert_eq!(trimmed_end(&old, &split), RuleEnd::Count { count: 4 });
}

#[test]
fn a_rule_ends_the_second_before_the_split_in_the_form_rfc_5545_requires() {
    let zoned = simple(Form::Zoned, RuleEnd::Never);
    let split = split_at("2026-03-10T09:30:00", Form::Zoned);
    // 09:30 in New York on 10 March is 13:30 UTC, after the change to daylight time.
    assert_eq!(
        trimmed_end(&zoned, &split),
        RuleEnd::Until {
            until: TimeValue::instant("2026-03-10T13:29:59Z".parse().unwrap())
        }
    );
    let all_day = simple(Form::AllDay, RuleEnd::Never);
    let split = split_at("2026-03-10T00:00:00", Form::AllDay);
    assert_eq!(
        trimmed_end(&all_day, &split),
        RuleEnd::Until {
            until: TimeValue::all_day("2026-03-09".parse().unwrap())
        }
    );
    let floating = simple(Form::Floating, RuleEnd::Never);
    let split = split_at("2026-03-10T09:30:00", Form::Floating);
    assert_eq!(
        trimmed_end(&floating, &split),
        RuleEnd::Until {
            until: TimeValue::floating("2026-03-10T09:29:59".parse().unwrap())
        }
    );
}

#[test]
fn a_rule_already_ending_before_the_split_is_left_alone() {
    let until = TimeValue::instant("2026-03-03T00:00:00Z".parse().unwrap());
    let old = simple(
        Form::Zoned,
        RuleEnd::Until {
            until: until.clone(),
        },
    );
    let split = split_at("2026-03-10T09:30:00", Form::Zoned);
    let zones = zones();
    let splitter = Splitter::new(&zones, floating());
    let tail = old.master.as_ref().unwrap().fields.clone();
    // The old series has nothing at or after the split, so there is nothing to split.
    assert_eq!(
        splitter.next(&split, Some(&tail), &old, None),
        Ok(SplitStep::Done)
    );
}

#[test]
fn a_split_at_the_start_or_of_a_single_event_is_refused() {
    let zones = zones();
    let splitter = Splitter::new(&zones, floating());
    let old = simple(Form::Zoned, RuleEnd::Never);
    let tail = old.master.as_ref().unwrap().fields.clone();
    let split = split_at("2026-03-01T09:30:00", Form::Zoned);
    assert_eq!(
        splitter.next(&split, Some(&tail), &old, None),
        Err(SplitError::AtStart)
    );
    let mut single = old.clone();
    single.master.as_mut().unwrap().fields.recurrence = None;
    let split = split_at("2026-03-05T09:30:00", Form::Zoned);
    assert_eq!(
        splitter.next(&split, Some(&tail), &single, None),
        Err(SplitError::NotRecurring)
    );
}

#[test]
fn overrides_that_cannot_move_to_an_all_day_series_are_named() {
    let zones = zones();
    let splitter = Splitter::new(&zones, floating());
    let mut old = simple(Form::Zoned, RuleEnd::Never);
    let key = InstanceKey::new(Form::Zoned.value("2026-03-06T09:30:00".parse().unwrap()));
    old.overrides.insert(
        key.clone(),
        Override::Cancelled {
            ids: None,
            revision: None,
        },
    );
    let mut tail = old.master.as_ref().unwrap().fields.clone();
    tail.times = EventTimes::at(TimeValue::all_day("2026-03-05".parse().unwrap()));
    let split = split_at("2026-03-05T09:30:00", Form::Zoned);
    let step = splitter.next(&split, Some(&tail), &old, None).unwrap();
    let SplitStep::Create { uncarried, .. } = step else {
        panic!("{step:?}");
    };
    assert_eq!(uncarried, vec![key]);
}
