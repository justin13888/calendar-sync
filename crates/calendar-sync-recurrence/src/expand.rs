//! A series expanded over a window: its rules, extra dates, excluded dates and overrides.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use calendar_sync_model::{
    EventSeries, EventTimes, Field, Frequency, InstanceKey, InstanceRange, Loss, LossReason,
    Override, ProviderEvent, RecurrenceRule, RuleEnd, TimeKind, TimeValue, TimeZoneId,
};
use jiff::civil::{DateTime, Time};
use jiff::tz::{TimeZone, TimeZoneDatabase};
use jiff::{SignedDuration, Timestamp};

use crate::rule::{CivilRule, PERIOD_BUDGET};

/// Slack, beyond an event's own length, by which the expanded range exceeds the window. It
/// covers the widest UTC offset either side of a civil time (about 26 hours).
const SLACK: SignedDuration = SignedDuration::from_hours(48);

/// Where time zones come from.
///
/// The expander reads no time zone database of its own; the caller supplies one. A
/// [`TimeZoneDatabase`] is one, and so is any closure from a [`TimeZoneId`] to a [`TimeZone`].
pub trait ZoneSource {
    /// The zone `id` names, or `None` if this source does not know it.
    fn time_zone(&self, id: &TimeZoneId) -> Option<TimeZone>;
}

impl ZoneSource for TimeZoneDatabase {
    fn time_zone(&self, id: &TimeZoneId) -> Option<TimeZone> {
        self.get(id.as_str()).ok()
    }
}

impl<F> ZoneSource for F
where
    F: Fn(&TimeZoneId) -> Option<TimeZone>,
{
    fn time_zone(&self, id: &TimeZoneId) -> Option<TimeZone> {
        self(id)
    }
}

/// The span of time to expand over: from `start`, inclusive, to `end`, exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Window {
    start: Timestamp,
    end: Timestamp,
}

impl Window {
    /// The window from `start`, inclusive, to `end`, exclusive.
    ///
    /// # Errors
    ///
    /// [`EmptyWindow`] if `end` is not after `start`.
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self, EmptyWindow> {
        if end > start {
            Ok(Self { start, end })
        } else {
            Err(EmptyWindow)
        }
    }

    /// Where the window starts, inclusive.
    #[must_use]
    pub fn start(&self) -> Timestamp {
        self.start
    }

    /// Where the window ends, exclusive.
    #[must_use]
    pub fn end(&self) -> Timestamp {
        self.end
    }

    /// Whether an occurrence from `start` to `end` falls in the window.
    ///
    /// This is the `CalDAV` time-range test (RFC 4791 section 9.9): an occurrence with a length
    /// overlaps the window, and one without a length starts inside it.
    #[must_use]
    pub fn overlaps(&self, start: Timestamp, end: Timestamp) -> bool {
        if end > start {
            start < self.end && end > self.start
        } else {
            start >= self.start && start < self.end
        }
    }
}

/// A [`Window`] whose end is not after its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyWindow;

impl core::fmt::Display for EmptyWindow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("a window must end after it starts")
    }
}

impl core::error::Error for EmptyWindow {}

/// One occurrence of a series.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence<'s> {
    /// The instance's identity: its original start, before any override moved it, in the form
    /// the series' start (or the `RDATE` that added it) is written in. For an override with no
    /// master instance, the override's own key.
    pub key: InstanceKey,
    /// When this occurrence starts and ends, after any override.
    pub times: EventTimes,
    /// [`Occurrence::times`]' start, as an instant.
    pub start: Timestamp,
    /// [`Occurrence::times`]' end, as an instant. Equal to [`Occurrence::start`] for an
    /// occurrence that takes no time.
    pub end: Timestamp,
    /// The record whose fields this occurrence shows: the master, or the override that changed
    /// it.
    pub event: &'s ProviderEvent,
    /// Where the occurrence came from.
    pub source: Source,
}

/// Where an [`Occurrence`] came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The master's rules or extra dates, unchanged.
    Master,
    /// An override. For [`InstanceRange::ThisAndFuture`] it may be one keyed at an earlier
    /// instance, whose change carries forward to this one.
    Override {
        /// The override's own key.
        key: InstanceKey,
        /// Which instances the override applies to.
        range: InstanceRange,
    },
}

