//! Value grammars: `DATE`, `DATE-TIME`, `DURATION` and `RECUR` (RFC 5545 sections 3.3.4, 3.3.5,
//! 3.3.6 and 3.3.10, RFC 7529), read into the model and written back.

use core::num::NonZeroU32;

use calendar_sync_model::{
    Field, Frequency, Loss, LossReason, NDay, RecurrenceRule, RuleEnd, RuleMonth, Skip, TimeValue,
    Weekday,
};
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};

/// A `DATE` value, `YYYYMMDD`.
pub(crate) fn parse_date(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    if bytes.len() != 8 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let year = text[0..4].parse().ok()?;
    let month = text[4..6].parse().ok()?;
    let day = text[6..8].parse().ok()?;
    Date::new(year, month, day).ok()
}

/// A `DATE-TIME` value, `YYYYMMDDTHHMMSS` with an optional `Z`: the civil time, and whether it
/// is UTC.
pub(crate) fn parse_date_time(text: &str) -> Option<(DateTime, bool)> {
    let (text, utc) = match text.strip_suffix(['Z', 'z']) {
        Some(text) => (text, true),
        None => (text, false),
    };
    let (date, time) = text.split_once(['T', 't'])?;
    let date = parse_date(date)?;
    let bytes = time.as_bytes();
    if bytes.len() != 6 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let hour = time[0..2].parse().ok()?;
    let minute = time[2..4].parse().ok()?;
    // A leap second (60) is read as the last second of its minute.
    let second: i8 = time[4..6].parse().ok()?;
    let time = Time::new(hour, minute, second.min(59), 0).ok()?;
    Some((date.to_datetime(time), utc))
}

/// A `DATE` value as written.
pub(crate) fn format_date(date: Date) -> String {
    format!("{:04}{:02}{:02}", date.year(), date.month(), date.day())
}

/// A floating `DATE-TIME` value as written, its fraction of a second dropped.
pub(crate) fn format_date_time(date_time: DateTime) -> String {
    format!(
        "{}T{:02}{:02}{:02}",
        format_date(date_time.date()),
        date_time.hour(),
        date_time.minute(),
        date_time.second()
    )
}

/// A UTC `DATE-TIME` value as written, its fraction of a second dropped.
pub(crate) fn format_instant(timestamp: Timestamp) -> String {
    format!(
        "{}Z",
        format_date_time(timestamp.to_zoned(TimeZone::UTC).datetime())
    )
}

/// A `DATE-TIME` from the model as written, for `UNTIL` and other values that cannot carry a
/// `TZID`: an instant in UTC, a floating or all-day value as itself, and a zoned value resolved
/// to UTC through the bundled time zone database.
pub(crate) fn format_utc_or_local(
    value: &TimeValue,
    losses: &mut Vec<Loss>,
    field: &Field,
) -> String {
    match value {
        TimeValue::Instant { timestamp } => format_instant(*timestamp),
        TimeValue::Floating { date_time } => format_date_time(*date_time),
        TimeValue::AllDay { date } => format_date(*date),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => match TimeZone::get(time_zone.as_str()).and_then(|zone| date_time.to_zoned(zone)) {
            Ok(zoned) => format_instant(zoned.timestamp()),
            Err(_) => {
                losses.push(Loss::new(
                    field.clone(),
                    LossReason::Approximated,
                    format!(
                        "{date_time} in {time_zone} could not be placed in UTC, so it is written floating"
                    ),
                ));
                format_date_time(*date_time)
            }
        },
    }
}

/// A `DURATION` value (RFC 5545 section 3.3.6): its sign, its nominal days (weeks included), and
/// its exact seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Duration {
    pub(crate) negative: bool,
    pub(crate) days: i64,
    pub(crate) seconds: i64,
}

