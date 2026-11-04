//! Recurrence: the rules, extra dates and excluded dates of a series.
//!
//! A [`RecurrenceRule`] mirrors JSCalendar's `RecurrenceRule` (RFC 8984 section 4.3.3), which is
//! itself a mapping of the iCalendar `RECUR` value (RFC 5545 section 3.3.10, RFC 7529). Values are
//! carried as written: range checks, and the meaning of an `rscale` this library cannot expand,
//! belong to the expander, which reports a [`Loss`](crate::Loss) rather than guessing.

use core::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::TimeValue;

/// How a series repeats.
///
/// The occurrences are the union of every rule's occurrences and every extra date, less every
/// excluded date and every exclusion rule's occurrences (RFC 8984 sections 4.3.3 and 4.3.4,
/// RFC 5545 section 3.8.5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recurrence {
    /// The rules (`RRULE`, JSCalendar `recurrenceRules`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RecurrenceRule>,
    /// Rules whose occurrences are removed (`EXRULE`, JSCalendar `excludedRecurrenceRules`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusion_rules: Vec<RecurrenceRule>,
    /// Extra occurrences (`RDATE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dates: Vec<TimeValue>,
    /// Removed occurrences (`EXDATE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_dates: Vec<TimeValue>,
}

impl Recurrence {
    /// A recurrence with a single rule.
    #[must_use]
    pub fn rule(rule: RecurrenceRule) -> Self {
        Self {
            rules: vec![rule],
            ..Self::default()
        }
    }
}

/// One repeating pattern: JSCalendar's `RecurrenceRule`, iCalendar's `RECUR`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceRule {
    /// The period each iteration covers (`FREQ`).
    pub frequency: Frequency,
    /// Iterations between occurrences (`INTERVAL`). At least 1.
    #[serde(default = "one")]
    pub interval: NonZeroU32,
    /// The calendar system (`RSCALE`), lowercase. `None` is Gregorian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rscale: Option<String>,
    /// What to do with an invalid date such as 30 February (`SKIP`).
    #[serde(default)]
    pub skip: Skip,
    /// The day a week starts on (`WKST`).
    #[serde(default)]
    pub first_day_of_week: Weekday,
    /// Days of the week (`BYDAY`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_day: Vec<NDay>,
    /// Days of the month, negative from the end (`BYMONTHDAY`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_month_day: Vec<i8>,
    /// Months of the year (`BYMONTH`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_month: Vec<RuleMonth>,
    /// Days of the year, negative from the end (`BYYEARDAY`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_year_day: Vec<i16>,
    /// Weeks of the year, negative from the end (`BYWEEKNO`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_week_no: Vec<i8>,
    /// Hours of the day (`BYHOUR`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_hour: Vec<u8>,
    /// Minutes of the hour (`BYMINUTE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_minute: Vec<u8>,
    /// Seconds of the minute (`BYSECOND`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_second: Vec<u8>,
    /// Positions within each iteration's set, negative from the end (`BYSETPOS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_set_position: Vec<i32>,
    /// When the rule stops (`COUNT`, `UNTIL`).
    #[serde(default)]
    pub end: RuleEnd,
}

/// The serde default for [`RecurrenceRule::interval`].
fn one() -> NonZeroU32 {
    NonZeroU32::MIN
}

impl RecurrenceRule {
    /// A rule repeating every period of `frequency`, forever, with every other part at its
    /// default.
    #[must_use]
    pub fn new(frequency: Frequency) -> Self {
        Self {
            frequency,
            interval: NonZeroU32::MIN,
            rscale: None,
            skip: Skip::default(),
            first_day_of_week: Weekday::default(),
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            by_year_day: Vec::new(),
            by_week_no: Vec::new(),
            by_hour: Vec::new(),
            by_minute: Vec::new(),
            by_second: Vec::new(),
            by_set_position: Vec::new(),
            end: RuleEnd::default(),
        }
    }

