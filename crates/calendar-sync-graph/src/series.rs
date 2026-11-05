//! Grouping Graph's events into series.
//!
//! Ground truth: `references/graph/outlook-schedule-recurring-events.md`. A series master holds
//! the pattern; each instance is its own event, with `seriesMasterId` and `originalStart`, of
//! `type` `occurrence` (as the pattern made it) or `exception` (changed).

use std::collections::BTreeMap;

use calendar_sync_model::{
    EventSeries, EventStatus, InstanceKey, InstanceRange, Override, Provider, ProviderEvent,
    TimeValue, TimeZoneId,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;

use crate::zones;

/// A passthrough value that is a JSON string.
fn kept(event: &ProviderEvent, name: &str) -> Option<String> {
    let raw = event.passthrough.get(name).next()?;
    match serde_json::from_str(raw).ok()? {
        Value::String(text) => Some(text),
        _ => None,
    }
}

/// The instance a Graph instance stands for: its `originalStart`, which the mapping keeps in the
/// record's passthrough, in the form of the instance's own start. A zoned start keys by the
/// original start in that zone, which is how the master's `DTSTART` counts instances; a whole
/// day keys by the day in the event's original zone. `None` for a record that is not a Graph
/// instance.
#[must_use]
pub fn instance_key(event: &ProviderEvent) -> Option<InstanceKey> {
    if event.ids.provider != Provider::Graph || event.ids.series_id.is_none() {
        return None;
    }
    let original: Timestamp = kept(event, "originalStart")?.parse().ok()?;
    let key = match event.fields.times.start() {
        TimeValue::Zoned { time_zone, .. } => match zones::iana(time_zone.as_str()) {
            Some(zone) => TimeValue::zoned(
                original.to_zoned(zone).datetime(),
                TimeZoneId::new(time_zone.as_str()),
            ),
            None => TimeValue::instant(original),
        },
        TimeValue::AllDay { .. } => {
            let zone = kept(event, "originalStartTimeZone")
                .and_then(|name| zones::resolve(&name))
                .map_or(TimeZone::UTC, |(_, zone)| zone);
            TimeValue::all_day(original.to_zoned(zone).date())
        }
        TimeValue::Instant { .. } | TimeValue::Floating { .. } => TimeValue::instant(original),
    };
    Some(InstanceKey::new(key))
}

/// Groups Graph events into series: each master with the instances whose `seriesMasterId` names
/// it, keyed by [`instance_key`].
///
/// An `exception` becomes [`Override::Modified`], and a cancelled instance
/// [`Override::Cancelled`] with its own ids and revision. An unchanged `occurrence` is what the
/// master's pattern already says, so it is dropped when its master is among `events`; without its
/// master (a delta window holds instances, not always their master) it is kept as an override, so
/// the instance is not lost. An instance with no readable `originalStart` is kept as a single
/// event.
///
/// Series come out in the order each first appears.
#[must_use]
pub fn group(events: impl IntoIterator<Item = ProviderEvent>) -> Vec<EventSeries> {
    let events: Vec<ProviderEvent> = events.into_iter().collect();
    let masters: Vec<String> = events
        .iter()
        .filter(|event| event.ids.series_id.is_none())
        .map(|event| event.ids.id.clone())
        .collect();
    let mut order: Vec<String> = Vec::new();
    let mut series: BTreeMap<String, EventSeries> = BTreeMap::new();
    for event in events {
        let key = instance_key(&event);
        let id = match (&event.ids.series_id, &key) {
            (Some(series_id), Some(_)) => series_id.clone(),
            _ => event.ids.id.clone(),
        };
        let entry = series.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            EventSeries {
                master: None,
                overrides: BTreeMap::new(),
            }
        });
        let Some(key) = key else {
            entry.master = Some(event);
            continue;
        };
        let occurrence = kept(&event, "type").as_deref() == Some("occurrence");
        if occurrence && masters.contains(&id) {
            continue;
        }
        let value = if event.fields.status == EventStatus::Cancelled {
            Override::Cancelled {
                ids: Some(event.ids),
                revision: event.revision,
            }
        } else {
            Override::Modified {
                event: Box::new(event),
                range: InstanceRange::ThisInstance,
            }
        };
        entry.overrides.insert(key, value);
    }
    order
        .into_iter()
        .filter_map(|id| series.remove(&id))
        .collect()
}