impl Duration {
    /// The duration as a jiff span, days nominal and the rest exact.
    pub(crate) fn span(self) -> Option<Span> {
        let sign = if self.negative { -1 } else { 1 };
        Span::new()
            .try_days(sign * self.days)
            .ok()?
            .try_seconds(sign * self.seconds)
            .ok()
    }

    /// The duration as an exact length, a day counted as 24 hours.
    pub(crate) fn exact(self) -> Option<SignedDuration> {
        let total = self.days.checked_mul(86_400)?.checked_add(self.seconds)?;
        Some(SignedDuration::from_secs(if self.negative {
            -total
        } else {
            total
        }))
    }
}

/// Reads a `DURATION` value.
pub(crate) fn parse_duration(text: &str) -> Option<Duration> {
    let (negative, rest) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let (date_part, time_part) = match rest.split_once(['T', 't']) {
        Some((date, time)) => (date, Some(time)),
        None => (rest, None),
    };
    let mut days: i64 = 0;
    let mut seconds: i64 = 0;
    let mut any = false;
    for (number, unit) in units(date_part)? {
        any = true;
        match unit.to_ascii_uppercase() {
            'W' => days = days.checked_add(number.checked_mul(7)?)?,
            'D' => days = days.checked_add(number)?,
            _ => return None,
        }
    }
    if let Some(time_part) = time_part {
        let parts = units(time_part)?;
        if parts.is_empty() {
            return None;
        }
        for (number, unit) in parts {
            any = true;
            let scale = match unit.to_ascii_uppercase() {
                'H' => 3600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
            seconds = seconds.checked_add(number.checked_mul(scale)?)?;
        }
    }
    any.then_some(Duration {
        negative,
        days,
        seconds,
    })
}

/// The `<digits><unit>` pairs of a duration part.
fn units(text: &str) -> Option<Vec<(i64, char)>> {
    let mut out = Vec::new();
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if digits.is_empty() {
                return None;
            }
            out.push((digits.parse().ok()?, c));
            digits.clear();
        }
    }
    digits.is_empty().then_some(out)
}

/// An exact length as a `DURATION` value, whole days as `D` and the rest as `H`, `M` and `S`. A
/// fraction of a second is dropped.
pub(crate) fn format_duration(duration: SignedDuration) -> String {
    let negative = duration.is_negative();
    let total = duration.as_secs().unsigned_abs();
    let (days, rest) = (total / 86_400, total % 86_400);
    let (hours, minutes, seconds) = (rest / 3600, rest % 3600 / 60, rest % 60);
    let mut out = String::from(if negative { "-P" } else { "P" });
    if days > 0 {
        out.push_str(&format!("{days}D"));
    }
    if rest > 0 || days == 0 {
        out.push('T');
        if hours > 0 {
            out.push_str(&format!("{hours}H"));
        }
        if minutes > 0 {
            out.push_str(&format!("{minutes}M"));
        }
        if seconds > 0 || rest == 0 {
            out.push_str(&format!("{seconds}S"));
        }
    }
    out
}

/// What reading a `RECUR` value found besides the rule.
#[derive(Debug, Default)]
pub(crate) struct RecurNotes {
    /// Rule parts the model has no place for, as written.
    pub(crate) dropped: Vec<String>,
}

