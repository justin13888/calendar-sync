//! Which recurrences Microsoft Graph can hold.
//!
//! Graph has no `RRULE`. A recurring event carries a `patternedRecurrence`: one of six pattern
//! types (`daily`, `weekly`, `absoluteMonthly`, `relativeMonthly`, `absoluteYearly`,
//! `relativeYearly`) and one of three range types (`endDate`, `noEnd`, `numbered`). Ground truth is
//! `references/graph/recurrencepattern.md`, `recurrencerange.md` and `patternedrecurrence.md`.
//!
//! [`recurrence_losses`] lists every part of a [`Recurrence`] that no pattern can hold, so the
//! Graph mapper can refuse or report a write instead of sending a different series.

use crate::{Field, Frequency, Loss, LossReason, Recurrence, RecurrenceRule, RuleEnd, Skip};

/// The `nthOfPeriod` values Graph's `index` can hold: `first` to `fourth`, and `last`.
const GRAPH_INDEXES: [i32; 5] = [1, 2, 3, 4, -1];

/// Every part of `recurrence` that a Graph `patternedRecurrence` cannot hold. Empty means it can
/// be written to Graph as it is.
///
/// Conservative where Graph's documentation is ambiguous: a relative pattern with more than one
/// weekday is reported, because Graph places it on "the first day that satisfies the pattern",
/// which an `RRULE` does not.
///
/// Not checked here, because it needs the event's start: an `UNTIL` that falls on the same day
/// as an occurrence but before its start time. Graph's `endDate` is a whole day, inclusive, so
/// the Graph mapper must check that case itself.
#[must_use]
pub fn recurrence_losses(recurrence: &Recurrence) -> Vec<Loss> {
    let mut losses = Vec::new();
    let mut lose = |detail: &str| {
        losses.push(Loss::new(
            Field::Recurrence,
            LossReason::Unrepresentable,
            detail,
        ));
    };

    match recurrence.rules.as_slice() {
        [] => lose("no rule: Graph recurs only by a pattern"),
        [rule] => rule_losses(rule, &mut lose),
        [_, _, ..] => lose("more than one rule: Graph holds one pattern"),
    }
    if !recurrence.exclusion_rules.is_empty() {
        lose("exclusion rules (EXRULE)");
    }
    if !recurrence.dates.is_empty() {
        lose("extra dates (RDATE)");
    }
    if !recurrence.excluded_dates.is_empty() {
        lose("excluded dates (EXDATE): Graph removes an occurrence by deleting it");
    }
    losses
}

/// The parts of one rule that no Graph pattern holds.
fn rule_losses(rule: &RecurrenceRule, lose: &mut impl FnMut(&str)) {
    if !rule.is_gregorian() {
        lose("a non-Gregorian calendar (RSCALE)");
    }
    if rule.skip != Skip::Omit {
        lose("SKIP other than OMIT");
    }
    if !rule.by_year_day.is_empty() {
        lose("BYYEARDAY");
    }
    if !rule.by_week_no.is_empty() {
        lose("BYWEEKNO");
    }
    if !rule.by_hour.is_empty() || !rule.by_minute.is_empty() || !rule.by_second.is_empty() {
        lose("BYHOUR, BYMINUTE or BYSECOND");
    }
    if i32::try_from(rule.interval.get()).is_err() {
        lose("an INTERVAL beyond Graph's Int32");
    }
    match rule.end {
        RuleEnd::Count { count: 0 } => {
            lose("COUNT=0: Graph's numberOfOccurrences must be positive");
        }
        RuleEnd::Count { count } if i32::try_from(count).is_err() => {
            lose("a COUNT beyond Graph's Int32");
        }
        _ => {}
    }

    match rule.frequency {
        Frequency::Daily => {
            if !rule.by_day.is_empty() {
                lose("BYDAY on a daily rule");
            }
            no_month_day(rule, lose);
            no_month(rule, lose);
            no_set_position(rule, lose);
        }
        Frequency::Weekly => {
            if rule.by_day.iter().any(|day| day.nth_of_period.is_some()) {
                lose("an ordinal BYDAY on a weekly rule");
            }
            no_month_day(rule, lose);
            no_month(rule, lose);
            no_set_position(rule, lose);
        }
        Frequency::Monthly => {
            no_month(rule, lose);
            day_in_period(rule, lose);
        }
        Frequency::Yearly => {
            match rule.by_month.as_slice() {
                // Without BYMONTH, RFC 5545 expands a BYMONTHDAY to every month and counts a
                // BYDAY position within the year; Graph's yearly patterns need one month and
                // count `index` within it.
                [] if !rule.by_month_day.is_empty() || !rule.by_day.is_empty() => {
                    lose("BYMONTHDAY or BYDAY on a yearly rule without BYMONTH");
                }
                [] => {}
                [month] if !month.leap => {}
                [month] if month.leap => lose("a leap month"),
                _ => lose("more than one BYMONTH"),
            }
            day_in_period(rule, lose);
        }
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
            lose("a frequency finer than daily");
        }
    }
}

