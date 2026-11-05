//! Graph's `dateTimeTimeZone` (`dateTime`, `timeZone`) to and from [`TimeValue`].

use calendar_sync_model::{Field, Loss, LossReason, TimeValue, TimeZoneId};
use jiff::ToSpan;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::zones;

/// Reads one `dateTimeTimeZone`.
///
/// `dateTime` is a civil time in `timeZone`, which the seam asks to be `UTC`. The event's own zone
/// is `original_zone` (`originalStartTimeZone` or `originalEndTimeZone`), so the time is converted
/// into it. A time whose zone is UTC reads as an instant. A zone neither the bundled database nor
/// the CLDR table knows, such as `tzone://Microsoft/Custom`, is reported, and the time is read in
/// the zone it was given in.
///
/// `all_day` reads the civil date of `dateTime`, which Graph sets to midnight.
pub(crate) fn read(
    value: &Value,
    original_zone: Option<&str>,
    all_day: bool,
    what: &str,
    losses: &mut Vec<Loss>,
) -> Result<TimeValue, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))?;
    let text = object
        .get("dateTime")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what} has no dateTime"))?;
    let civil: DateTime = text
        .parse()
        .map_err(|error| format!("{what}.dateTime {text:?}: {error}"))?;
    if all_day {
        return Ok(TimeValue::all_day(civil.date()));
    }
    let given = object
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let (given_name, given_zone) = zones::resolve(given).unwrap_or_else(|| {
        losses.push(Loss::new(
            Field::Times,
            LossReason::Unsupported,
            format!("{what}.timeZone {given:?} is not a known zone; read in UTC"),
        ));
        ("UTC".to_owned(), TimeZone::UTC)
    });
    let instant = civil
        .to_zoned(given_zone.clone())
        .map_err(|error| format!("{what}.dateTime {text:?} in {given_name}: {error}"))?
        .timestamp();
    let (name, zone) = match original_zone {
        Some(original) => zones::resolve(original).unwrap_or_else(|| {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Unsupported,
                format!("the event's zone {original:?} is not a known zone; read in {given_name}"),
            ));
            (given_name, given_zone)
        }),
        None => (given_name, given_zone),
    };
    Ok(if zones::is_utc(&name) {
        TimeValue::instant(instant)
    } else {
        TimeValue::zoned(instant.to_zoned(zone).datetime(), TimeZoneId::new(name))
    })
}

/// Writes one `dateTimeTimeZone`. A zone is written under its Windows name where CLDR lists one.
/// An instant and a whole day are written in UTC, and a floating time, which Graph cannot hold,
/// as that civil time in UTC, reported.
pub(crate) fn write(time: &TimeValue, losses: &mut Vec<Loss>) -> Value {
    match time {
        TimeValue::AllDay { date } => json!({
            "dateTime": date.to_datetime(jiff::civil::Time::midnight()).to_string(),
            "timeZone": "UTC",
        }),
        TimeValue::Instant { timestamp } => json!({
            "dateTime": timestamp.to_zoned(TimeZone::UTC).datetime().to_string(),
            "timeZone": "UTC",
        }),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => json!({
            "dateTime": date_time.to_string(),
            "timeZone": zones::graph_name(time_zone.as_str()),
        }),
        TimeValue::Floating { date_time } => {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Approximated,
                "Graph has no floating time; written in UTC",
            ));
            json!({ "dateTime": date_time.to_string(), "timeZone": "UTC" })
        }
    }
}

/// The end Graph needs for an event the model gives no end: the start itself for a timed event,
/// and the next day for an all-day one.
pub(crate) fn implied_end(start: &TimeValue, losses: &mut Vec<Loss>) -> TimeValue {
    match start {
        TimeValue::AllDay { date } => {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Approximated,
                "Graph needs an end; an all-day event with none is written lasting one day",
            ));
            TimeValue::all_day(date.checked_add(1.day()).unwrap_or(*date))
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::{TimeValue, TimeZoneId};
    use jiff::civil::date;
    use serde_json::json;

    use super::{read, write};

    #[test]
    fn utc_time_is_converted_into_the_event_zone() {
        let mut losses = Vec::new();
        let value = json!({ "dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC" });
        let time = read(
            &value,
            Some("Eastern Standard Time"),
            false,
            "start",
            &mut losses,
        )
        .unwrap();
        let toronto = TimeValue::zoned(
            date(2026, 11, 10).at(10, 0, 0, 0),
            TimeZoneId::new("America/New_York"),
        );
        assert_eq!(time, toronto);
        assert!(losses.is_empty());
        assert_eq!(
            write(&time, &mut losses),
            json!({ "dateTime": "2026-11-10T10:00:00", "timeZone": "Eastern Standard Time" })
        );
        // An IANA name is kept as it is.
        let time = read(&value, Some("America/Toronto"), false, "start", &mut losses).unwrap();
        assert_eq!(
            time,
            TimeValue::zoned(
                date(2026, 11, 10).at(10, 0, 0, 0),
                TimeZoneId::new("America/Toronto")
            )
        );
    }

    #[test]
    fn utc_is_an_instant() {
        let mut losses = Vec::new();
        let value = json!({ "dateTime": "2026-11-10T15:00:00", "timeZone": "UTC" });
        let time = read(&value, Some("UTC"), false, "start", &mut losses).unwrap();
        assert_eq!(
            time,
            TimeValue::instant("2026-11-10T15:00:00Z".parse().unwrap())
        );
        assert_eq!(
            write(&time, &mut losses),
            json!({ "dateTime": "2026-11-10T15:00:00", "timeZone": "UTC" })
        );
        assert!(losses.is_empty());
    }

    #[test]
    fn an_unknown_zone_is_reported() {
        let mut losses = Vec::new();
        let value = json!({ "dateTime": "2026-11-10T15:00:00", "timeZone": "UTC" });
        let time = read(
            &value,
            Some("tzone://Microsoft/Custom"),
            false,
            "start",
            &mut losses,
        )
        .unwrap();
        assert!(matches!(time, TimeValue::Instant { .. }));
        assert_eq!(losses.len(), 1);
        let value = json!({ "dateTime": "2026-11-10T15:00:00", "timeZone": "Mars" });
        read(&value, None, false, "start", &mut losses).unwrap();
        assert_eq!(losses.len(), 2);
        assert!(
            read(
                &json!({ "timeZone": "UTC" }),
                None,
                false,
                "start",
                &mut losses
            )
            .is_err()
        );
    }

    #[test]
    fn all_day_and_floating() {
        let mut losses = Vec::new();
        let value = json!({ "dateTime": "2026-11-06T00:00:00.0000000", "timeZone": "UTC" });
        let day = read(
            &value,
            Some("Pacific Standard Time"),
            true,
            "start",
            &mut losses,
        )
        .unwrap();
        assert_eq!(day, TimeValue::all_day(date(2026, 11, 6)));
        assert_eq!(
            write(&day, &mut losses),
            json!({ "dateTime": "2026-11-06T00:00:00", "timeZone": "UTC" })
        );
        assert!(losses.is_empty());
        let floating = TimeValue::floating(date(2026, 11, 6).at(9, 0, 0, 0));
        assert_eq!(
            write(&floating, &mut losses),
            json!({ "dateTime": "2026-11-06T09:00:00", "timeZone": "UTC" })
        );
        assert_eq!(losses.len(), 1);
    }
}