/// Reads a `RECUR` value (RFC 5545 section 3.3.10, RFC 7529). `Err` names why the rule is
/// malformed.
pub(crate) fn parse_recur(text: &str) -> Result<(RecurrenceRule, RecurNotes), String> {
    let mut notes = RecurNotes::default();
    let mut frequency = None;
    let mut rule = RecurrenceRule::new(Frequency::Daily);
    let mut count = None;
    let mut until = None;
    for part in text.split(';').filter(|part| !part.is_empty()) {
        let (name, value) = part
            .split_once('=')
            .ok_or_else(|| format!("rule part `{part}` has no value"))?;
        let bad = || format!("rule part `{part}` is malformed");
        match name.to_ascii_uppercase().as_str() {
            "FREQ" => frequency = Some(parse_frequency(value).ok_or_else(bad)?),
            "INTERVAL" => {
                rule.interval = value
                    .parse::<u32>()
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or_else(bad)?;
            }
            "COUNT" => count = Some(value.parse::<u32>().map_err(|_| bad())?),
            "UNTIL" => until = Some(parse_until(value).ok_or_else(bad)?),
            "BYSECOND" => rule.by_second = list(value).ok_or_else(bad)?,
            "BYMINUTE" => rule.by_minute = list(value).ok_or_else(bad)?,
            "BYHOUR" => rule.by_hour = list(value).ok_or_else(bad)?,
            "BYDAY" => {
                rule.by_day = value
                    .split(',')
                    .map(parse_nday)
                    .collect::<Option<_>>()
                    .ok_or_else(bad)?;
            }
            "BYMONTHDAY" => rule.by_month_day = list(value).ok_or_else(bad)?,
            "BYYEARDAY" => rule.by_year_day = list(value).ok_or_else(bad)?,
            "BYWEEKNO" => rule.by_week_no = list(value).ok_or_else(bad)?,
            "BYMONTH" => {
                rule.by_month = value
                    .split(',')
                    .map(parse_month)
                    .collect::<Option<_>>()
                    .ok_or_else(bad)?;
            }
            "BYSETPOS" => rule.by_set_position = list(value).ok_or_else(bad)?,
            "WKST" => rule.first_day_of_week = parse_weekday(value).ok_or_else(bad)?,
            "RSCALE" => rule.rscale = Some(value.to_ascii_lowercase()),
            "SKIP" => {
                rule.skip = match value.to_ascii_uppercase().as_str() {
                    "OMIT" => Skip::Omit,
                    "BACKWARD" => Skip::Backward,
                    "FORWARD" => Skip::Forward,
                    _ => return Err(bad()),
                };
            }
            _ => notes.dropped.push(part.to_owned()),
        }
    }
    rule.frequency = frequency.ok_or_else(|| "the rule has no FREQ".to_owned())?;
    rule.end = match (count, until) {
        (None, None) => RuleEnd::Never,
        (Some(count), None) => RuleEnd::Count { count },
        (None, Some(until)) => RuleEnd::Until { until },
        (Some(count), Some(until)) => {
            notes.dropped.push(format!("COUNT={count}"));
            RuleEnd::Until { until }
        }
    };
    Ok((rule, notes))
}

/// An `UNTIL` value: a `DATE`, a UTC `DATE-TIME`, or a floating one.
fn parse_until(value: &str) -> Option<TimeValue> {
    if let Some(date) = parse_date(value) {
        return Some(TimeValue::all_day(date));
    }
    let (date_time, utc) = parse_date_time(value)?;
    if utc {
        let timestamp = date_time.to_zoned(TimeZone::UTC).ok()?.timestamp();
        Some(TimeValue::instant(timestamp))
    } else {
        Some(TimeValue::floating(date_time))
    }
}

fn parse_frequency(value: &str) -> Option<Frequency> {
    Some(match value.to_ascii_uppercase().as_str() {
        "YEARLY" => Frequency::Yearly,
        "MONTHLY" => Frequency::Monthly,
        "WEEKLY" => Frequency::Weekly,
        "DAILY" => Frequency::Daily,
        "HOURLY" => Frequency::Hourly,
        "MINUTELY" => Frequency::Minutely,
        "SECONDLY" => Frequency::Secondly,
        _ => return None,
    })
}

fn frequency_name(frequency: Frequency) -> &'static str {
    match frequency {
        Frequency::Yearly => "YEARLY",
        Frequency::Monthly => "MONTHLY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Daily => "DAILY",
        Frequency::Hourly => "HOURLY",
        Frequency::Minutely => "MINUTELY",
        Frequency::Secondly => "SECONDLY",
    }
}

