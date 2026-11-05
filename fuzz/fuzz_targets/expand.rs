//! Expands an arbitrary series over an arbitrary window.
//!
//! Beyond not panicking, every expansion must hold its contract: occurrences in order, each
//! inside the window and not ending before it starts, and nothing at all beside a loss that
//! refuses the series.

#![no_main]

use std::num::NonZeroU32;

use arbitrary::Arbitrary;
use calendar_sync_model::{
    EventSeries, EventTimes, Frequency, InstanceKey, InstanceRange, LossReason, MapperVersion,
    NDay, NormalizedEvent, Override, Passthrough, Provider, ProviderEvent, ProviderIds, Recurrence,
    RecurrenceRule, RuleEnd, RuleMonth, Skip, TimeValue, TimeZoneId, Weekday,
};
use calendar_sync_recurrence::{Expander, Window};
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZoneDatabase;
use jiff::{SignedDuration, Timestamp};
use libfuzzer_sys::fuzz_target;

/// Zones with gaps, folds, half-hour offsets and none at all, and one no database holds.
const ZONES: &[&str] = &[
    "UTC",
    "America/New_York",
    "Europe/Berlin",
    "Australia/Lord_Howe",
    "Asia/Kolkata",
    "Pacific/Apia",
    "America/Santiago",
    "Nowhere/Unknown",
];

#[derive(Arbitrary, Debug)]
struct Civil {
    year: i16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Civil {
    fn date_time(&self) -> DateTime {
        let year = self.year.rem_euclid(9_999) + 1;
        let date = Date::new(year, (self.month % 12 + 1) as i8, (self.day % 28 + 1) as i8)
            .unwrap_or(Date::constant(2000, 1, 1));
        let time = Time::new(
            (self.hour % 24) as i8,
            (self.minute % 60) as i8,
            (self.second % 60) as i8,
            0,
        )
        .unwrap_or(Time::midnight());
        date.to_datetime(time)
    }
}

#[derive(Arbitrary, Debug)]
enum Form {
    Instant,
    Zoned(u8),
    Floating,
    AllDay,
}

impl Form {
    fn value(&self, at: DateTime) -> TimeValue {
        match self {
            Form::Instant => TimeValue::instant(
                jiff::tz::TimeZone::UTC
                    .to_ambiguous_timestamp(at)
                    .compatible()
                    .unwrap_or(Timestamp::UNIX_EPOCH),
            ),
            Form::Zoned(zone) => {
                TimeValue::zoned(at, TimeZoneId::new(ZONES[usize::from(*zone) % ZONES.len()]))
            }
            Form::Floating => TimeValue::floating(at),
            Form::AllDay => TimeValue::all_day(at.date()),
        }
    }
}

#[derive(Arbitrary, Debug)]
struct Rule {
    frequency: u8,
    interval: u16,
    rscale: Option<bool>,
    skip: u8,
    week_start: u8,
    by_day: Vec<(u8, Option<i8>)>,
    by_month_day: Vec<i8>,
    by_month: Vec<(u8, bool)>,
    by_year_day: Vec<i16>,
    by_week_no: Vec<i8>,
    by_hour: Vec<u8>,
    by_minute: Vec<u8>,
    by_second: Vec<u8>,
    by_set_position: Vec<i32>,
    end: Option<Result<u16, Civil>>,
}

fn weekday(n: u8) -> Weekday {
    [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ][usize::from(n % 7)]
}

impl Rule {
    fn model(&self, form: &Form) -> RecurrenceRule {
        let frequency = [
            Frequency::Yearly,
            Frequency::Monthly,
            Frequency::Weekly,
            Frequency::Daily,
            Frequency::Hourly,
            Frequency::Minutely,
            Frequency::Secondly,
        ][usize::from(self.frequency % 7)];
        let mut rule = RecurrenceRule::new(frequency);
        rule.interval = NonZeroU32::new(u32::from(self.interval)).unwrap_or(NonZeroU32::MIN);
        rule.rscale = self
            .rscale
            .map(|gregorian| if gregorian { "GREGORIAN" } else { "hebrew" }.to_owned());
        rule.skip = [Skip::Omit, Skip::Backward, Skip::Forward][usize::from(self.skip % 3)];
        rule.first_day_of_week = weekday(self.week_start);
        rule.by_day = self
            .by_day
            .iter()
            .take(7)
            .map(|&(day, nth)| NDay {
                day: weekday(day),
                nth_of_period: nth.map(i32::from),
            })
            .collect();
        rule.by_month_day = self.by_month_day.iter().copied().take(8).collect();
        rule.by_month = self
            .by_month
            .iter()
            .take(12)
            .map(|&(month, leap)| RuleMonth { month, leap })
            .collect();
        rule.by_year_day = self.by_year_day.iter().copied().take(8).collect();
        rule.by_week_no = self.by_week_no.iter().copied().take(8).collect();
        rule.by_hour = self.by_hour.iter().copied().take(24).collect();
        rule.by_minute = self.by_minute.iter().copied().take(60).collect();
        rule.by_second = self.by_second.iter().copied().take(61).collect();
        rule.by_set_position = self.by_set_position.iter().copied().take(8).collect();
        rule.end = match &self.end {
            None => RuleEnd::Never,
            Some(Ok(count)) => RuleEnd::Count {
                count: u32::from(*count),
            },
            Some(Err(until)) => RuleEnd::Until {
                until: form.value(until.date_time()),
            },
        };
        rule
    }
}

#[derive(Arbitrary, Debug)]
struct Change {
    key: Civil,
    key_form: Form,
    moved: Option<(Civil, u16, bool)>,
}

#[derive(Arbitrary, Debug)]
struct Input {
    form: Form,
    start: Civil,
    length_minutes: Option<u16>,
    rules: Vec<Rule>,
    exclusion_rules: Vec<Rule>,
    dates: Vec<Civil>,
    excluded_dates: Vec<Civil>,
    changes: Vec<Change>,
    no_master: bool,
    window_start: Civil,
    window_hours: u16,
    floating_zone: u8,
}

fn event(times: EventTimes) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(Provider::CalDav, "fuzz"),
        revision: None,
        mapper_version: MapperVersion::new(1),
        fields: NormalizedEvent::new(times),
        passthrough: Passthrough::new(),
    }
}