/// A monthly or yearly rule's day: absolute (one positive `BYMONTHDAY`), relative (one weekday at
/// a Graph index), or neither, when the mapper takes the day from the start.
fn day_in_period(rule: &RecurrenceRule, lose: &mut impl FnMut(&str)) {
    match (rule.by_month_day.as_slice(), rule.by_day.as_slice()) {
        ([], []) => no_set_position(rule, lose),
        ([day], []) => {
            if *day < 1 {
                lose("a BYMONTHDAY counted from the end of the month");
            }
            no_set_position(rule, lose);
        }
        ([], [day]) => match (day.nth_of_period.as_ref(), rule.by_set_position.as_slice()) {
            (Some(nth), []) | (None, [nth]) if GRAPH_INDEXES.contains(nth) => {}
            (Some(_), []) | (None, [_]) => lose("a weekday position other than 1 to 4 or -1"),
            (None, []) => lose("every such weekday in the period: Graph needs one position"),
            (Some(_), [_, ..]) | (None, [_, _, ..]) => lose("BYSETPOS beside other parts"),
        },
        ([], [_, _, ..]) => lose("more than one weekday in a relative pattern"),
        ([_, _, ..], _) => lose("more than one BYMONTHDAY"),
        ([_], [_, ..]) => lose("BYMONTHDAY and BYDAY together"),
    }
}

fn no_month_day(rule: &RecurrenceRule, lose: &mut impl FnMut(&str)) {
    if !rule.by_month_day.is_empty() {
        lose("BYMONTHDAY on this frequency");
    }
}

fn no_month(rule: &RecurrenceRule, lose: &mut impl FnMut(&str)) {
    if !rule.by_month.is_empty() {
        lose("BYMONTH on this frequency");
    }
}

