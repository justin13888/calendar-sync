//! Google's `recurrence`, a list of iCalendar `RRULE`, `EXRULE`, `RDATE` and `EXDATE` lines, to
//! and from [`Recurrence`].
//!
//! The lines are iCalendar, so `calendar-sync-ical` reads and writes them: they are placed in a
//! one-event calendar beside a `DTSTART` built from the event's start, which gives a date-only or
//! zoned `EXDATE` the context RFC 5545 reads it in.

use calendar_sync_ical::IcalDocument;
use calendar_sync_model::{
    EventSeries, EventTimes, Field, Loss, LossReason, NormalizedEvent, Provider, ProviderEvent,
    ProviderIds, Recurrence, TimeValue,
};
use jiff::Timestamp;

/// The names of the properties a `recurrence` entry may hold.
const NAMES: [&str; 4] = ["RRULE", "EXRULE", "RDATE", "EXDATE"];

/// The `UID` of the scratch event the lines are read and written in.
const UID: &str = "recurrence@calendar-sync-google";

/// The `DTSTART` line for `start`.
fn dtstart(start: &TimeValue) -> String {
    match start {
        TimeValue::AllDay { date } => format!("DTSTART;VALUE=DATE:{}", date.strftime("%Y%m%d")),
        TimeValue::Instant { timestamp } => {
            format!("DTSTART:{}", timestamp.strftime("%Y%m%dT%H%M%SZ"))
        }
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => format!(
            "DTSTART;TZID={time_zone}:{}",
            date_time.strftime("%Y%m%dT%H%M%S")
        ),
        TimeValue::Floating { date_time } => {
            format!("DTSTART:{}", date_time.strftime("%Y%m%dT%H%M%S"))
        }
    }
}

/// Reads `lines` for a series starting at `start`. Entries that are not recurrence properties,
/// and anything the iCalendar reader could not carry, are reported.
pub(crate) fn read(
    lines: &[String],
    start: &TimeValue,
    losses: &mut Vec<Loss>,
) -> Option<Recurrence> {
    let mut text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//calendar-sync//google//EN\r\nBEGIN:VEVENT\r\nUID:{UID}\r\n{}\r\n",
        dtstart(start)
    );
    for line in lines {
        let name = property_name(line);
        if NAMES.contains(&name.as_str()) && !line.contains(['\r', '\n']) {
            text.push_str(line);
            text.push_str("\r\n");
        } else {
            losses.push(Loss::new(
                Field::Recurrence,
                LossReason::Unsupported,
                format!("recurrence entry {line:?} is not RRULE, EXRULE, RDATE or EXDATE"),
            ));
        }
    }
    text.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    let read = calendar_sync_ical::read_feed(text.as_bytes());
    let mut series = read.series.into_iter();
    let Some(found) = series.next() else {
        losses.push(Loss::new(
            Field::Recurrence,
            LossReason::Malformed,
            "the recurrence could not be read",
        ));
        return None;
    };
    losses.extend(
        found
            .losses
            .into_iter()
            .chain(read.losses)
            .filter(|loss| loss.field == Field::Recurrence),
    );
    found
        .series
        .master
        .and_then(|master| master.fields.recurrence)
}

/// Writes `recurrence` for a series starting at `start`, as the lines Google takes.
pub(crate) fn write(
    recurrence: &Recurrence,
    start: &TimeValue,
    losses: &mut Vec<Loss>,
) -> Vec<String> {
    let mut ids = ProviderIds::new(Provider::Ics, UID);
    ids.ical_uid = Some(UID.to_owned());
    let mut fields = NormalizedEvent::new(EventTimes::at(start.clone()));
    fields.recurrence = Some(recurrence.clone());
    let series = EventSeries::single(ProviderEvent {
        ids,
        revision: None,
        mapper_version: calendar_sync_ical::MAPPER_VERSION,
        fields,
        passthrough: calendar_sync_model::Passthrough::new(),
    });
    let mut document = IcalDocument::new("-//calendar-sync//google//EN");
    match document.write(&series, Timestamp::UNIX_EPOCH) {
        Ok(written) => losses.extend(
            written
                .into_iter()
                .filter(|loss| loss.field == Field::Recurrence),
        ),
        Err(error) => {
            losses.push(Loss::new(
                Field::Recurrence,
                LossReason::Unrepresentable,
                error.to_string(),
            ));
            return Vec::new();
        }
    }
    let bytes = document.to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    let unfolded = text.replace("\r\n ", "").replace("\r\n\t", "");
    // Only the event's own properties: a `VTIMEZONE` written beside it has `RRULE`s too.
    let mut path: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    for line in unfolded.split("\r\n") {
        let name = property_name(line);
        let value = line.split_once(':').map_or("", |(_, value)| value);
        match name.as_str() {
            "BEGIN" => path.push(value.to_ascii_uppercase()),
            "END" => {
                path.pop();
            }
            _ if path.last().map(String::as_str) == Some("VEVENT")
                && NAMES.contains(&name.as_str()) =>
            {
                lines.push(line.to_owned());
            }
            _ => {}
        }
    }
    lines
}

/// A content line's property name, upper-cased.
fn property_name(line: &str) -> String {
    line.split([';', ':'])
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::{Frequency, NDay, TimeValue, TimeZoneId, Weekday};
    use jiff::civil::date;

    use super::{read, write};

    #[test]
    fn reads_and_writes_a_weekly_rule_with_an_exception_date() {
        let start = TimeValue::zoned(
            date(2026, 11, 3).at(10, 0, 0, 0),
            TimeZoneId::new("America/Toronto"),
        );
        let lines = vec![
            "RRULE:FREQ=WEEKLY;BYDAY=TU,TH;COUNT=10".to_owned(),
            "EXDATE;TZID=America/Toronto:20261105T100000".to_owned(),
        ];
        let mut losses = Vec::new();
        let recurrence = read(&lines, &start, &mut losses).expect("recurrence");
        assert!(losses.is_empty(), "{losses:?}");
        assert_eq!(recurrence.rules.len(), 1);
        assert_eq!(recurrence.rules[0].frequency, Frequency::Weekly);
        assert_eq!(
            recurrence.rules[0].by_day,
            vec![
                NDay::every(Weekday::Tuesday),
                NDay::every(Weekday::Thursday)
            ]
        );
        assert_eq!(
            recurrence.excluded_dates,
            vec![TimeValue::zoned(
                date(2026, 11, 5).at(10, 0, 0, 0),
                TimeZoneId::new("America/Toronto")
            )]
        );

        let written = write(&recurrence, &start, &mut losses);
        assert!(losses.is_empty(), "{losses:?}");
        let mut again = Vec::new();
        assert_eq!(read(&written, &start, &mut again), Some(recurrence));
        assert!(written.iter().any(|line| line.starts_with("RRULE:")));
        assert!(written.iter().any(|line| line.starts_with("EXDATE")));
    }

    #[test]
    fn a_line_that_is_not_a_recurrence_property_is_reported() {
        let start = TimeValue::all_day(date(2026, 11, 3));
        let mut losses = Vec::new();
        let recurrence = read(
            &["RRULE:FREQ=DAILY".to_owned(), "SUMMARY:hi".to_owned()],
            &start,
            &mut losses,
        );
        assert!(recurrence.is_some());
        assert_eq!(losses.len(), 1);
    }
}