fn parse_weekday(value: &str) -> Option<Weekday> {
    Some(match value.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

/// A `BYDAY` entry: `[+|-][n]DD`.
fn parse_nday(value: &str) -> Option<NDay> {
    let split = value.len().checked_sub(2)?;
    if !value.is_char_boundary(split) {
        return None;
    }
    let (nth, day) = value.split_at(split);
    let day = parse_weekday(day)?;
    if nth.is_empty() {
        return Some(NDay::every(day));
    }
    let nth: i32 = nth.strip_prefix('+').unwrap_or(nth).parse().ok()?;
    (nth != 0).then(|| NDay::nth(day, nth))
}

/// A `BYMONTH` entry: a month number, with RFC 7529's `L` suffix for a leap month.
fn parse_month(value: &str) -> Option<RuleMonth> {
    let (number, leap) = match value.strip_suffix(['L', 'l']) {
        Some(number) => (number, true),
        None => (value, false),
    };
    Some(RuleMonth {
        month: number.parse().ok()?,
        leap,
    })
}

/// A comma-separated list of signed integers, each with an optional `+`.
fn list<T: core::str::FromStr>(value: &str) -> Option<Vec<T>> {
    value
        .split(',')
        .map(|item| item.strip_prefix('+').unwrap_or(item).parse().ok())
        .collect()
}

fn join<T: ToString>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// A rule as a `RECUR` value. `RSCALE` comes first, as RFC 7529 section 4.1 asks.
pub(crate) fn format_recur(rule: &RecurrenceRule, losses: &mut Vec<Loss>) -> String {
    let mut parts = Vec::new();
    if let Some(rscale) = &rule.rscale {
        parts.push(format!("RSCALE={}", rscale.to_ascii_uppercase()));
    }
    parts.push(format!("FREQ={}", frequency_name(rule.frequency)));
    match &rule.end {
        RuleEnd::Never => {}
        RuleEnd::Count { count } => parts.push(format!("COUNT={count}")),
        RuleEnd::Until { until } => parts.push(format!(
            "UNTIL={}",
            format_utc_or_local(until, losses, &Field::Recurrence)
        )),
    }
    if rule.interval.get() != 1 {
        parts.push(format!("INTERVAL={}", rule.interval));
    }
    if !rule.by_second.is_empty() {
        parts.push(format!("BYSECOND={}", join(&rule.by_second)));
    }
    if !rule.by_minute.is_empty() {
        parts.push(format!("BYMINUTE={}", join(&rule.by_minute)));
    }
    if !rule.by_hour.is_empty() {
        parts.push(format!("BYHOUR={}", join(&rule.by_hour)));
    }
    if !rule.by_day.is_empty() {
        let days: Vec<String> = rule
            .by_day
            .iter()
            .map(|nday| match nday.nth_of_period {
                Some(nth) => format!("{nth}{}", weekday_name(nday.day)),
                None => weekday_name(nday.day).to_owned(),
            })
            .collect();
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    if !rule.by_month_day.is_empty() {
        parts.push(format!("BYMONTHDAY={}", join(&rule.by_month_day)));
    }
    if !rule.by_year_day.is_empty() {
        parts.push(format!("BYYEARDAY={}", join(&rule.by_year_day)));
    }
    if !rule.by_week_no.is_empty() {
        parts.push(format!("BYWEEKNO={}", join(&rule.by_week_no)));
    }
    if !rule.by_month.is_empty() {
        let months: Vec<String> = rule
            .by_month
            .iter()
            .map(|month| {
                if month.leap {
                    format!("{}L", month.month)
                } else {
                    month.month.to_string()
                }
            })
            .collect();
        parts.push(format!("BYMONTH={}", months.join(",")));
    }
    if !rule.by_set_position.is_empty() {
        parts.push(format!("BYSETPOS={}", join(&rule.by_set_position)));
    }
    if rule.first_day_of_week != Weekday::Monday {
        parts.push(format!("WKST={}", weekday_name(rule.first_day_of_week)));
    }
    match rule.skip {
        Skip::Omit => {}
        Skip::Backward => parts.push("SKIP=BACKWARD".to_owned()),
        Skip::Forward => parts.push("SKIP=FORWARD".to_owned()),
    }
    parts.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_date_times_round_trip() {
        let date = parse_date("20260304").unwrap();
        assert_eq!(format_date(date), "20260304");
        let (date_time, utc) = parse_date_time("20260304T050607Z").unwrap();
        assert!(utc);
        assert_eq!(format_date_time(date_time), "20260304T050607");
        assert!(parse_date("2026034").is_none());
        assert!(parse_date("20261304").is_none());
        assert!(parse_date_time("20260304T25000").is_none());
        assert_eq!(
            parse_date_time("20261231T235960").unwrap().0.second(),
            59,
            "a leap second is the minute's last second"
        );
    }

    #[test]
    fn durations_read_and_write() {
        let duration = parse_duration("-P1W2DT3H4M5S").unwrap();
        assert_eq!(
            duration,
            Duration {
                negative: true,
                days: 9,
                seconds: 3 * 3600 + 4 * 60 + 5
            }
        );
        assert_eq!(
            parse_duration("PT15M").unwrap().exact().unwrap().as_secs(),
            900
        );
        assert!(parse_duration("P").is_none());
        assert!(parse_duration("PT").is_none());
        assert!(parse_duration("P1Y").is_none());
        assert!(parse_duration("15M").is_none());
        assert_eq!(format_duration(SignedDuration::from_secs(-900)), "-PT15M");
        assert_eq!(format_duration(SignedDuration::from_secs(86_400)), "P1D");
        assert_eq!(
            format_duration(SignedDuration::from_secs(90_061)),
            "P1DT1H1M1S"
        );
        assert_eq!(format_duration(SignedDuration::ZERO), "PT0S");
    }

    #[test]
    fn rules_read_and_write() {
        let (rule, notes) =
            parse_recur("FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR,+2MO,SU;COUNT=10;WKST=SU;X-FOO=1")
                .unwrap();
        assert_eq!(rule.frequency, Frequency::Monthly);
        assert_eq!(rule.interval.get(), 2);
        assert_eq!(
            rule.by_day,
            vec![
                NDay::nth(Weekday::Friday, -1),
                NDay::nth(Weekday::Monday, 2),
                NDay::every(Weekday::Sunday)
            ]
        );
        assert_eq!(rule.end, RuleEnd::Count { count: 10 });
        assert_eq!(notes.dropped, vec!["X-FOO=1".to_owned()]);
        let mut losses = Vec::new();
        assert_eq!(
            format_recur(&rule, &mut losses),
            "FREQ=MONTHLY;COUNT=10;INTERVAL=2;BYDAY=-1FR,2MO,SU;WKST=SU"
        );

        let (rule, _) =
            parse_recur("RSCALE=CHINESE;FREQ=YEARLY;BYMONTH=5L;SKIP=FORWARD;UNTIL=20300101")
                .unwrap();
        assert_eq!(rule.rscale.as_deref(), Some("chinese"));
        assert_eq!(
            rule.by_month,
            vec![RuleMonth {
                month: 5,
                leap: true
            }]
        );
        assert_eq!(
            format_recur(&rule, &mut losses),
            "RSCALE=CHINESE;FREQ=YEARLY;UNTIL=20300101;BYMONTH=5L;SKIP=FORWARD"
        );
        assert!(losses.is_empty());
    }

    #[test]
    fn malformed_rules_are_refused() {
        for text in [
            "INTERVAL=2",
            "FREQ=SOMETIMES",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=DAILY;BYDAY=0MO",
            "FREQ",
        ] {
            assert!(parse_recur(text).is_err(), "{text}");
        }
    }
}
