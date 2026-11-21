//! The instance index and free/busy, by property.
//!
//! - An index brought to its state by any sequence of series changes, horizon moves, time zone
//!   data changes, discards and interrupted refreshes, then refreshed, equals one rebuilt from
//!   nothing.
//! - A series that never ends holds only what overlaps the horizon, however often it rolls.
//! - A series finer than daily whose expansion runs out of the period budget rolls to what a
//!   rebuild holds, with the one loss of its horizon.
//! - Merged free/busy is ordered and disjoint, and takes time exactly where its inputs do.

use std::collections::BTreeMap;

use calendar_sync_engine::ResourceId;
use calendar_sync_engine::freebusy::{self, BusyInterval, BusyKind};
use calendar_sync_engine::index::{
    Horizon, IndexError, IndexMeta, IndexStore, InstanceIndex, MemoryStore, SeriesEntry,
    ZoneDataVersion,
};
use calendar_sync_model::{
    EventSeries, EventStatus, EventTimes, Frequency, InstanceKey, InstanceRange, MapperVersion,
    NormalizedEvent, Override, Passthrough, Provider, ProviderEvent, ProviderIds, Recurrence,
    RecurrenceRule, RuleEnd, TimeValue, TimeZoneId, Transparency,
};
use calendar_sync_recurrence::Window;
use jiff::civil::DateTime;
use jiff::tz::{self, TimeZone};
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;

/// The one named zone the series use. Its offset depends on the zone data version, so an entry
/// expanded with stale data differs from a fresh one.
const ZONE: &str = "Test/Zone";
const DAY: SignedDuration = SignedDuration::from_hours(24);

fn base() -> Timestamp {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

/// Time zone data at one version: `Test/Zone`'s offset, and the version's name.
#[derive(Clone, Copy, Debug)]
struct ZoneData {
    version: u8,
    floating: i8,
}

impl ZoneData {
    fn offset(self) -> i8 {
        if self.version == 0 { 2 } else { -5 }
    }

    fn version(self) -> ZoneDataVersion {
        ZoneDataVersion::new(format!("2026{}", char::from(b'a' + self.version)))
    }

    fn floating(self) -> TimeZone {
        TimeZone::fixed(tz::offset(self.floating))
    }

    fn zones(self) -> impl Fn(&TimeZoneId) -> Option<TimeZone> {
        let offset = self.offset();
        move |id: &TimeZoneId| (id.as_str() == ZONE).then(|| TimeZone::fixed(tz::offset(offset)))
    }
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
    fn value(self, civil: DateTime) -> TimeValue {
        match self {
            Self::Instant => TimeValue::instant(TimeZone::UTC.to_timestamp(civil).unwrap()),
            Self::Zoned => TimeValue::zoned(civil, TimeZoneId::new(ZONE)),
            Self::Floating => TimeValue::floating(civil),
            Self::AllDay => TimeValue::all_day(civil.date()),
        }
    }
}

#[derive(Clone, Debug)]
struct SeriesCase {
    form: Form,
    start_day: i64,
    start_hour: i64,
    /// Length in hours; zero is an event that takes no time (or one day, all-day).
    hours: i64,
    rule: Option<(Frequency, u32, u8)>,
    excluded: Option<i64>,
    cancelled: Option<i64>,
    modified: Option<(i64, i64, bool)>,
    status: EventStatus,
    transparency: Transparency,
}

fn form() -> impl Strategy<Value = Form> {
    prop_oneof![
        Just(Form::Instant),
        Just(Form::Zoned),
        Just(Form::Floating),
        Just(Form::AllDay),
    ]
}

fn series_case() -> impl Strategy<Value = SeriesCase> {
    (
        form(),
        0..60_i64,
        0..24_i64,
        0..4_i64,
        proptest::option::of((
            prop_oneof![Just(Frequency::Daily), Just(Frequency::Weekly)],
            1..4_u32,
            0..3_u8,
        )),
        proptest::option::of(0..12_i64),
        proptest::option::of(0..12_i64),
        proptest::option::of((0..12_i64, -30..30_i64, any::<bool>())),
        prop_oneof![
            Just(EventStatus::Confirmed),
            Just(EventStatus::Tentative),
            Just(EventStatus::Cancelled),
        ],
        prop_oneof![Just(Transparency::Opaque), Just(Transparency::Transparent)],
    )
        .prop_map(
            |(
                form,
                start_day,
                start_hour,
                hours,
                rule,
                excluded,
                cancelled,
                modified,
                status,
                transparency,
            )| SeriesCase {
                form,
                start_day,
                start_hour,
                hours,
                rule,
                excluded,
                cancelled,
                modified,
                status,
                transparency,
            },
        )
}

fn record(id: &str, times: EventTimes) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(Provider::CalDav, id),
        revision: None,
        mapper_version: MapperVersion::new(1),
        fields: NormalizedEvent::new(times),
        passthrough: Passthrough::new(),
    }
}

