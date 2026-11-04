//! Records calcard 0.3.14's expansion of every case, for the differential test.
//!
//! This is not part of any crate in this repository: calcard may not be a dependency of one
//! (docs/adr/0001-icalendar-crate.md). `README.md` says how to build and run it out of tree.
//!
//! Usage: oracle DATEUTIL_CASES DATEUTIL_DIVERGENCES CALCARD_CASES > expected.json

use std::collections::BTreeMap;
use std::fmt::Write as _;

use calcard::icalendar::ICalendar;
use calcard::icalendar::dates::TimeOrDelta;
use calendar_sync_model::{NDay, RecurrenceRule, RuleEnd, TimeValue, Weekday};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct DateutilCases {
    cases: Vec<DateutilCase>,
}

#[derive(Deserialize)]
struct DateutilCase {
    name: String,
    start: jiff::civil::DateTime,
    rule: RecurrenceRule,
}

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    start: TimeValue,
    #[serde(default)]
    end: Option<TimeValue>,
    #[serde(default)]
    rules: Vec<RecurrenceRule>,
    #[serde(default)]
    dates: Vec<TimeValue>,
    #[serde(default)]
    excluded_dates: Vec<TimeValue>,
    #[serde(default)]
    overrides: Vec<OverrideCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverrideCase {
    key: TimeValue,
    start: TimeValue,
    #[serde(default)]
    end: Option<TimeValue>,
    #[serde(default)]
    range: Option<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let read = |path: &str| std::fs::read_to_string(path).unwrap();
    let dateutil: DateutilCases = serde_json::from_str(&read(&args[1])).unwrap();
    let divergences: BTreeMap<String, Value> = serde_json::from_str(&read(&args[2])).unwrap();
    let cases: Cases = serde_json::from_str(&read(&args[3])).unwrap();

    let mut all = Vec::new();
    for case in dateutil.cases {
        if divergences.contains_key(&case.name) {
            continue;
        }
        // An hour long: calcard ends a DATE-TIME start with no end at 23:59:59, against
        // RFC 5545 section 3.6.1 (docs/adr/0001-icalendar-crate.md).
        let end = case.start + jiff::SignedDuration::from_hours(1);
        all.push(Case {
            name: format!("dateutil/{}", case.name),
            start: TimeValue::floating(case.start),
            end: Some(TimeValue::floating(end)),
            rules: vec![case.rule],
            dates: Vec::new(),
            excluded_dates: Vec::new(),
            overrides: Vec::new(),
        });
    }
    all.extend(cases.cases);

    let mut expected = BTreeMap::new();
    for case in &all {
        let ics = render(case);
        let calendar = ICalendar::parse(&ics).unwrap_or_else(|e| panic!("{}: {e:?}", case.name));
        let expansion = calendar.expand_dates(Utc, 100_000);
        if !expansion.errors.is_empty() {
            expected.insert(case.name.clone(), Value::String(format!("{:?}", expansion.errors)));
            continue;
        }
        let mut spans: Vec<(String, String)> = expansion
            .events
            .iter()
            .map(|event| {
                let start = event.start.with_timezone(&Utc);
                let end = match &event.end {
                    TimeOrDelta::Time(end) => end.with_timezone(&Utc),
                    TimeOrDelta::Delta(delta) => start + *delta,
                };
                (
                    start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                )
            })
            .collect();
        spans.sort();
        expected.insert(case.name.clone(), serde_json::to_value(spans).unwrap());
    }
    let out = serde_json::json!({
        "oracle": "calcard 0.3.14, ICalendar::expand_dates with UTC as the default zone",
        "expected": expected,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

fn value(time: &TimeValue) -> String {
    match time {
        TimeValue::Instant { timestamp } => {
            format!(":{}", timestamp.strftime("%Y%m%dT%H%M%SZ"))
        }
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => format!(";TZID={time_zone}:{}", date_time.strftime("%Y%m%dT%H%M%S")),
        TimeValue::Floating { date_time } => format!(":{}", date_time.strftime("%Y%m%dT%H%M%S")),
        TimeValue::AllDay { date } => format!(";VALUE=DATE:{}", date.strftime("%Y%m%d")),
    }
}

fn weekday(day: Weekday) -> &'static str {
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

fn list<T: ToString>(name: &str, values: &[T], out: &mut String) {
    if !values.is_empty() {
        let joined: Vec<String> = values.iter().map(ToString::to_string).collect();
        write!(out, ";{name}={}", joined.join(",")).unwrap();
    }
}

fn rule(rule: &RecurrenceRule) -> String {
    let mut out = format!("FREQ={:?}", rule.frequency).to_uppercase();
    write!(out, ";INTERVAL={}", rule.interval).unwrap();
    write!(out, ";WKST={}", weekday(rule.first_day_of_week)).unwrap();
    let months: Vec<u8> = rule.by_month.iter().map(|m| m.month).collect();
    list("BYMONTH", &months, &mut out);
    let days: Vec<String> = rule
        .by_day
        .iter()
        .map(|NDay { day, nth_of_period }| match nth_of_period {
            Some(nth) => format!("{nth}{}", weekday(*day)),
            None => weekday(*day).to_owned(),
        })
        .collect();
    list("BYDAY", &days, &mut out);
    list("BYMONTHDAY", &rule.by_month_day, &mut out);
    list("BYYEARDAY", &rule.by_year_day, &mut out);
    list("BYWEEKNO", &rule.by_week_no, &mut out);
    list("BYHOUR", &rule.by_hour, &mut out);
    list("BYMINUTE", &rule.by_minute, &mut out);
    list("BYSECOND", &rule.by_second, &mut out);
    list("BYSETPOS", &rule.by_set_position, &mut out);
    match &rule.end {
        RuleEnd::Never => {}
        RuleEnd::Count { count } => write!(out, ";COUNT={count}").unwrap(),
        RuleEnd::Until { until } => {
            let text = value(until);
            let text = text.rsplit(':').next().unwrap();
            write!(out, ";UNTIL={text}").unwrap();
        }
    }
    out
}

fn render(case: &Case) -> String {
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oracle//EN\r\n");
    out.push_str("BEGIN:VEVENT\r\nUID:case\r\nDTSTAMP:20240101T000000Z\r\n");
    writeln!(out, "DTSTART{}\r", value(&case.start)).unwrap();
    if let Some(end) = &case.end {
        writeln!(out, "DTEND{}\r", value(end)).unwrap();
    }
    for r in &case.rules {
        writeln!(out, "RRULE:{}\r", rule(r)).unwrap();
    }
    for date in &case.dates {
        writeln!(out, "RDATE{}\r", value(date)).unwrap();
    }
    for date in &case.excluded_dates {
        writeln!(out, "EXDATE{}\r", value(date)).unwrap();
    }
    out.push_str("END:VEVENT\r\n");
    for o in &case.overrides {
        out.push_str("BEGIN:VEVENT\r\nUID:case\r\nDTSTAMP:20240101T000000Z\r\n");
        let range = if o.range.as_deref() == Some("thisAndFuture") {
            ";RANGE=THISANDFUTURE"
        } else {
            ""
        };
        writeln!(out, "RECURRENCE-ID{range}{}\r", value(&o.key)).unwrap();
        writeln!(out, "DTSTART{}\r", value(&o.start)).unwrap();
        if let Some(end) = &o.end {
            writeln!(out, "DTEND{}\r", value(end)).unwrap();
        }
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}
