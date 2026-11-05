//! Graph's `patternedRecurrence` to and from [`Recurrence`].
//!
//! Ground truth: `references/graph/patternedrecurrence.md`, `recurrencepattern.md` and
//! `recurrencerange.md`. Graph holds one pattern (six types) and one range (three types); what
//! else a [`Recurrence`] can say is listed by
//! [`recurrence_losses`](calendar_sync_model::graph::recurrence_losses), and never written.

use calendar_sync_model::graph::recurrence_losses;
use calendar_sync_model::{
    Field, Frequency, Loss, LossReason, NDay, Recurrence, RecurrenceRule, RuleEnd, RuleMonth,
    TimeValue, Weekday,
};
use jiff::ToSpan;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use crate::zones;

/// Graph's `index` values and the `nthOfPeriod` each stands for.
const INDEXES: [(&str, i32); 5] = [
    ("first", 1),
    ("second", 2),
    ("third", 3),
    ("fourth", 4),
    ("last", -1),
];

/// Graph's `dayOfWeek` values.
const DAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

fn day(name: &str) -> Option<Weekday> {
    DAYS.iter()
        .find(|(graph, _)| graph.eq_ignore_ascii_case(name))
        .map(|(_, day)| *day)
}

fn day_name(day: Weekday) -> &'static str {
    DAYS.iter()
        .find(|(_, graph)| *graph == day)
        .map_or("sunday", |(name, _)| name)
}

fn malformed(losses: &mut Vec<Loss>, detail: String) -> Option<Recurrence> {
    losses.push(Loss::new(Field::Recurrence, LossReason::Malformed, detail));
    None
}

/// Reads a `patternedRecurrence` for a series starting at `start`.
///
/// A relative pattern with several weekdays falls, in Graph, on "the first day that satisfies the
/// pattern"; it is read as those weekdays at the `index` position (`BYSETPOS`) and reported as an
/// approximation.
pub(crate) fn read(value: &Value, start: &TimeValue, losses: &mut Vec<Loss>) -> Option<Recurrence> {
    let pattern = value.get("pattern").and_then(Value::as_object)?;
    let range = value.get("range").and_then(Value::as_object);
    let int = |object: &Map<String, Value>, name: &str| object.get(name).and_then(Value::as_i64);
    let kind = pattern.get("type").and_then(Value::as_str).unwrap_or("");
    let frequency = match kind {
        "daily" => Frequency::Daily,
        "weekly" => Frequency::Weekly,
        "absoluteMonthly" | "relativeMonthly" => Frequency::Monthly,
        "absoluteYearly" | "relativeYearly" => Frequency::Yearly,
        other => return malformed(losses, format!("pattern type {other:?}")),
    };
    let mut rule = RecurrenceRule::new(frequency);
    let interval = int(pattern, "interval").unwrap_or(1);
    match u32::try_from(interval).ok().and_then(|n| n.try_into().ok()) {
        Some(interval) => rule.interval = interval,
        None => return malformed(losses, format!("interval {interval}")),
    }
    let mut days = Vec::new();
    for name in pattern
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match name.as_str().and_then(day) {
            Some(day) => days.push(day),
            None => return malformed(losses, format!("day of week {name}")),
        }
    }
    let index = match pattern.get("index").and_then(Value::as_str) {
        None => 1,
        Some(name) => match INDEXES.iter().find(|(graph, _)| *graph == name) {
            Some((_, nth)) => *nth,
            None => return malformed(losses, format!("index {name:?}")),
        },
    };
    let month_day = int(pattern, "dayOfMonth");
    let month = int(pattern, "month");

    match kind {
        "weekly" => {
            rule.by_day = days.into_iter().map(NDay::every).collect();
            rule.first_day_of_week = pattern
                .get("firstDayOfWeek")
                .and_then(Value::as_str)
                .and_then(day)
                .unwrap_or(Weekday::Sunday);
        }
        "absoluteMonthly" | "absoluteYearly" => {
            let Some(month_day) = month_day.and_then(|day| i8::try_from(day).ok()) else {
                return malformed(losses, format!("{kind} without a dayOfMonth"));
            };
            rule.by_month_day = vec![month_day];
        }
        "relativeMonthly" | "relativeYearly" => match days.as_slice() {
            [] => return malformed(losses, format!("{kind} without daysOfWeek")),
            [day] => rule.by_day = vec![NDay::nth(*day, index)],
            _ => {
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Approximated,
                    "a relative pattern with several weekdays, read as the nth of any of them",
                ));
                rule.by_day = days.into_iter().map(NDay::every).collect();
                rule.by_set_position = vec![index];
            }
        },
        _ => {}
    }
    if frequency == Frequency::Yearly {
        let Some(month) = month.and_then(|month| u8::try_from(month).ok()) else {
            return malformed(losses, format!("{kind} without a month"));
        };
        rule.by_month = vec![RuleMonth::new(month)];
    }

    if let Some(range) = range {
        rule.end = match range.get("type").and_then(Value::as_str) {
            None | Some("noEnd") => RuleEnd::Never,
            Some("numbered") => match int(range, "numberOfOccurrences")
                .and_then(|count| u32::try_from(count).ok())
            {
                Some(count) => RuleEnd::Count { count },
                None => return malformed(losses, "numbered range without a count".to_owned()),
            },
            Some("endDate") => {
                match range
                    .get("endDate")
                    .and_then(Value::as_str)
                    .and_then(|date| date.parse::<Date>().ok())
                {
                    Some(end) => RuleEnd::Until {
                        until: until(end, start),
                    },
                    None => {
                        return malformed(losses, "endDate range without an endDate".to_owned());
                    }
                }
            }
            Some(other) => return malformed(losses, format!("range type {other:?}")),
        };
    }
    Some(Recurrence::rule(rule))
}