    /// Whether the rule uses the Gregorian calendar: no `rscale`, or `gregorian`.
    #[must_use]
    pub fn is_gregorian(&self) -> bool {
        self.rscale
            .as_deref()
            .is_none_or(|scale| scale.eq_ignore_ascii_case("gregorian"))
    }
}

/// The period each iteration of a rule covers (`FREQ`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    /// Every year.
    Yearly,
    /// Every month.
    Monthly,
    /// Every week.
    Weekly,
    /// Every day.
    Daily,
    /// Every hour.
    Hourly,
    /// Every minute.
    Minutely,
    /// Every second.
    Secondly,
}

/// What a rule does with a date that does not exist, such as 30 February (`SKIP`, RFC 7529).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Skip {
    /// Drop it.
    #[default]
    Omit,
    /// Move it to the previous valid date.
    Backward,
    /// Move it to the next valid date.
    Forward,
}

/// A day of the week, serialized as JSCalendar's two-letter lowercase names.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Weekday {
    /// Monday, `mo`.
    #[default]
    #[serde(rename = "mo")]
    Monday,
    /// Tuesday, `tu`.
    #[serde(rename = "tu")]
    Tuesday,
    /// Wednesday, `we`.
    #[serde(rename = "we")]
    Wednesday,
    /// Thursday, `th`.
    #[serde(rename = "th")]
    Thursday,
    /// Friday, `fr`.
    #[serde(rename = "fr")]
    Friday,
    /// Saturday, `sa`.
    #[serde(rename = "sa")]
    Saturday,
    /// Sunday, `su`.
    #[serde(rename = "su")]
    Sunday,
}

impl From<jiff::civil::Weekday> for Weekday {
    fn from(day: jiff::civil::Weekday) -> Self {
        match day {
            jiff::civil::Weekday::Monday => Self::Monday,
            jiff::civil::Weekday::Tuesday => Self::Tuesday,
            jiff::civil::Weekday::Wednesday => Self::Wednesday,
            jiff::civil::Weekday::Thursday => Self::Thursday,
            jiff::civil::Weekday::Friday => Self::Friday,
            jiff::civil::Weekday::Saturday => Self::Saturday,
            jiff::civil::Weekday::Sunday => Self::Sunday,
        }
    }
}

impl From<Weekday> for jiff::civil::Weekday {
    fn from(day: Weekday) -> Self {
        match day {
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
            Weekday::Sunday => Self::Sunday,
        }
    }
}

/// A `BYDAY` entry: a weekday, optionally only its nth occurrence in the period.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NDay {
    /// The day of the week.
    pub day: Weekday,
    /// Only the nth such day in the period, negative from the end. Never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth_of_period: Option<i32>,
}

impl NDay {
    /// Every such day in the period.
    #[must_use]
    pub fn every(day: Weekday) -> Self {
        Self {
            day,
            nth_of_period: None,
        }
    }

    /// Only the nth such day in the period.
    #[must_use]
    pub fn nth(day: Weekday, nth: i32) -> Self {
        Self {
            day,
            nth_of_period: Some(nth),
        }
    }
}

/// A `BYMONTH` entry: a month number in the rule's calendar, and whether it is the leap month
/// (RFC 7529's `L` suffix, as in `5L`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMonth {
    /// The month, from 1.
    pub month: u8,
    /// Whether this is the leap month that follows `month`.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub leap: bool,
}

impl RuleMonth {
    /// A month that is not a leap month.
    #[must_use]
    pub fn new(month: u8) -> Self {
        Self { month, leap: false }
    }
}

/// When a rule stops.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RuleEnd {
    /// It never stops.
    #[default]
    Never,
    /// After this many occurrences (`COUNT`).
    Count {
        /// The number of occurrences.
        count: u32,
    },
    /// At the last occurrence on or before this time (`UNTIL`).
    Until {
        /// The bound, inclusive.
        until: TimeValue,
    },
}