fn no_set_position(rule: &RecurrenceRule, lose: &mut impl FnMut(&str)) {
    if !rule.by_set_position.is_empty() {
        lose("BYSETPOS");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NDay, RuleMonth, Weekday};

    fn rule(frequency: Frequency) -> RecurrenceRule {
        RecurrenceRule::new(frequency)
    }

    fn losses(rule: RecurrenceRule) -> Vec<Loss> {
        recurrence_losses(&Recurrence::rule(rule))
    }

    #[test]
    fn the_six_graph_patterns_are_held() {
        let daily = RecurrenceRule {
            interval: 3.try_into().unwrap(),
            ..rule(Frequency::Daily)
        };
        let weekly = RecurrenceRule {
            by_day: vec![NDay::every(Weekday::Monday), NDay::every(Weekday::Tuesday)],
            first_day_of_week: Weekday::Sunday,
            ..rule(Frequency::Weekly)
        };
        let absolute_monthly = RecurrenceRule {
            by_month_day: vec![15],
            ..rule(Frequency::Monthly)
        };
        let relative_monthly = RecurrenceRule {
            by_day: vec![NDay::nth(Weekday::Thursday, 2)],
            ..rule(Frequency::Monthly)
        };
        let relative_monthly_setpos = RecurrenceRule {
            by_day: vec![NDay::every(Weekday::Friday)],
            by_set_position: vec![-1],
            ..rule(Frequency::Monthly)
        };
        let absolute_yearly = RecurrenceRule {
            by_month: vec![RuleMonth::new(3)],
            by_month_day: vec![15],
            end: RuleEnd::Count { count: 10 },
            ..rule(Frequency::Yearly)
        };
        let relative_yearly = RecurrenceRule {
            by_month: vec![RuleMonth::new(11)],
            by_day: vec![NDay::nth(Weekday::Thursday, 4)],
            ..rule(Frequency::Yearly)
        };
        for held in [
            daily,
            weekly,
            absolute_monthly,
            relative_monthly,
            relative_monthly_setpos,
            absolute_yearly,
            relative_yearly,
            rule(Frequency::Monthly),
        ] {
            assert_eq!(losses(held.clone()), Vec::new(), "{held:?}");
        }
    }

    #[test]
    fn rules_graph_cannot_hold_are_reported() {
        let cases = [
            rule(Frequency::Hourly),
            RecurrenceRule {
                by_day: vec![NDay::every(Weekday::Monday)],
                ..rule(Frequency::Daily)
            },
            RecurrenceRule {
                by_day: vec![NDay::nth(Weekday::Monday, 5)],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_day: vec![NDay::nth(Weekday::Monday, -2)],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_day: vec![NDay::every(Weekday::Monday)],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_day: vec![NDay::nth(Weekday::Monday, 1), NDay::nth(Weekday::Friday, 1)],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_month_day: vec![-1],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_month_day: vec![1, 15],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_month_day: vec![1],
                by_day: vec![NDay::every(Weekday::Monday)],
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_month: vec![RuleMonth::new(1), RuleMonth::new(7)],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_month: vec![RuleMonth {
                    month: 5,
                    leap: true,
                }],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_month_day: vec![15],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_day: vec![NDay::nth(Weekday::Friday, -1)],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_day: vec![NDay::every(Weekday::Friday)],
                by_set_position: vec![-1],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                rscale: Some("hebrew".into()),
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                skip: Skip::Forward,
                ..rule(Frequency::Monthly)
            },
            RecurrenceRule {
                by_week_no: vec![20],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_year_day: vec![100],
                ..rule(Frequency::Yearly)
            },
            RecurrenceRule {
                by_hour: vec![9],
                ..rule(Frequency::Daily)
            },
            RecurrenceRule {
                by_set_position: vec![1],
                ..rule(Frequency::Weekly)
            },
            RecurrenceRule {
                end: RuleEnd::Count { count: 0 },
                ..rule(Frequency::Daily)
            },
            RecurrenceRule {
                end: RuleEnd::Count { count: u32::MAX },
                ..rule(Frequency::Daily)
            },
            RecurrenceRule {
                interval: u32::MAX.try_into().unwrap(),
                ..rule(Frequency::Daily)
            },
        ];
        for lost in cases {
            let found = losses(lost.clone());
            assert!(!found.is_empty(), "{lost:?}");
            assert!(
                found.iter().all(|loss| loss.field == Field::Recurrence
                    && loss.reason == LossReason::Unrepresentable),
                "{found:?}"
            );
        }
    }

    #[test]
    fn series_level_parts_are_reported() {
        let date = crate::TimeValue::all_day(jiff::civil::date(2026, 1, 1));
        assert_eq!(recurrence_losses(&Recurrence::default()).len(), 1);
        let two_rules = Recurrence {
            rules: vec![rule(Frequency::Daily), rule(Frequency::Weekly)],
            ..Recurrence::default()
        };
        assert_eq!(recurrence_losses(&two_rules).len(), 1);
        let everything = Recurrence {
            rules: vec![rule(Frequency::Daily)],
            exclusion_rules: vec![rule(Frequency::Weekly)],
            dates: vec![date.clone()],
            excluded_dates: vec![date],
        };
        assert_eq!(recurrence_losses(&everything).len(), 3);
    }

    #[test]
    fn gregorian_rscale_is_held() {
        let gregorian = RecurrenceRule {
            rscale: Some("GREGORIAN".into()),
            ..rule(Frequency::Daily)
        };
        assert_eq!(losses(gregorian), Vec::new());
    }
}
