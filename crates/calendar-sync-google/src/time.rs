//! Google's `EventDateTime` (`date`, `dateTime`, `timeZone`) to and from [`TimeValue`].

use calendar_sync_model::{Field, Loss, LossReason, TimeValue, TimeZoneId};
use jiff::civil::{Date, DateTime};
use jiff::tz::{TimeZone, TimeZoneDatabase};
use jiff::{SignedDuration, Timestamp, ToSpan};
use serde_json::{Map, Value, json};

/// The IANA zone `name`, from the bundled database only. jiff's global database may read the
/// filesystem, which this crate never does.
pub(crate) fn zone(name: &str) -> Option<TimeZone> {
    TimeZoneDatabase::bundled().get(name).ok()
}

/// Reads one `EventDateTime`.
///
/// `date` is all-day. A `dateTime` with a `timeZone` is a civil time in that zone: Google writes
/// the instant with the offset of the calendar's zone, not necessarily the event's, so the
/// instant is converted into the event's zone. A `dateTime` without a `timeZone` is an instant.
/// A zone the bundled database does not know keeps the instant and reports a loss.
pub(crate) fn read(value: &Value, what: &str, losses: &mut Vec<Loss>) -> Result<TimeValue, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))?;
    let time_zone = object.get("timeZone").and_then(Value::as_str);
    if let Some(date) = object.get("date").and_then(Value::as_str) {
        let date: Date = date
            .parse()
            .map_err(|error| format!("{what}.date {date:?}: {error}"))?;
        return Ok(TimeValue::all_day(date));
    }
    let Some(text) = object.get("dateTime").and_then(Value::as_str) else {
        return Err(format!("{what} has neither date nor dateTime"));
    };
    if let Ok(timestamp) = text.parse::<Timestamp>() {
        let Some(name) = time_zone else {
            return Ok(TimeValue::instant(timestamp));
        };
        return Ok(if let Some(tz) = zone(name) {
            TimeValue::zoned(timestamp.to_zoned(tz).datetime(), TimeZoneId::new(name))
        } else {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Unsupported,
                format!("{what}.timeZone {name:?} is not an IANA zone; read as an instant"),
            ));
            TimeValue::instant(timestamp)
        });
    }
    let civil: DateTime = text
        .parse()
        .map_err(|error| format!("{what}.dateTime {text:?}: {error}"))?;
    match time_zone {
        Some(name) => Ok(TimeValue::zoned(civil, TimeZoneId::new(name))),
        None => Err(format!(
            "{what}.dateTime {text:?} has no offset and no timeZone"
        )),
    }
}

/// Writes one `EventDateTime`. A floating time has no Google form: it is written as that civil
/// time in UTC, and reported.
pub(crate) fn write(time: &TimeValue, what: Field, losses: &mut Vec<Loss>) -> Value {
    match time {
        TimeValue::AllDay { date } => json!({ "date": date.to_string() }),
        TimeValue::Instant { timestamp } => json!({ "dateTime": timestamp.to_string() }),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => {
            // Google takes a civil `dateTime` when `timeZone` names the zone.
            let mut object = Map::new();
            object.insert("dateTime".to_owned(), Value::String(date_time.to_string()));
            object.insert(
                "timeZone".to_owned(),
                Value::String(time_zone.as_str().to_owned()),
            );
            Value::Object(object)
        }
        TimeValue::Floating { date_time } => {
            losses.push(Loss::new(
                what,
                LossReason::Approximated,
                "Google has no floating time; written in UTC",
            ));
            json!({ "dateTime": date_time.to_string(), "timeZone": "UTC" })
        }
    }
}

/// The end Google needs for an event the model gives no end: the start itself for a timed event,
/// and the next day for an all-day one.
pub(crate) fn implied_end(start: &TimeValue, losses: &mut Vec<Loss>) -> TimeValue {
    match start {
        TimeValue::AllDay { date } => {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Approximated,
                "Google needs an end; an all-day event with none is written lasting one day",
            ));
            TimeValue::all_day(date.checked_add(1.day()).unwrap_or(*date))
        }
        other => other.clone(),
    }
}

