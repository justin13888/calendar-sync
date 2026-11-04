//! Grouping Google's resources into series, and the local window filter.

use std::collections::BTreeMap;

use calendar_sync_model::{
    EventSeries, EventStatus, InstanceKey, InstanceRange, Override, Provider, ProviderEvent,
};
use jiff::Timestamp;
use serde_json::Value;

use crate::time;

/// The instance a Google override replaces: its `originalStartTime`, which the mapping keeps in
/// the record's passthrough. `None` for a record that is not a Google instance.
#[must_use]
pub fn instance_key(event: &ProviderEvent) -> Option<InstanceKey> {
    if event.ids.provider != Provider::Google || event.ids.series_id.is_none() {
        return None;
    }
    let raw = event.passthrough.get("originalStartTime").next()?;
    let value: Value = serde_json::from_str(raw).ok()?;
    time::read(&value, "originalStartTime", &mut Vec::new())
        .ok()
        .map(InstanceKey::new)
}

/// Groups Google resources into series: each master with the instances whose
/// `recurringEventId` names it, keyed by [`instance_key`]. A cancelled instance becomes
/// [`Override::Cancelled`] with its own ids and revision; any other instance,
/// [`Override::Modified`]. An instance whose master is not among `events` forms a series with no
/// master, as an invitation to one instance does. An instance with no readable
/// `originalStartTime` is kept as a single event.
///
/// Series come out in the order each first appears.
#[must_use]
pub fn group(events: impl IntoIterator<Item = ProviderEvent>) -> Vec<EventSeries> {
    let mut order: Vec<String> = Vec::new();
    let mut series: BTreeMap<String, EventSeries> = BTreeMap::new();
    let mut slot = |id: &str, order: &mut Vec<String>| -> String {
        if !series.contains_key(id) {
            order.push(id.to_owned());
            series.insert(
                id.to_owned(),
                EventSeries {
                    master: None,
                    overrides: BTreeMap::new(),
                },
            );
        }
        id.to_owned()
    };
    let mut placed: Vec<(String, Option<InstanceKey>, ProviderEvent)> = Vec::new();
    for event in events {
        let key = instance_key(&event);
        let id = match (&event.ids.series_id, &key) {
            (Some(series_id), Some(_)) => series_id.clone(),
            _ => event.ids.id.clone(),
        };
        let id = slot(&id, &mut order);
        placed.push((id, key, event));
    }
    for (id, key, event) in placed {
        let Some(entry) = series.get_mut(&id) else {
            continue;
        };
        match key {
            None => entry.master = Some(event),
            Some(key) => {
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
        }
    }
    order
        .into_iter()
        .filter_map(|id| series.remove(&id))
        .collect()
}

/// A span of time a sync keeps events for, applied on this side.
///
/// Google refuses `timeMin` and `timeMax` beside a `syncToken`, so an incremental list reports
/// changes in the whole calendar, and the application filters them with [`Window::contains`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// The start, inclusive.
    pub start: Timestamp,
    /// The end, exclusive.
    pub end: Timestamp,
}

impl Window {
    /// Whether `event` may fall in the window. A series master always may, since an instance of
    /// it can. Where the event's span cannot be decided (a zone the bundled database does not
    /// know), it may. A whole day or a floating time is taken as wide as every zone makes it.
    #[must_use]
    pub fn contains(&self, event: &ProviderEvent) -> bool {
        if event.fields.recurrence.is_some() {
            return true;
        }
        let times = &event.fields.times;
        let Some((earliest, _)) = time::bounds(times.start()) else {
            return true;
        };
        let Some((_, latest)) = times
            .end()
            .map_or(time::bounds(times.start()), time::bounds)
        else {
            return true;
        };
        earliest < self.end && (latest > self.start || (latest == earliest && latest >= self.start))
    }
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::{
        EventStatus, EventTimes, MapperVersion, NormalizedEvent, Passthrough, Provider,
        ProviderEvent, ProviderIds, ProviderRevision, TimeValue,
    };
    use jiff::civil::date;

    use super::Window;

    fn event(start: TimeValue, end: TimeValue) -> ProviderEvent {
        ProviderEvent {
            ids: ProviderIds::new(Provider::Google, "e"),
            revision: Some(ProviderRevision::new("\"1\"")),
            mapper_version: MapperVersion::new(1),
            fields: NormalizedEvent::new(EventTimes::new(start, Some(end)).unwrap()),
            passthrough: Passthrough::new(),
        }
    }

    #[test]
    fn window_keeps_overlapping_events_and_drops_the_rest() {
        let window = Window {
            start: "2026-11-01T00:00:00Z".parse().unwrap(),
            end: "2026-12-01T00:00:00Z".parse().unwrap(),
        };
        let at = |text: &str| TimeValue::instant(text.parse().unwrap());
        assert!(window.contains(&event(
            at("2026-11-05T10:00:00Z"),
            at("2026-11-05T11:00:00Z")
        )));
        assert!(!window.contains(&event(
            at("2026-10-05T10:00:00Z"),
            at("2026-10-05T11:00:00Z")
        )));
        assert!(window.contains(&event(
            at("2026-10-31T23:00:00Z"),
            at("2026-11-01T01:00:00Z")
        )));
        assert!(!window.contains(&event(
            at("2026-12-01T00:00:00Z"),
            at("2026-12-01T01:00:00Z")
        )));
        // A whole day on the boundary may fall inside in some zone.
        assert!(window.contains(&event(
            TimeValue::all_day(date(2026, 12, 1)),
            TimeValue::all_day(date(2026, 12, 2))
        )));
        assert!(!window.contains(&event(
            TimeValue::all_day(date(2026, 12, 3)),
            TimeValue::all_day(date(2026, 12, 4))
        )));
        let mut cancelled = event(at("2026-10-05T10:00:00Z"), at("2026-10-05T11:00:00Z"));
        cancelled.fields.status = EventStatus::Cancelled;
        assert!(!window.contains(&cancelled));
    }
}