/// An event starting at `start` and lasting `minutes`, in `form`.
fn times(form: &Form, start: DateTime, minutes: Option<u16>) -> EventTimes {
    let begin = form.value(start);
    let end = minutes.and_then(|minutes| {
        let at = start
            .checked_add(SignedDuration::from_mins(i64::from(minutes)))
            .ok()?;
        let at = if matches!(form, Form::AllDay) {
            at.date().tomorrow().ok()?.to_datetime(Time::midnight())
        } else {
            at
        };
        Some(form.value(at))
    });
    EventTimes::new(begin.clone(), end).unwrap_or_else(|_| EventTimes::at(begin))
}

fn series(input: &Input) -> EventSeries {
    let start = input.start.date_time();
    let mut master = event(times(&input.form, start, input.length_minutes));
    master.fields.recurrence = Some(Recurrence {
        rules: input
            .rules
            .iter()
            .take(3)
            .map(|r| r.model(&input.form))
            .collect(),
        exclusion_rules: input
            .exclusion_rules
            .iter()
            .take(2)
            .map(|r| r.model(&input.form))
            .collect(),
        dates: input
            .dates
            .iter()
            .take(8)
            .map(|d| input.form.value(d.date_time()))
            .collect(),
        excluded_dates: input
            .excluded_dates
            .iter()
            .take(8)
            .map(|d| input.form.value(d.date_time()))
            .collect(),
    });
    let mut series = EventSeries::single(master);
    if input.no_master {
        series.master = None;
    }
    for change in input.changes.iter().take(6) {
        let key = InstanceKey::new(change.key_form.value(change.key.date_time()));
        let value = match &change.moved {
            None => Override::Cancelled {
                ids: None,
                revision: None,
            },
            Some((to, minutes, future)) => Override::Modified {
                event: Box::new(event(times(&input.form, to.date_time(), Some(*minutes)))),
                range: if *future {
                    InstanceRange::ThisAndFuture
                } else {
                    InstanceRange::ThisInstance
                },
            },
        };
        series.overrides.insert(key, value);
    }
    series
}

fuzz_target!(|input: Input| {
    let zones = TimeZoneDatabase::bundled();
    let floating = zones
        .get(ZONES[usize::from(input.floating_zone) % (ZONES.len() - 1)])
        .unwrap();
    let Ok(window_start) = jiff::tz::TimeZone::UTC
        .to_ambiguous_timestamp(input.window_start.date_time())
        .compatible()
    else {
        return;
    };
    let hours = i64::from(input.window_hours % (24 * 400)) + 1;
    let Ok(window_end) = window_start.checked_add(SignedDuration::from_hours(hours)) else {
        return;
    };
    let window = Window::new(window_start, window_end).unwrap();
    let series = series(&input);
    let expansion = Expander::new(&zones, floating).expand(&series, &window);

    let refused = expansion.losses.iter().any(|loss| {
        loss.detail.contains("the series is not shown") && loss.reason != LossReason::Approximated
    });
    if refused {
        assert!(expansion.occurrences.is_empty(), "{expansion:?}");
    }
    for pair in expansion.occurrences.windows(2) {
        assert!(pair[0].start <= pair[1].start, "out of order: {pair:?}");
    }
    for occurrence in &expansion.occurrences {
        assert!(occurrence.start <= occurrence.end, "{occurrence:?}");
        assert!(
            window.overlaps(occurrence.start, occurrence.end),
            "outside the window: {occurrence:?}"
        );
    }
});