fn build(case: &SeriesCase) -> EventSeries {
    let civil = TimeZone::UTC
        .to_datetime(base())
        .checked_add(DAY * i32::try_from(case.start_day).unwrap())
        .unwrap()
        .checked_add(SignedDuration::from_hours(case.start_hour))
        .unwrap();
    let times = |civil: DateTime| {
        let end = (case.hours > 0).then(|| {
            let length = if matches!(case.form, Form::AllDay) {
                DAY * i32::try_from(case.hours).unwrap()
            } else {
                SignedDuration::from_hours(case.hours)
            };
            case.form.value(civil.checked_add(length).unwrap())
        });
        EventTimes::new(case.form.value(civil), end).unwrap()
    };
    let mut master = record("master", times(civil));
    master.fields.status = case.status;
    master.fields.transparency = case.transparency;
    let mut series = EventSeries::single(master);
    let Some((frequency, interval, end)) = case.rule else {
        return series;
    };

    let step = match frequency {
        Frequency::Weekly => DAY * 7,
        _ => DAY,
    } * i32::try_from(interval).unwrap();
    let nth = |n: i64| civil.checked_add(step * i32::try_from(n).unwrap()).unwrap();
    let mut rule = RecurrenceRule::new(frequency);
    rule.interval = core::num::NonZeroU32::new(interval).unwrap();
    rule.end = match end {
        0 => RuleEnd::Never,
        1 => RuleEnd::Count { count: 9 },
        _ => RuleEnd::Until {
            until: case.form.value(nth(7)),
        },
    };
    let mut recurrence = Recurrence::rule(rule);
    if let Some(n) = case.excluded {
        recurrence.excluded_dates.push(case.form.value(nth(n)));
    }
    let master = series.master.as_mut().unwrap();
    master.fields.recurrence = Some(recurrence);
    if let Some(n) = case.cancelled {
        series.overrides.insert(
            InstanceKey::new(case.form.value(nth(n))),
            Override::Cancelled {
                ids: None,
                revision: None,
            },
        );
    }
    if let Some((n, by, future)) = case.modified {
        let moved = nth(n).checked_add(SignedDuration::from_hours(by)).unwrap();
        let mut event = record("override", times(moved));
        event.fields.status = EventStatus::Tentative;
        series.overrides.insert(
            InstanceKey::new(case.form.value(nth(n))),
            Override::Modified {
                event: Box::new(event),
                range: if future {
                    InstanceRange::ThisAndFuture
                } else {
                    InstanceRange::ThisInstance
                },
            },
        );
    }
    series
}

fn window(start_day: i64, days: i64) -> Window {
    let start = base()
        .checked_add(DAY * i32::try_from(start_day).unwrap())
        .unwrap();
    Window::new(
        start,
        start
            .checked_add(DAY * i32::try_from(days).unwrap())
            .unwrap(),
    )
    .unwrap()
}

#[derive(Clone, Debug)]
enum Op {
    Upsert(u8, SeriesCase),
    Remove(u8),
    /// The horizon moves to start this many days from the base, this many days long.
    Roll(i64, i64),
    Zones(ZoneData),
    /// The store drops the cache.
    Discard,
    /// A roll that fails after this many writes to the store.
    Interrupted(i64, i64, u8),
}

fn zone_data() -> impl Strategy<Value = ZoneData> {
    (0..2_u8, prop_oneof![Just(0_i8), Just(-8_i8)])
        .prop_map(|(version, floating)| ZoneData { version, floating })
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..4_u8, series_case()).prop_map(|(id, case)| Op::Upsert(id, case)),
        1 => (0..4_u8).prop_map(Op::Remove),
        3 => (-20..80_i64, 1..40_i64).prop_map(|(start, days)| Op::Roll(start, days)),
        2 => zone_data().prop_map(Op::Zones),
        1 => Just(Op::Discard),
        1 => (-20..80_i64, 1..40_i64, 0..6_u8)
            .prop_map(|(start, days, budget)| Op::Interrupted(start, days, budget)),
    ]
}