/// The occurrences of a series in a window, and what the expansion could not honour.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expansion<'s> {
    /// The occurrences that fall in the window, ordered by start, then by key.
    pub occurrences: Vec<Occurrence<'s>>,
    /// What could not be expanded. A series whose rule, calendar scale or time zone this
    /// library cannot read yields no occurrences at all and says why here: it never guesses.
    pub losses: Vec<Loss>,
}

/// Expands series over windows, with time zones from a [`ZoneSource`].
///
/// Expansion reads no clock: the window is an input.
#[derive(Debug)]
pub struct Expander<'z, Z: ?Sized> {
    zones: &'z Z,
    floating: TimeZone,
}

impl<'z, Z: ZoneSource + ?Sized> Expander<'z, Z> {
    /// An expander resolving named zones through `zones`, and floating and all-day times in
    /// `floating`: the zone of whoever is looking at the calendar.
    pub fn new(zones: &'z Z, floating: TimeZone) -> Self {
        Self { zones, floating }
    }

    /// The occurrences of `series` that fall in `window`.
    ///
    /// The recurrence set follows RFC 5545 section 3.8.5 with its errata: the start, every
    /// rule's instances and every extra date, less every excluded date and every exclusion
    /// rule's instances, with duplicates kept once. Then each override applies: a cancelled
    /// instance is removed, a modified one replaced, and a `RANGE=THISANDFUTURE` change carries
    /// to every later instance, shifted as the keyed instance was shifted.
    ///
    /// A civil time a rule produces in a daylight-saving gap or fold is placed as section 3.3.5
    /// says, as errata 4271 to section 3.3.10 directs: in a fold, the first occurrence; in a
    /// gap, the offset before the gap, so the instance moves later by the gap's length. This is
    /// jiff's "compatible" disambiguation.
    ///
    /// A series without a master, such as an invitation to one instance, yields its modified
    /// overrides as they are.
    #[must_use]
    pub fn expand<'s>(&self, series: &'s EventSeries, window: &Window) -> Expansion<'s> {
        let mut expansion = Expansion::default();
        match self.try_expand(series, window, &mut expansion.losses) {
            Ok(occurrences) => expansion.occurrences = occurrences,
            Err(loss) => expansion.losses.push(loss),
        }
        expansion
            .occurrences
            .sort_by(|a, b| (a.start, a.end, &a.key).cmp(&(b.start, b.end, &b.key)));
        expansion
    }

    fn try_expand<'s>(
        &self,
        series: &'s EventSeries,
        window: &Window,
        losses: &mut Vec<Loss>,
    ) -> Result<Vec<Occurrence<'s>>, Loss> {
        let place = self.places(series)?;
        let Some(master) = &series.master else {
            return Ok(orphans(series, &place, window, losses));
        };
        let recurrence = master.fields.recurrence.as_ref();
        if let Some(rule) = recurrence
            .into_iter()
            .flat_map(|r| r.rules.iter().chain(&r.exclusion_rules))
            .find(|rule| !rule.is_gregorian())
        {
            // RFC 7529 section 6, option 2: reject the component, and its overrides with it.
            return Err(Loss::new(
                Field::Recurrence,
                LossReason::Unsupported,
                format!(
                    "RSCALE={} is not a calendar this library expands; the series is not shown",
                    rule.rscale.as_deref().unwrap_or_default()
                ),
            ));
        }

        let template = &master.fields.times;
        let first = template.start();
        let frame = place.frame(first)?;
        let rules = compile(
            recurrence.map(|r| &r.rules[..]),
            first,
            &frame,
            &place,
            true,
            losses,
        )?;
        let exclusion_rules = compile(
            recurrence.map(|r| &r.exclusion_rules[..]),
            first,
            &frame,
            &place,
            false,
            losses,
        )?;

        let spans = Self::spans(series, window, &frame, &place);

        // The recurrence set, keyed by instant so duplicates collapse (RFC 5545 section 3.8.5.3).
        let mut set: BTreeMap<Timestamp, Instance> = BTreeMap::new();
        let add = |value: TimeValue, set: &mut BTreeMap<Timestamp, Instance>| {
            if let Some(start) = place.instant(&value)
                && let Entry::Vacant(slot) = set.entry(start)
            {
                slot.insert(Instance {
                    value,
                    start,
                    change: None,
                });
            }
        };
        add(first.clone(), &mut set);
        for rule in &rules {
            for civil in expand_rule(rule, &spans, losses) {
                add(frame.value_at(civil), &mut set);
            }
        }
        for date in recurrence.map(|r| &r.dates[..]).unwrap_or_default() {
            if date.kind().pairs_with(first.kind()) {
                add(date.clone(), &mut set);
            } else {
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Malformed,
                    format!(
                        "an RDATE of form {:?} cannot join a series starting {:?}; it is not shown",
                        date.kind(),
                        first.kind()
                    ),
                ));
            }
        }

        // Exclusions take precedence over inclusions (RFC 5545 section 3.8.5.1).
        let mut excluded: Vec<Instance> = Vec::new();
        for date in recurrence
            .map(|r| &r.excluded_dates[..])
            .unwrap_or_default()
        {
            let date_start = place.instant(date);
            let removed: Vec<Timestamp> = set
                .values()
                .filter(|instance| matches(date, date_start, instance))
                .map(|instance| instance.start)
                .collect();
            excluded.extend(removed.iter().filter_map(|start| set.remove(start)));
        }
        for rule in &exclusion_rules {
            for civil in expand_rule(rule, &spans, losses) {
                if let Some(start) = place.instant(&frame.value_at(civil)) {
                    excluded.extend(set.remove(&start));
                }
            }
        }

        let mut occurrences = Vec::new();
        let mut futures: Vec<Future<'s>> = Vec::new();
        for (key, change) in &series.overrides {
            let value = key.original_start();
            let key_start = place.instant(value);
            let found = set
                .values()
                .find(|instance| matches(value, key_start, instance))
                .map(|instance| instance.start);
            match change {
                Override::Cancelled { .. } => {
                    if let Some(start) = found {
                        set.remove(&start);
                    }
                }
                Override::Modified { event, range } => {
                    if *range == InstanceRange::ThisAndFuture
                        && let Some(key_start) = key_start
                    {
                        futures.push(Future {
                            key: value,
                            key_start,
                            event,
                        });
                    }
                    let source = Source::Override {
                        key: key.clone(),
                        range: *range,
                    };
                    if let Some(instance) = found.and_then(|start| set.get_mut(&start)) {
                        instance.change = Some((event, source));
                    } else if !excluded.iter().any(|i| matches(value, key_start, i)) {
                        // An override of an instance the rules never produced: the provider
                        // holds it as an event, so it is shown where it says.
                        occurrences.extend(occurrence(
                            key.clone(),
                            event.fields.times.clone(),
                            event,
                            source,
                            &place,
                            losses,
                        ));
                    }
                }
            }
        }
        futures.sort_by_key(|future| future.key_start);

        for instance in set.into_values() {
            let key = InstanceKey::new(instance.value.clone());
            if let Some((event, source)) = instance.change {
                occurrences.extend(occurrence(
                    key,
                    event.fields.times.clone(),
                    event,
                    source,
                    &place,
                    losses,
                ));
                continue;
            }
            let carried = futures
                .iter()
                .rev()
                .find(|future| future.key_start < instance.start);
            let (times, event, source) = match carried {
                None => (
                    times_at(&instance.value, instance.start, template, &place),
                    master,
                    Source::Master,
                ),
                Some(future) => {
                    let start = shift(&instance, future, &place);
                    let times = start.and_then(|(value, at)| {
                        times_at(&value, at, &future.event.fields.times, &place)
                    });
                    let source = Source::Override {
                        key: InstanceKey::new(future.key.clone()),
                        range: InstanceRange::ThisAndFuture,
                    };
                    (times, future.event, source)
                }
            };
            match times {
                Some(times) => {
                    occurrences.extend(occurrence(key, times, event, source, &place, losses));
                }
                None => losses.push(Loss::new(
                    Field::Times,
                    LossReason::Malformed,
                    format!(
                        "the instance at {:?} cannot be placed in time",
                        instance.value
                    ),
                )),
            }
        }
        occurrences.retain(|o| window.overlaps(o.start, o.end));
        Ok(occurrences)
    }

    /// Resolves every zone the series names, or says which one is unknown.
    fn places<'p>(&'p self, series: &EventSeries) -> Result<Place<'p>, Loss> {
        let mut zones = BTreeMap::new();
        // Every time value the expansion may need to place.
        let mut values: Vec<&TimeValue> = Vec::new();
        if let Some(master) = &series.master {
            collect_times(&master.fields.times, &mut values);
            if let Some(recurrence) = &master.fields.recurrence {
                values.extend(&recurrence.dates);
                values.extend(&recurrence.excluded_dates);
                for rule in recurrence.rules.iter().chain(&recurrence.exclusion_rules) {
                    if let RuleEnd::Until { until } = &rule.end {
                        values.push(until);
                    }
                }
            }
        }
        for (key, change) in &series.overrides {
            values.push(key.original_start());
            if let Override::Modified { event, .. } = change {
                collect_times(&event.fields.times, &mut values);
            }
        }
        for value in values {
            if let TimeValue::Zoned { time_zone, .. } = value {
                if zones.contains_key(time_zone) {
                    continue;
                }
                let Some(zone) = self.zones.time_zone(time_zone) else {
                    return Err(Loss::new(
                        Field::Times,
                        LossReason::Unsupported,
                        format!(
                            "the time zone {time_zone} is not in the time zone database; the series is not shown"
                        ),
                    ));
                };
                zones.insert(time_zone.clone(), zone);
            }
        }
        Ok(Place {
            zones,
            floating: &self.floating,
        })
    }

    /// The civil spans, in the series' frame, that the rules are expanded over. The first is the
    /// window, widened by the longest event and the furthest carried shift. Each other one is
    /// the neighbourhood of an override's key that lies outside it, where that override moves
    /// its instance into the window, so that the override can find its instance. A key whose
    /// override cannot reach the window needs no span: its instance, moved or not, is not shown.
    /// The spans are kept apart rather than joined, so that a key years before the window does
    /// not spend the rule's budget on the years between.
    fn spans(
        series: &EventSeries,
        window: &Window,
        frame: &Frame,
        place: &Place<'_>,
    ) -> Vec<(DateTime, DateTime)> {
        let length = |times: &EventTimes| {
            let start = place.instant(times.start());
            let end = times.end().and_then(|end| place.instant(end));
            match (start, end) {
                (Some(start), Some(end)) => end.duration_since(start).abs(),
                _ => SignedDuration::ZERO,
            }
        };
        let mut longest = series
            .master
            .as_ref()
            .map_or(SignedDuration::ZERO, |master| length(&master.fields.times));
        let mut furthest = SignedDuration::ZERO;
        let mut keys = Vec::new();
        for (key, change) in &series.overrides {
            if let Override::Modified { event, range } = change {
                if reaches(&event.fields.times, window, place) {
                    keys.push(frame.civil_of(key.original_start(), place));
                }
                longest = longest.max(length(&event.fields.times));
                if *range == InstanceRange::ThisAndFuture
                    && let (Some(from), Some(to)) = (
                        place.instant(key.original_start()),
                        place.instant(event.fields.times.start()),
                    )
                {
                    furthest = furthest.max(to.duration_since(from).abs());
                }
            }
        }
        let reach = SLACK.saturating_add(longest).saturating_add(furthest);
        let low = frame.zone.to_datetime(window.start);
        let high = frame.zone.to_datetime(window.end);
        let low = low.checked_sub(reach).unwrap_or(DateTime::MIN);
        let high = high.checked_add(reach).unwrap_or(DateTime::MAX);
        let mut spans = vec![(low, high)];
        for key in keys.into_iter().flatten() {
            if (low..=high).contains(&key) {
                continue;
            }
            spans.push((
                key.checked_sub(SLACK).unwrap_or(DateTime::MIN),
                key.checked_add(SLACK).unwrap_or(DateTime::MAX),
            ));
        }
        spans
    }
}