/// The `UNTIL` an inclusive `endDate` stands for: the end date at the start's time of day, in the
/// start's zone. Occurrences fall at that time of day, so the last one Graph keeps is the last
/// one `UNTIL` does.
fn until(end: Date, start: &TimeValue) -> TimeValue {
    match start {
        TimeValue::AllDay { .. } => TimeValue::all_day(end),
        TimeValue::Floating { date_time } => TimeValue::floating(end.to_datetime(date_time.time())),
        TimeValue::Instant { timestamp } => {
            let time = timestamp.to_zoned(TimeZone::UTC).time();
            instant_at(end.to_datetime(time), &TimeZone::UTC)
        }
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => {
            let zone = zones::iana(time_zone.as_str()).unwrap_or(TimeZone::UTC);
            instant_at(end.to_datetime(date_time.time()), &zone)
        }
    }
}

fn instant_at(civil: DateTime, zone: &TimeZone) -> TimeValue {
    match civil.to_zoned(zone.clone()) {
        Ok(zoned) => TimeValue::instant(zoned.timestamp()),
        Err(_) => TimeValue::floating(civil),
    }
}

/// The civil date and time of `time` as the series sees it: in the start's zone.
fn civil_in_start(time: &TimeValue, start: &TimeValue) -> Option<DateTime> {
    match time {
        TimeValue::AllDay { date } => Some(date.to_datetime(Time::MAX)),
        TimeValue::Floating { date_time } | TimeValue::Zoned { date_time, .. } => Some(*date_time),
        TimeValue::Instant { timestamp } => {
            let zone = match start {
                TimeValue::Zoned { time_zone, .. } => zones::iana(time_zone.as_str())?,
                _ => TimeZone::UTC,
            };
            Some(timestamp.to_zoned(zone).datetime())
        }
    }
}

fn start_civil(start: &TimeValue) -> DateTime {
    match start {
        TimeValue::AllDay { date } => date.to_datetime(Time::midnight()),
        TimeValue::Floating { date_time } | TimeValue::Zoned { date_time, .. } => *date_time,
        TimeValue::Instant { timestamp } => timestamp.to_zoned(TimeZone::UTC).datetime(),
    }
}

