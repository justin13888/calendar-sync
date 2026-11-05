//! python-dateutil's rrule test cases, ported as data (`tests/data/recurrence/dateutil/`).
//!
//! Each case expands one rule from a naive start with [`CivilRule`], which counts only the
//! instances the rule produces, as dateutil does. Where RFC 5545 rules a case out and dateutil
//! expands it anyway, `divergences.json` names the case, the RFC's reason, and the loss this
//! crate reports instead.

use std::collections::BTreeMap;

use calendar_sync_model::{LossReason, RecurrenceRule, RuleEnd, TimeValue};
use calendar_sync_recurrence::CivilRule;
use jiff::civil::DateTime;
use serde::Deserialize;

const CASES: &str = include_str!("../../../tests/data/recurrence/dateutil/cases.json");
const DIVERGENCES: &str = include_str!("../../../tests/data/recurrence/dateutil/divergences.json");

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    start: DateTime,
    rule: RecurrenceRule,
    expected: Vec<DateTime>,
}

#[derive(Deserialize)]
struct Divergence {
    /// Why this crate departs from dateutil, citing the RFC.
    reason: String,
    /// The loss reported instead of instances.
    loss: LossReason,
}

#[test]
fn dateutil_cases_agree() {
    let cases: Cases = serde_json::from_str(CASES).unwrap();
    let divergences: BTreeMap<String, Divergence> = serde_json::from_str(DIVERGENCES).unwrap();
    assert!(cases.cases.len() > 200, "the port lost cases");

    let mut failures = Vec::new();
    let mut diverged = 0;
    for case in &cases.cases {
        let until = match &case.rule.end {
            RuleEnd::Until {
                until: TimeValue::Floating { date_time },
            } => Some(*date_time),
            RuleEnd::Until { until } => panic!("{}: until {until:?} is not floating", case.name),
            RuleEnd::Never | RuleEnd::Count { .. } => None,
        };
        let compiled = CivilRule::new(&case.rule, case.start, until);
        if let Some(divergence) = divergences.get(&case.name) {
            diverged += 1;
            match compiled {
                Err(loss) if loss.reason == divergence.loss => {}
                other => failures.push(format!(
                    "{}: expected a {:?} loss ({}), got {other:?}",
                    case.name, divergence.loss, divergence.reason
                )),
            }
            continue;
        }
        let rule = match compiled {
            Ok(rule) => rule,
            Err(loss) => {
                failures.push(format!("{}: refused: {loss}", case.name));
                continue;
            }
        };
        let horizon: DateTime = "9999-12-31T23:59:59".parse().unwrap();
        let produced: Vec<DateTime> = rule
            .instances(None, horizon)
            .take(case.expected.len() + 1)
            .collect();
        if produced != case.expected {
            failures.push(format!(
                "{}:\n  expected {:?}\n  produced {produced:?}",
                case.name, case.expected
            ));
        }
    }
    assert_eq!(
        diverged,
        divergences.len(),
        "a divergence names no ported case"
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases disagree:\n{}",
        failures.len(),
        cases.cases.len(),
        failures.join("\n")
    );
}