/// Whether an override's times overlap the window, so that the occurrence it makes is shown.
fn reaches(times: &EventTimes, window: &Window, place: &Place<'_>) -> bool {
    let Some(start) = place.instant(times.start()) else {
        return false;
    };
    let end = times
        .end()
        .and_then(|end| place.instant(end))
        .unwrap_or(start)
        .max(start);
    window.overlaps(start, end)
}

/// Pushes an event's start and end.
fn collect_times<'t>(times: &'t EventTimes, values: &mut Vec<&'t TimeValue>) {
    values.push(times.start());
    values.extend(times.end());
}

/// One member of the recurrence set.
struct Instance<'s> {
    /// The original start, in the form the series start or `RDATE` gives it.
    value: TimeValue,
    /// The original start, as an instant.
    start: Timestamp,
    /// The override that replaces this instance, if any.
    change: Option<(&'s ProviderEvent, Source)>,
}

/// A `RANGE=THISANDFUTURE` change, carried to every later instance.
struct Future<'s> {
    key: &'s TimeValue,
    key_start: Timestamp,
    event: &'s ProviderEvent,
}

/// Every zone the series names, resolved, and the zone floating times are seen in.
struct Place<'p> {
    zones: BTreeMap<TimeZoneId, TimeZone>,
    floating: &'p TimeZone,
}

