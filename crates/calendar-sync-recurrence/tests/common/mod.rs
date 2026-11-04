//! Builders shared by the integration tests.

#![allow(dead_code, unreachable_pub)]

use std::collections::BTreeMap;

use calendar_sync_model::{
    EventSeries, EventTimes, InstanceKey, InstanceRange, MapperVersion, NormalizedEvent, Override,
    Passthrough, Provider, ProviderEvent, ProviderIds, Recurrence, TimeValue, TimeZoneId,
};
use calendar_sync_recurrence::{Expander, Expansion, Window};
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{TimeZone, TimeZoneDatabase};

/// The bundled time zone database.
pub fn zones() -> TimeZoneDatabase {
    TimeZoneDatabase::bundled()
}

pub fn tz(name: &str) -> TimeZone {
    zones().get(name).unwrap()
}

pub fn ts(text: &str) -> Timestamp {
    text.parse().unwrap()
}

pub fn dt(text: &str) -> DateTime {
    text.parse().unwrap()
}

pub fn zoned(text: &str, zone: &str) -> TimeValue {
    TimeValue::zoned(dt(text), TimeZoneId::new(zone))
}

pub fn floating(text: &str) -> TimeValue {
    TimeValue::floating(dt(text))
}

pub fn instant(text: &str) -> TimeValue {
    TimeValue::instant(ts(text))
}

pub fn day(text: &str) -> TimeValue {
    TimeValue::all_day(text.parse().unwrap())
}

pub fn window(start: &str, end: &str) -> Window {
    Window::new(ts(start), ts(end)).unwrap()
}

pub fn event(id: &str, start: TimeValue, end: Option<TimeValue>) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(Provider::CalDav, id),
        revision: None,
        mapper_version: MapperVersion::new(1),
        fields: NormalizedEvent::new(EventTimes::new(start, end).unwrap()),
        passthrough: Passthrough::new(),
    }
}

/// A series master starting at `start`, ending at `end`, recurring by `recurrence`.
pub fn series(start: TimeValue, end: Option<TimeValue>, recurrence: Recurrence) -> EventSeries {
    let mut master = event("master", start, end);
    master.fields.recurrence = Some(recurrence);
    EventSeries::single(master)
}

pub fn modified(
    series: &mut EventSeries,
    key: TimeValue,
    start: TimeValue,
    end: Option<TimeValue>,
    range: InstanceRange,
) {
    series.overrides.insert(
        InstanceKey::new(key),
        Override::Modified {
            event: Box::new(event("override", start, end)),
            range,
        },
    );
}

pub fn cancelled(series: &mut EventSeries, key: TimeValue) {
    series.overrides.insert(
        InstanceKey::new(key),
        Override::Cancelled {
            ids: None,
            revision: None,
        },
    );
}

pub fn expand<'s>(series: &'s EventSeries, floating_zone: &str, window: &Window) -> Expansion<'s> {
    let zones = zones();
    Expander::new(&zones, tz(floating_zone)).expand(series, window)
}

/// The occurrences' starts as instants.
pub fn starts(expansion: &Expansion<'_>) -> Vec<Timestamp> {
    expansion.occurrences.iter().map(|o| o.start).collect()
}

/// The occurrences' start values.
pub fn start_values(expansion: &Expansion<'_>) -> Vec<TimeValue> {
    expansion
        .occurrences
        .iter()
        .map(|o| o.times.start().clone())
        .collect()
}

pub fn empty_overrides() -> BTreeMap<InstanceKey, Override> {
    BTreeMap::new()
}

/// Every timestamp shown in UTC, for readable failures.
pub fn utc(expansion: &Expansion<'_>) -> Vec<DateTime> {
    expansion
        .occurrences
        .iter()
        .map(|o| TimeZone::UTC.to_datetime(o.start))
        .collect()
}