/// A store that fails once it has taken `budget` writes.
struct Failing<'s> {
    inner: &'s mut MemoryStore,
    budget: u8,
}

#[derive(Debug)]
struct Crashed;

impl Failing<'_> {
    fn write(&mut self) -> Result<(), Crashed> {
        if self.budget == 0 {
            return Err(Crashed);
        }
        self.budget -= 1;
        Ok(())
    }
}

impl IndexStore for Failing<'_> {
    type Error = Crashed;

    fn meta(&self) -> Result<Option<IndexMeta>, Crashed> {
        Ok(self.inner.meta_ref().cloned())
    }

    fn set_meta(&mut self, meta: Option<&IndexMeta>) -> Result<(), Crashed> {
        self.write()?;
        self.inner.set_meta(meta).map_err(|never| match never {})
    }

    fn entry(&self, id: &ResourceId) -> Result<Option<SeriesEntry>, Crashed> {
        Ok(self.inner.entry_ref(id).cloned())
    }

    fn put(&mut self, id: &ResourceId, entry: SeriesEntry) -> Result<(), Crashed> {
        self.write()?;
        self.inner.put(id, entry).map_err(|never| match never {})
    }

    fn remove(&mut self, id: &ResourceId) -> Result<(), Crashed> {
        self.write()?;
        self.inner.remove(id).map_err(|never| match never {})
    }

    fn ids(&self) -> Result<Vec<ResourceId>, Crashed> {
        Ok(self.inner.entries().keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), Crashed> {
        self.write()?;
        self.inner.clear().map_err(|never| match never {})
    }
}

fn id(n: u8) -> ResourceId {
    ResourceId::new(format!("series-{n}"))
}