impl Place<'_> {
    /// The zone a value's civil time is read in: UTC for an instant, its own for a zoned value,
    /// and the floating zone for floating and all-day values.
    fn zone_of(&self, value: &TimeValue) -> Option<TimeZone> {
        match value {
            TimeValue::Instant { .. } => Some(TimeZone::UTC),
            TimeValue::Zoned { time_zone, .. } => self.zones.get(time_zone).cloned(),
            TimeValue::Floating { .. } | TimeValue::AllDay { .. } => Some(self.floating.clone()),
        }
    }

    /// `value` as an instant. A civil time in a gap or fold is placed as RFC 5545 section 3.3.5
    /// says, which is jiff's "compatible" disambiguation.
    fn instant(&self, value: &TimeValue) -> Option<Timestamp> {
        if let TimeValue::Instant { timestamp } = value {
            return Some(*timestamp);
        }
        self.zone_of(value)?
            .to_ambiguous_timestamp(civil(value))
            .compatible()
            .ok()
    }

    /// The frame a series starting at `first` iterates in.
    fn frame(&self, first: &TimeValue) -> Result<Frame, Loss> {
        let zone = self.zone_of(first).ok_or_else(|| {
            Loss::new(
                Field::Times,
                LossReason::Unsupported,
                "the series' time zone is not in the time zone database",
            )
        })?;
        Ok(Frame {
            zone,
            form: first.clone(),
        })
    }
}