/// The earliest and latest instants `time` can stand for. A whole day or a floating time spans
/// every zone, so it is widened by the widest offsets in use, `-12:00` and `+14:00`. `None` when
/// the zone is unknown.
pub(crate) fn bounds(time: &TimeValue) -> Option<(Timestamp, Timestamp)> {
    let widen = |civil: DateTime| -> Option<(Timestamp, Timestamp)> {
        let utc = civil.to_zoned(TimeZone::UTC).ok()?.timestamp();
        Some((
            utc.checked_sub(SignedDuration::from_hours(14)).ok()?,
            utc.checked_add(SignedDuration::from_hours(12)).ok()?,
        ))
    };
    match time {
        TimeValue::Instant { timestamp } => Some((*timestamp, *timestamp)),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => {
            let at = date_time
                .to_zoned(zone(time_zone.as_str())?)
                .ok()?
                .timestamp();
            Some((at, at))
        }
        TimeValue::Floating { date_time } => widen(*date_time),
        TimeValue::AllDay { date } => widen(date.to_datetime(jiff::civil::Time::midnight())),
    }
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::{Field, TimeValue, TimeZoneId};
    use jiff::civil::date;
    use serde_json::json;

    use super::{read, write};

    #[test]
    fn date_time_is_converted_into_the_event_zone() {
        let mut losses = Vec::new();
        // The calendar is in UTC, the event in Toronto.
        let value = json!({ "dateTime": "2026-11-05T15:00:00Z", "timeZone": "America/Toronto" });
        let time = read(&value, "start", &mut losses).unwrap();
        assert_eq!(
            time,
            TimeValue::zoned(
                date(2026, 11, 5).at(10, 0, 0, 0),
                TimeZoneId::new("America/Toronto")
            )
        );
        assert!(losses.is_empty());
        assert_eq!(
            write(&time, Field::Times, &mut losses),
            json!({ "dateTime": "2026-11-05T10:00:00", "timeZone": "America/Toronto" })
        );
    }

    #[test]
    fn offset_without_zone_is_an_instant() {
        let mut losses = Vec::new();
        let time = read(
            &json!({ "dateTime": "2026-11-05T10:00:00-05:00" }),
            "start",
            &mut losses,
        )
        .unwrap();
        assert_eq!(
            time,
            TimeValue::instant("2026-11-05T15:00:00Z".parse().unwrap())
        );
        assert_eq!(
            write(&time, Field::Times, &mut losses),
            json!({ "dateTime": "2026-11-05T15:00:00Z" })
        );
    }

    #[test]
    fn unknown_zone_keeps_the_instant_and_reports() {
        let mut losses = Vec::new();
        let time = read(
            &json!({ "dateTime": "2026-11-05T10:00:00-05:00", "timeZone": "Mars/Olympus" }),
            "start",
            &mut losses,
        )
        .unwrap();
        assert!(matches!(time, TimeValue::Instant { .. }));
        assert_eq!(losses.len(), 1);
    }

    #[test]
    fn civil_date_time_needs_a_zone() {
        let mut losses = Vec::new();
        assert!(
            read(
                &json!({ "dateTime": "2026-11-05T10:00:00" }),
                "start",
                &mut losses
            )
            .is_err()
        );
        assert_eq!(
            read(
                &json!({ "dateTime": "2026-11-05T10:00:00", "timeZone": "Europe/Berlin" }),
                "start",
                &mut losses
            )
            .unwrap(),
            TimeValue::zoned(
                date(2026, 11, 5).at(10, 0, 0, 0),
                TimeZoneId::new("Europe/Berlin")
            )
        );
    }

    #[test]
    fn all_day_and_floating() {
        let mut losses = Vec::new();
        let day = read(&json!({ "date": "2026-11-06" }), "start", &mut losses).unwrap();
        assert_eq!(day, TimeValue::all_day(date(2026, 11, 6)));
        assert_eq!(
            write(&day, Field::Times, &mut losses),
            json!({ "date": "2026-11-06" })
        );
        assert!(losses.is_empty());
        let floating = TimeValue::floating(date(2026, 11, 6).at(9, 0, 0, 0));
        assert_eq!(
            write(&floating, Field::Times, &mut losses),
            json!({ "dateTime": "2026-11-06T09:00:00", "timeZone": "UTC" })
        );
        assert_eq!(losses.len(), 1);
    }
}