/// Every instance in a current index overlaps its horizon.
fn assert_within_horizon(store: &MemoryStore) {
    if let Some(horizon) = store.meta_ref().and_then(IndexMeta::horizon) {
        for entry in store.entries().values() {
            for instance in &entry.instances {
                assert!(
                    horizon.overlaps(instance.start, instance.end),
                    "{instance:?} outside {horizon:?}"
                );
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn incremental_equals_rebuild(
        initial in zone_data(),
        ops in proptest::collection::vec(op(), 0..24),
        last in (-20..80_i64, 1..40_i64),
    ) {
        let mut zones = initial;
        let mut series: BTreeMap<ResourceId, EventSeries> = BTreeMap::new();
        let mut store = MemoryStore::new();
        {
            let source = zones.zones();
            let index = InstanceIndex::new(&source, zones.floating(), zones.version());
            index.refresh(&mut store, window(0, 30), &series).unwrap();
        }

        for op in ops {
            let source = zones.zones();
            let index = InstanceIndex::new(&source, zones.floating(), zones.version());
            match op {
                Op::Upsert(n, case) => {
                    let built = build(&case);
                    match index.upsert(&mut store, &id(n), &built) {
                        Ok(()) | Err(IndexError::Stale | IndexError::NoHorizon) => {}
                        Err(error) => panic!("{error:?}"),
                    }
                    series.insert(id(n), built);
                }
                Op::Remove(n) => {
                    index.remove(&mut store, &id(n)).unwrap();
                    series.remove(&id(n));
                }
                Op::Roll(start, days) => {
                    index.refresh(&mut store, window(start, days), &series).unwrap();
                }
                Op::Zones(next) => zones = next,
                Op::Discard => store.clear().unwrap(),
                Op::Interrupted(start, days, budget) => {
                    let mut failing = Failing { inner: &mut store, budget };
                    // It may finish within its budget; either way the next refresh repairs it.
                    let _ = index.refresh(&mut failing, window(start, days), &series);
                }
            }
            assert_within_horizon(&store);
        }

        let source = zones.zones();
        let index = InstanceIndex::new(&source, zones.floating(), zones.version());
        let horizon = window(last.0, last.1);
        index.refresh(&mut store, horizon, &series).unwrap();
        let mut rebuilt = MemoryStore::new();
        index.rebuild(&mut rebuilt, horizon, &series).unwrap();
        prop_assert_eq!(&store, &rebuilt);

        // Free/busy read from the index is the merge of what a fresh expansion holds.
        let direct = freebusy::merge(
            rebuilt.entries().values().flat_map(|entry| {
                entry.instances.iter().filter_map(|instance| {
                    instance.busy.map(|kind| BusyInterval::new(instance.start, instance.end, kind))
                })
            }),
            &horizon,
        );
        prop_assert_eq!(index.free_busy(&store, &horizon).unwrap(), direct);
    }

    #[test]
    fn an_infinite_series_is_bounded_by_the_horizon(
        case in series_case(),
        frequency in prop_oneof![Just(Frequency::Daily), Just(Frequency::Weekly)],
        days in 1..60_i64,
        steps in proptest::collection::vec(0..90_i64, 1..20),
    ) {
        let mut case = case;
        case.rule = Some((frequency, 1, 0));
        case.cancelled = None;
        case.modified = None;
        case.excluded = None;
        let series = build(&case);
        let id = id(0);
        let zones = ZoneData { version: 0, floating: 0 };
        let source = zones.zones();
        let index = InstanceIndex::new(&source, zones.floating(), zones.version());
        let length = Horizon::new(SignedDuration::ZERO, DAY * i32::try_from(days).unwrap());
        let mut store = MemoryStore::new();
        let mut now = base();
        for step in steps {
            now = now.checked_add(DAY * i32::try_from(step).unwrap()).unwrap();
            let horizon = length.window_at(now).unwrap();
            index.refresh(&mut store, horizon, [(&id, &series)]).unwrap();
            assert_within_horizon(&store);
            let period = if frequency == Frequency::Weekly { 7 } else { 1 };
            // One occurrence per period that starts in the horizon, one more that runs into it
            // from before, and one more for the series' first instance, which need not fall on
            // the rule's grid.
            let bound = usize::try_from(days / period + 3).unwrap();
            let held = store.entry_ref(&id).map_or(0, |entry| entry.instances.len());
            prop_assert!(held <= bound, "{held} instances over {days} days");
        }
    }

    #[test]
    fn merged_free_busy_takes_exactly_its_inputs_time(
        intervals in proptest::collection::vec(
            (0..200_i64, 0..40_i64, any::<bool>()),
            0..30,
        ),
        range in (0..100_i64, 1..150_i64),
    ) {
        let minute = |n: i64| base().checked_add(SignedDuration::from_mins(n)).unwrap();
        let intervals: Vec<BusyInterval> = intervals
            .into_iter()
            .map(|(start, length, busy)| BusyInterval::new(
                minute(start),
                minute(start + length),
                if busy { BusyKind::Busy } else { BusyKind::Tentative },
            ))
            .collect();
        let range = Window::new(minute(range.0), minute(range.0 + range.1)).unwrap();
        let merged = freebusy::merge(intervals.iter().copied(), &range);

        for pair in merged.windows(2) {
            prop_assert!(pair[0].end <= pair[1].start, "{merged:?} overlaps");
            prop_assert!(
                pair[0].end < pair[1].start || pair[0].kind != pair[1].kind,
                "{merged:?} leaves touching intervals of one kind apart"
            );
        }
        for interval in &merged {
            prop_assert!(interval.start < interval.end);
            prop_assert!(interval.start >= range.start() && interval.end <= range.end());
        }
        // Every minute is taken as the strongest input that covers it, within the range.
        for n in -1..250 {
            let at = minute(n);
            let expected = if range.start() <= at && at < range.end() {
                intervals
                    .iter()
                    .filter(|interval| interval.start <= at && at < interval.end)
                    .map(|interval| interval.kind)
                    .max()
            } else {
                None
            };
            let actual = merged
                .iter()
                .find(|interval| interval.start <= at && at < interval.end)
                .map(|interval| interval.kind);
            prop_assert_eq!(actual, expected, "at minute {}", n);
        }
    }
}

proptest! {
    // Each expansion examines the whole period budget, so few cases and one roll.
    #![proptest_config(ProptestConfig::with_cases(3))]

    #[test]
    fn a_series_past_the_period_budget_rolls_as_it_rebuilds(
        offset in 0..60_i64,
        days in 24..40_i64,
        roll in 1..6_i64,
    ) {
        // Every second is a period, and only the first of each minute an instance: a horizon
        // over 23 days runs out of the recurrence crate's period budget part-way through.
        let mut rule = RecurrenceRule::new(Frequency::Secondly);
        rule.by_second = vec![0];
        let start = base().checked_add(SignedDuration::from_mins(offset)).unwrap();
        let mut master = record(
            "master",
            EventTimes::new(
                TimeValue::instant(start),
                Some(TimeValue::instant(start.checked_add(SignedDuration::from_secs(30)).unwrap())),
            )
            .unwrap(),
        );
        master.fields.recurrence = Some(Recurrence::rule(rule));
        let series = EventSeries::single(master);
        let id = id(0);
        let zones = ZoneData { version: 0, floating: 0 };
        let source = zones.zones();
        let index = InstanceIndex::new(&source, zones.floating(), zones.version());
        let length = Horizon::new(SignedDuration::ZERO, DAY * i32::try_from(days).unwrap());
        let mut store = MemoryStore::new();
        index.refresh(&mut store, length.window_at(base()).unwrap(), [(&id, &series)]).unwrap();
        let now = base().checked_add(DAY * i32::try_from(roll).unwrap()).unwrap();
        let horizon = length.window_at(now).unwrap();
        index.refresh(&mut store, horizon, [(&id, &series)]).unwrap();
        let mut rebuilt = MemoryStore::new();
        index.rebuild(&mut rebuilt, horizon, [(&id, &series)]).unwrap();
        prop_assert_eq!(&store, &rebuilt);
        // The loss is the horizon's, not one per roll.
        let losses = store.entry_ref(&id).map_or(0, |entry| entry.losses.len());
        prop_assert!(losses == 1, "{losses} losses after a roll");
    }
}

#[test]
fn reads_refuse_an_index_that_is_absent_stale_or_too_short() {
    let old = ZoneData {
        version: 0,
        floating: 0,
    };
    let new = ZoneData {
        version: 1,
        floating: 0,
    };
    let (old_source, new_source) = (old.zones(), new.zones());
    let old_index = InstanceIndex::new(&old_source, old.floating(), old.version());
    let new_index = InstanceIndex::new(&new_source, new.floating(), new.version());
    let series: BTreeMap<ResourceId, EventSeries> = BTreeMap::new();
    let horizon = window(0, 10);
    let mut store = MemoryStore::new();

    assert_eq!(
        old_index.free_busy(&store, &horizon),
        Err(IndexError::NoHorizon)
    );
    old_index.refresh(&mut store, horizon, &series).unwrap();
    assert_eq!(old_index.free_busy(&store, &horizon), Ok(Vec::new()));
    assert_eq!(
        old_index.free_busy(&store, &window(5, 10)),
        Err(IndexError::OutsideHorizon)
    );
    assert_eq!(
        new_index.free_busy(&store, &horizon),
        Err(IndexError::Stale)
    );
    let case = SeriesCase {
        form: Form::Zoned,
        start_day: 1,
        start_hour: 9,
        hours: 1,
        rule: None,
        excluded: None,
        cancelled: None,
        modified: None,
        status: EventStatus::Confirmed,
        transparency: Transparency::Opaque,
    };
    assert_eq!(
        new_index.upsert(&mut store, &id(0), &build(&case)),
        Err(IndexError::Stale)
    );

    // A new database moves the zoned event, and a refresh picks the move up.
    let mut series = series;
    series.insert(id(0), build(&case));
    old_index.refresh(&mut store, horizon, &series).unwrap();
    let before = old_index.instances(&store, &horizon).unwrap();
    new_index.refresh(&mut store, horizon, &series).unwrap();
    let after = new_index.instances(&store, &horizon).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].1.start.duration_since(before[0].1.start),
        SignedDuration::from_hours(7)
    );
}

#[test]
fn busy_follows_status_and_transparency() {
    let mut fields = NormalizedEvent::new(EventTimes::at(TimeValue::instant(base())));
    assert_eq!(BusyKind::of(&fields), Some(BusyKind::Busy));
    fields.status = EventStatus::Tentative;
    assert_eq!(BusyKind::of(&fields), Some(BusyKind::Tentative));
    fields.transparency = Transparency::Transparent;
    assert_eq!(BusyKind::of(&fields), None);
    fields.transparency = Transparency::Opaque;
    fields.status = EventStatus::Cancelled;
    assert_eq!(BusyKind::of(&fields), None);
}