/// The civil frame a series' rules iterate in: the zone and form of its start.
struct Frame {
    zone: TimeZone,
    /// The series' start, whose form every rule instance takes.
    form: TimeValue,
}

impl Frame {
    /// A rule's civil instance, in the form of the series' start.
    fn value_at(&self, at: DateTime) -> TimeValue {
        with_civil(&self.form, at, &self.zone)
    }

    /// `value`'s civil time in this frame.
    fn civil_of(&self, value: &TimeValue, place: &Place<'_>) -> Option<DateTime> {
        match (value, &self.form) {
            (TimeValue::Floating { date_time }, _) => Some(*date_time),
            (TimeValue::AllDay { date }, TimeValue::AllDay { .. }) => {
                Some(date.to_datetime(Time::midnight()))
            }
            (TimeValue::AllDay { date }, _) => Some(date.to_datetime(Time::MAX)),
            (
                TimeValue::Zoned {
                    date_time,
                    time_zone,
                },
                TimeValue::Zoned { time_zone: own, .. },
            ) if time_zone == own => Some(*date_time),
            _ => Some(self.zone.to_datetime(place.instant(value)?)),
        }
    }
}

/// `value`'s civil date and time in its own terms: an instant's in UTC, a day's at midnight.
fn civil(value: &TimeValue) -> DateTime {
    match value {
        TimeValue::Instant { timestamp } => TimeZone::UTC.to_datetime(*timestamp),
        TimeValue::Zoned { date_time, .. } | TimeValue::Floating { date_time } => *date_time,
        TimeValue::AllDay { date } => date.to_datetime(Time::midnight()),
    }
}

