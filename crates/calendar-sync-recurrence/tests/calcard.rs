//! Differential test against calcard 0.3.14, recorded out of tree
//! (`tests/data/recurrence/calcard/`).
//!
//! calcard may not be a dependency of any crate here (docs/adr/0001-icalendar-crate.md), so its
//! expansions were recorded once by `oracle.rs` and are compared as data. The cases avoid
//! daylight-saving transitions and custom zones, where calcard is known to be wrong. Where
//! calcard disagrees with RFC 5545 elsewhere, `divergences.json` names the case, the RFC's
//! reason, and the occurrences the RFC calls for, which this crate must produce instead.

mod common;

use std::collections::BTreeMap;

use calendar_sync_model::{
    EventSeries, InstanceKey, InstanceRange, Override, Recurrence, RecurrenceRule, RuleEnd,
    TimeValue,
};
use calendar_sync_recurrence::Expander;
use common::{event, tz, window, zones};
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::Value;

const DATEUTIL_CASES: &str = include_str!("../../../tests/data/recurrence/dateutil/cases.json");
const DATEUTIL_DIVERGENCES: &str =
    include_str!("../../../tests/data/recurrence/dateutil/divergences.json");
const CASES: &str = include_str!("../../../tests/data/recurrence/calcard/cases.json");
const EXPECTED: &str = include_str!("../../../tests/data/recurrence/calcard/expected.json");
const DIVERGENCES: &str = include_str!("../../../tests/data/recurrence/calcard/divergences.json");

#[derive(Deserialize)]
struct DateutilCases {
    cases: Vec<DateutilCase>,
}

#[derive(Deserialize)]
struct DateutilCase {
    name: String,
    start: DateTime,
    rule: RecurrenceRule,
    expected: Vec<DateTime>,
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
    /// Whether the rule produces the start. Where it does not, RFC 5545 section 3.8.5.3 calls
    /// the set undefined: this crate counts the start as the first instance, as section 3.3.10
    /// says of `COUNT`, and calcard leaves it out. Only the rule's own instances are compared.
    #[serde(skip, default = "synchronized")]
    synchronized: bool,
}

fn synchronized() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverrideCase {
    key: TimeValue,
    start: TimeValue,
    #[serde(default)]
    end: Option<TimeValue>,
    #[serde(default)]
    range: InstanceRange,
}

#[derive(Deserialize)]
struct Expected {
    expected: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct Divergence {
    /// Why calcard is wrong here, citing the RFC.
    reason: String,
    /// The occurrences RFC 5545 calls for, as UTC start and end.
    occurrences: Vec<(String, String)>,
}

fn series_of(case: &Case) -> EventSeries {
    let mut master = event("master", case.start.clone(), case.end.clone());
    master.fields.recurrence = Some(Recurrence {
        rules: case.rules.clone(),
        exclusion_rules: Vec::new(),
        dates: case.dates.clone(),
        excluded_dates: case.excluded_dates.clone(),
    });
    let mut series = EventSeries::single(master);
    for o in &case.overrides {
        series.overrides.insert(
            InstanceKey::new(o.key.clone()),
            Override::Modified {
                event: Box::new(event("override", o.start.clone(), o.end.clone())),
                range: o.range,
            },
        );
    }
    series
}

fn utc(at: jiff::Timestamp) -> String {
    TimeZone::UTC
        .to_datetime(at)
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[test]
fn calcard_expansions_agree() {
    let dateutil: DateutilCases = serde_json::from_str(DATEUTIL_CASES).unwrap();
    let dateutil_divergences: BTreeMap<String, Value> =
        serde_json::from_str(DATEUTIL_DIVERGENCES).unwrap();
    let cases: Cases = serde_json::from_str(CASES).unwrap();
    let expected: Expected = serde_json::from_str(EXPECTED).unwrap();
    let divergences: BTreeMap<String, Divergence> = serde_json::from_str(DIVERGENCES).unwrap();

    let mut all: Vec<Case> = dateutil
        .cases
        .into_iter()
        .filter(|case| !dateutil_divergences.contains_key(&case.name))
        .map(|case| Case {
            name: format!("dateutil/{}", case.name),
            start: TimeValue::floating(case.start),
            // An hour long, as oracle.rs records it: calcard ends a DATE-TIME start with no end
            // at 23:59:59, against RFC 5545 section 3.6.1.
            end: Some(TimeValue::floating(
                case.start + jiff::SignedDuration::from_hours(1),
            )),
            synchronized: case.expected.first() == Some(&case.start),
            rules: vec![case.rule],
            dates: Vec::new(),
            excluded_dates: Vec::new(),
            overrides: Vec::new(),
        })
        .collect();
    all.extend(cases.cases);
    assert_eq!(all.len(), expected.expected.len(), "the recording is stale");

    let zones = zones();
    let expander = Expander::new(&zones, tz("UTC"));
    let wide = window("1900-01-01T00:00:00Z", "2200-01-01T00:00:00Z");
    let mut failures = Vec::new();
    let mut diverged = 0;
    for case in &all {
        let series = series_of(case);
        let expansion = expander.expand(&series, &wide);
        let produced: Vec<(String, String)> = expansion
            .occurrences
            .iter()
            .map(|o| (utc(o.start), utc(o.end)))
            .collect();
        let want: Vec<(String, String)> = match divergences.get(&case.name) {
            Some(divergence) => {
                diverged += 1;
                assert!(!divergence.reason.is_empty());
                divergence.occurrences.clone()
            }
            None => match serde_json::from_value(expected.expected[&case.name].clone()) {
                Ok(spans) => spans,
                Err(_) => {
                    failures.push(format!(
                        "{}: calcard refused it ({}) and no divergence says why",
                        case.name, expected.expected[&case.name]
                    ));
                    continue;
                }
            },
        };
        let mut sorted = produced.clone();
        sorted.sort();
        let mut want = want;
        if !case.synchronized && !divergences.contains_key(&case.name) && !sorted.is_empty() {
            // Drop the start, which only this crate shows; with a COUNT it also took one of
            // the rule's instances, so calcard's list is cut to match.
            sorted.remove(0);
            if case
                .rules
                .iter()
                .any(|r| matches!(r.end, RuleEnd::Count { .. }))
            {
                want.truncate(sorted.len());
            }
        }
        if sorted != want || !expansion.losses.is_empty() {
            failures.push(format!(
                "{}:\n  expected {want:?}\n  produced {sorted:?}\n  losses {:?}",
                case.name, expansion.losses
            ));
        }
    }
    assert_eq!(diverged, divergences.len(), "a divergence names no case");
    assert!(
        failures.is_empty(),
        "{} of {} cases disagree:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}