/// Writes `recurrence` for a series starting at `start` as a `patternedRecurrence`, or `None`
/// where Graph cannot hold it, with every reason reported.
pub(crate) fn write(
    recurrence: &Recurrence,
    start: &TimeValue,
    losses: &mut Vec<Loss>,
) -> Option<Value> {
    let lost = recurrence_losses(recurrence);
    if !lost.is_empty() {
        losses.extend(lost);
        return None;
    }
    let [rule] = recurrence.rules.as_slice() else {
        return None;
    };
    let first = start_civil(start);
    let interval = rule.interval.get();
    let relative = |pattern: &mut Map<String, Value>| -> bool {
        let (day, nth) = match (rule.by_day.as_slice(), rule.by_set_position.as_slice()) {
            ([day], []) => (day.day, day.nth_of_period.unwrap_or(1)),
            ([day], [nth]) => (day.day, *nth),
            _ => return false,
        };
        let index = INDEXES
            .iter()
            .find(|(_, graph)| *graph == nth)
            .map_or("first", |(name, _)| name);
        pattern.insert("daysOfWeek".to_owned(), json!([day_name(day)]));
        pattern.insert("index".to_owned(), json!(index));
        true
    };
    let month_day = rule.by_month_day.first().copied().unwrap_or(first.day());
    let mut pattern = Map::new();
    pattern.insert("interval".to_owned(), json!(interval));
    let kind = match rule.frequency {
        Frequency::Daily => "daily",
        Frequency::Weekly => {
            let days: Vec<&str> = if rule.by_day.is_empty() {
                vec![day_name(first.weekday().into())]
            } else {
                rule.by_day.iter().map(|day| day_name(day.day)).collect()
            };
            pattern.insert("daysOfWeek".to_owned(), json!(days));
            pattern.insert(
                "firstDayOfWeek".to_owned(),
                json!(day_name(rule.first_day_of_week)),
            );
            "weekly"
        }
        Frequency::Monthly => {
            if relative(&mut pattern) {
                "relativeMonthly"
            } else {
                pattern.insert("dayOfMonth".to_owned(), json!(month_day));
                "absoluteMonthly"
            }
        }
        Frequency::Yearly => {
            let month = rule.by_month.first().map_or(first.month(), |month| {
                i8::try_from(month.month).unwrap_or(1)
            });
            pattern.insert("month".to_owned(), json!(month));
            if relative(&mut pattern) {
                "relativeYearly"
            } else {
                pattern.insert("dayOfMonth".to_owned(), json!(month_day));
                "absoluteYearly"
            }
        }
        // `recurrence_losses` refuses every finer frequency.
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => return None,
    };
    pattern.insert("type".to_owned(), json!(kind));

    let mut range = Map::new();
    range.insert("startDate".to_owned(), json!(first.date().to_string()));
    if let TimeValue::Zoned { time_zone, .. } = start {
        range.insert(
            "recurrenceTimeZone".to_owned(),
            json!(zones::graph_name(time_zone.as_str())),
        );
    }
    match &rule.end {
        RuleEnd::Never => {
            range.insert("type".to_owned(), json!("noEnd"));
        }
        RuleEnd::Count { count } => {
            range.insert("type".to_owned(), json!("numbered"));
            range.insert("numberOfOccurrences".to_owned(), json!(count));
        }
        RuleEnd::Until { until } => {
            let Some(bound) = civil_in_start(until, start) else {
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Unsupported,
                    "UNTIL in a zone the bundled database does not know",
                ));
                return None;
            };
            // `endDate` keeps every occurrence on its day; an UNTIL before the start's time of
            // day ends the series a day earlier.
            let end = if bound.time() < first.time() {
                bound.date().checked_sub(1.day()).unwrap_or(bound.date())
            } else {
                bound.date()
            };
            range.insert("type".to_owned(), json!("endDate"));
            range.insert("endDate".to_owned(), json!(end.to_string()));
        }
    }
    Some(json!({ "pattern": pattern, "range": range }))
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::{
        Frequency, LossReason, NDay, Recurrence, RecurrenceRule, RuleEnd, RuleMonth, TimeValue,
        TimeZoneId, Weekday,
    };
    use jiff::civil::date;
    use serde_json::json;

    use super::{read, write};

    fn toronto(y: i16, m: i8, d: i8, h: i8) -> TimeValue {
        TimeValue::zoned(
            date(y, m, d).at(h, 0, 0, 0),
            TimeZoneId::new("America/Toronto"),
        )
    }

    fn round_trip(value: &serde_json::Value, start: &TimeValue) -> Recurrence {
        let mut losses = Vec::new();
        let recurrence = read(value, start, &mut losses).expect("read");
        assert!(losses.is_empty(), "{losses:?}");
        let written = write(&recurrence, start, &mut losses).expect("written");
        assert!(losses.is_empty(), "{losses:?}");
        assert_eq!(read(&written, start, &mut losses), Some(recurrence.clone()));
        recurrence
    }

    #[test]
    fn the_six_patterns_round_trip() {
        let start = toronto(2026, 11, 3, 10);
        let range = json!({ "type": "noEnd", "startDate": "2026-11-03" });
        let weekly = round_trip(
            &json!({
                "pattern": { "type": "weekly", "interval": 2, "daysOfWeek": ["tuesday", "thursday"], "firstDayOfWeek": "monday" },
                "range": { "type": "numbered", "numberOfOccurrences": 10, "startDate": "2026-11-03" }
            }),
            &start,
        );
        let rule = &weekly.rules[0];
        assert_eq!(rule.frequency, Frequency::Weekly);
        assert_eq!(rule.interval.get(), 2);
        assert_eq!(
            rule.by_day,
            [
                NDay::every(Weekday::Tuesday),
                NDay::every(Weekday::Thursday)
            ]
        );
        assert_eq!(rule.first_day_of_week, Weekday::Monday);
        assert_eq!(rule.end, RuleEnd::Count { count: 10 });

        let daily = round_trip(
            &json!({ "pattern": { "type": "daily", "interval": 3 }, "range": range }),
            &start,
        );
        assert_eq!(daily.rules[0].frequency, Frequency::Daily);

        let absolute_monthly = round_trip(
            &json!({ "pattern": { "type": "absoluteMonthly", "interval": 1, "dayOfMonth": 15 }, "range": range }),
            &start,
        );
        assert_eq!(absolute_monthly.rules[0].by_month_day, [15]);

        let relative_monthly = round_trip(
            &json!({ "pattern": { "type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"], "index": "last" }, "range": range }),
            &start,
        );
        assert_eq!(
            relative_monthly.rules[0].by_day,
            [NDay::nth(Weekday::Friday, -1)]
        );

        let absolute_yearly = round_trip(
            &json!({ "pattern": { "type": "absoluteYearly", "interval": 1, "dayOfMonth": 3, "month": 11 }, "range": range }),
            &start,
        );
        assert_eq!(absolute_yearly.rules[0].by_month, [RuleMonth::new(11)]);

        let relative_yearly = round_trip(
            &json!({ "pattern": { "type": "relativeYearly", "interval": 1, "daysOfWeek": ["thursday"], "index": "fourth", "month": 11 }, "range": range }),
            &start,
        );
        assert_eq!(
            relative_yearly.rules[0].by_day,
            [NDay::nth(Weekday::Thursday, 4)]
        );
    }

    #[test]
    fn an_end_date_is_the_start_time_on_that_day() {
        let start = toronto(2026, 11, 3, 10);
        let recurrence = round_trip(
            &json!({
                "pattern": { "type": "daily", "interval": 1 },
                "range": { "type": "endDate", "startDate": "2026-11-03", "endDate": "2026-11-10" }
            }),
            &start,
        );
        assert_eq!(
            recurrence.rules[0].end,
            RuleEnd::Until {
                // 10:00 in Toronto, standard time.
                until: TimeValue::instant("2026-11-10T15:00:00Z".parse().unwrap())
            }
        );
        // An UNTIL earlier in the day than the start keeps no occurrence on that day.
        let mut rule = RecurrenceRule::new(Frequency::Daily);
        rule.end = RuleEnd::Until {
            until: TimeValue::instant("2026-11-10T14:00:00Z".parse().unwrap()),
        };
        let mut losses = Vec::new();
        let written = write(&Recurrence::rule(rule), &start, &mut losses).unwrap();
        assert_eq!(written["range"]["endDate"], "2026-11-09");
        assert_eq!(
            written["range"]["recurrenceTimeZone"],
            "Eastern Standard Time"
        );
    }

    #[test]
    fn what_graph_cannot_hold_is_not_written() {
        let start = toronto(2026, 11, 3, 10);
        let mut losses = Vec::new();
        let recurrence = Recurrence {
            excluded_dates: vec![toronto(2026, 11, 5, 10)],
            ..Recurrence::rule(RecurrenceRule::new(Frequency::Daily))
        };
        assert_eq!(write(&recurrence, &start, &mut losses), None);
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].reason, LossReason::Unrepresentable);
    }

    #[test]
    fn defaults_come_from_the_start() {
        let start = TimeValue::all_day(date(2026, 11, 3));
        let mut losses = Vec::new();
        let written = write(
            &Recurrence::rule(RecurrenceRule::new(Frequency::Weekly)),
            &start,
            &mut losses,
        )
        .unwrap();
        assert_eq!(written["pattern"]["daysOfWeek"], json!(["tuesday"]));
        let written = write(
            &Recurrence::rule(RecurrenceRule::new(Frequency::Yearly)),
            &start,
            &mut losses,
        )
        .unwrap();
        assert_eq!(written["pattern"]["type"], "absoluteYearly");
        assert_eq!(written["pattern"]["month"], 11);
        assert_eq!(written["pattern"]["dayOfMonth"], 3);
        assert!(losses.is_empty());
    }

    #[test]
    fn several_relative_weekdays_and_malformed_patterns_are_reported() {
        let start = toronto(2026, 11, 3, 10);
        let mut losses = Vec::new();
        let read_back = read(
            &json!({ "pattern": { "type": "relativeMonthly", "interval": 1, "daysOfWeek": ["thursday", "friday"], "index": "second" } }),
            &start,
            &mut losses,
        )
        .unwrap();
        assert_eq!(read_back.rules[0].by_set_position, [2]);
        assert_eq!(losses[0].reason, LossReason::Approximated);
        for bad in [
            json!({ "pattern": { "type": "hourly", "interval": 1 } }),
            json!({ "pattern": { "type": "daily", "interval": 0 } }),
            json!({ "pattern": { "type": "absoluteMonthly", "interval": 1 } }),
            json!({ "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["someday"] } }),
            json!({ "pattern": { "type": "daily", "interval": 1 }, "range": { "type": "numbered" } }),
        ] {
            let mut losses = Vec::new();
            assert_eq!(read(&bad, &start, &mut losses), None, "{bad}");
            assert_eq!(losses[0].reason, LossReason::Malformed, "{bad}");
        }
    }
}