/// A value of `form`'s form at civil time `at`, read in `zone`.
fn with_civil(form: &TimeValue, at: DateTime, zone: &TimeZone) -> TimeValue {
    match form {
        TimeValue::Instant { .. } => match zone.to_ambiguous_timestamp(at).compatible() {
            Ok(timestamp) => TimeValue::instant(timestamp),
            Err(_) => TimeValue::floating(at),
        },
        TimeValue::Zoned { time_zone, .. } => TimeValue::zoned(at, time_zone.clone()),
        TimeValue::Floating { .. } => TimeValue::floating(at),
        TimeValue::AllDay { .. } => TimeValue::all_day(at.date()),
    }
}

/// Whether two values read their civil times in the same frame, so that a civil difference
/// between them means the same thing anywhere in it.
fn same_frame(a: &TimeValue, b: &TimeValue) -> bool {
    match (a, b) {
        (TimeValue::Zoned { time_zone: x, .. }, TimeValue::Zoned { time_zone: y, .. }) => x == y,
        _ => a.kind() == b.kind(),
    }
}

/// Whether an `EXDATE` or override key names `instance`.
///
/// A day names every instance on that day, and a floating time the instance at that civil time,
/// whatever the series' form, so a mistyped `EXDATE` still excludes what it was written for. An
/// instant or zoned value names the instance at the same instant: a UTC `RECURRENCE-ID` matches
/// an instance of a zoned series.
fn matches(value: &TimeValue, value_start: Option<Timestamp>, instance: &Instance<'_>) -> bool {
    match value {
        TimeValue::AllDay { date } => civil(&instance.value).date() == *date,
        TimeValue::Floating { date_time } => civil(&instance.value) == *date_time,
        TimeValue::Instant { .. } | TimeValue::Zoned { .. } => value_start == Some(instance.start),
    }
}

/// The times of an instance starting at `start` (placed at `at`), lasting as `template` does.
///
/// Where the template's start and end, and the instance, share a frame, the length is civil: a
/// 9:00 to 17:00 event stays 9:00 to 17:00 across a daylight-saving change, and an all-day event
/// keeps its number of days. Otherwise the length is exact, and the end keeps the template end's
/// form.
fn times_at(
    start: &TimeValue,
    at: Timestamp,
    template: &EventTimes,
    place: &Place<'_>,
) -> Option<EventTimes> {
    let Some(end) = template.end() else {
        return Some(EventTimes::at(start.clone()));
    };
    let first = template.start();
    let end = if same_frame(first, end) && same_frame(first, start) {
        let length = civil(end).duration_since(civil(first));
        let zone = place.zone_of(start)?;
        with_civil(start, civil(start).checked_add(length).ok()?, &zone)
    } else {
        let length = place.instant(end)?.duration_since(place.instant(first)?);
        let end_at = at.checked_add(length).ok()?;
        let zone = place.zone_of(end)?;
        with_civil(end, zone.to_datetime(end_at), &zone)
    };
    EventTimes::new(start.clone(), Some(end)).ok()
}

/// Where a `RANGE=THISANDFUTURE` change moves `instance`: by the civil shift its keyed instance
/// took where both share a frame, otherwise by the exact shift.
fn shift(
    instance: &Instance<'_>,
    future: &Future<'_>,
    place: &Place<'_>,
) -> Option<(TimeValue, Timestamp)> {
    let moved = future.event.fields.times.start();
    let value = if same_frame(future.key, moved) && same_frame(future.key, &instance.value) {
        let by = civil(moved).duration_since(civil(future.key));
        let zone = place.zone_of(&instance.value)?;
        with_civil(
            &instance.value,
            civil(&instance.value).checked_add(by).ok()?,
            &zone,
        )
    } else {
        let by = place.instant(moved)?.duration_since(future.key_start);
        let at = instance.start.checked_add(by).ok()?;
        let zone = place.zone_of(&instance.value)?;
        with_civil(&instance.value, zone.to_datetime(at), &zone)
    };
    let at = place.instant(&value)?;
    Some((value, at))
}

/// An occurrence, or nothing (with a loss) where its times cannot be placed.
fn occurrence<'s>(
    key: InstanceKey,
    times: EventTimes,
    event: &'s ProviderEvent,
    source: Source,
    place: &Place<'_>,
    losses: &mut Vec<Loss>,
) -> Option<Occurrence<'s>> {
    let Some(start) = place.instant(times.start()) else {
        losses.push(Loss::new(
            Field::Times,
            LossReason::Malformed,
            format!(
                "the occurrence at {:?} cannot be placed in time",
                times.start()
            ),
        ));
        return None;
    };
    let end = times
        .end()
        .and_then(|end| place.instant(end))
        .unwrap_or(start)
        .max(start);
    Some(Occurrence {
        key,
        times,
        start,
        end,
        event,
        source,
    })
}

/// The modified overrides of a series with no master, as they are.
fn orphans<'s>(
    series: &'s EventSeries,
    place: &Place<'_>,
    window: &Window,
    losses: &mut Vec<Loss>,
) -> Vec<Occurrence<'s>> {
    series
        .overrides
        .iter()
        .filter_map(|(key, change)| match change {
            Override::Modified { event, range } => occurrence(
                key.clone(),
                event.fields.times.clone(),
                event,
                Source::Override {
                    key: key.clone(),
                    range: *range,
                },
                place,
                losses,
            ),
            Override::Cancelled { .. } => None,
        })
        .filter(|o| window.overlaps(o.start, o.end))
        .collect()
}

/// Reads each rule for a series starting at `first`, or says why one cannot be read.
fn compile(
    rules: Option<&[RecurrenceRule]>,
    first: &TimeValue,
    frame: &Frame,
    place: &Place<'_>,
    inclusive: bool,
    losses: &mut Vec<Loss>,
) -> Result<Vec<CivilRule>, Loss> {
    let start = civil(first);
    let mut compiled = Vec::new();
    for rule in rules.unwrap_or_default() {
        let mut rule = rule;
        let mut timeless;
        if first.kind() == TimeKind::AllDay {
            if rule.frequency > Frequency::Daily {
                return Err(Loss::new(
                    Field::Recurrence,
                    LossReason::Malformed,
                    "an all-day series cannot repeat by the hour, minute or second; the series is not shown",
                ));
            }
            if !(rule.by_hour.is_empty() && rule.by_minute.is_empty() && rule.by_second.is_empty())
            {
                // RFC 5545 section 3.3.10: with a DATE start, these parts "MUST be ignored".
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Malformed,
                    "BYHOUR, BYMINUTE and BYSECOND were ignored, as RFC 5545 section 3.3.10 \
                     directs for an all-day series",
                ));
                timeless = rule.clone();
                timeless.by_hour.clear();
                timeless.by_minute.clear();
                timeless.by_second.clear();
                rule = &timeless;
            }
        }
        let until = match &rule.end {
            RuleEnd::Until { until } => Some(frame.civil_of(until, place).ok_or_else(|| {
                Loss::new(
                    Field::Recurrence,
                    LossReason::Malformed,
                    format!("UNTIL {until:?} cannot be placed in time; the series is not shown"),
                )
            })?),
            RuleEnd::Never | RuleEnd::Count { .. } => None,
        };
        let civil_rule = CivilRule::new(rule, start, until).map_err(|mut loss| {
            loss.detail.push_str("; the series is not shown");
            loss
        })?;
        compiled.push(if inclusive {
            civil_rule.counting_start()
        } else {
            civil_rule
        });
    }
    Ok(compiled)
}

/// A rule's civil instances within each span, noting a loss for each span whose budget ran out.
fn expand_rule(
    rule: &CivilRule,
    spans: &[(DateTime, DateTime)],
    losses: &mut Vec<Loss>,
) -> Vec<DateTime> {
    let mut all = Vec::new();
    for &(low, high) in spans {
        let mut instances = rule.instances(Some(low), high);
        let found: Vec<DateTime> = instances
            .by_ref()
            .filter(|&at| at >= low && at <= high)
            .collect();
        if instances.exhausted() {
            losses.push(Loss::new(
                Field::Recurrence,
                LossReason::Unsupported,
                format!(
                    "expansion stopped after {PERIOD_BUDGET} periods; instances after {} were not expanded",
                    found.last().copied().unwrap_or(rule.start())
                ),
            ));
        }
        all.extend(found);
    }
    all
}
