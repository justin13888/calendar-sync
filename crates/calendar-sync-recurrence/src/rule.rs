//! One recurrence rule, expanded in civil time.
//!
//! [`CivilRule`] reads a [`RecurrenceRule`] against a civil start and yields the civil date-times
//! the rule produces, in order. It knows nothing of time zones: [`crate::Expander`] places each
//! civil time in the series' zone.
//!
//! The expansion follows RFC 5545 section 3.3.10 as corrected by its errata
//! (`references/rfc/rfc5545-inline-errata.html`), and RFC 7529 for `SKIP`
//! (`references/rfc/rfc7529.txt`). Each period of the rule's frequency is expanded to the set of
//! days its `BYxxx` parts select, crossed with the times of day they select, then cut by
//! `BYSETPOS`. Where the RFC leaves a combination undefined the reading is python-dateutil's,
//! whose test cases this crate runs as data.

use calendar_sync_model::{
    Field, Frequency, Loss, LossReason, NDay, RecurrenceRule, RuleEnd, Skip, Weekday,
};
use jiff::SignedDuration;
use jiff::civil::{Date, DateTime, Time, Weekday as CivilWeekday};

/// The most periods one [`Instances`] iterator examines before it stops and reports
/// [`Instances::exhausted`].
///
/// A rule can select nothing for a very long run of periods (`FREQ=SECONDLY;BYMONTH=2` started in
/// March), and a `COUNT` rule cannot skip ahead to the window. The bound keeps every expansion
/// finite; a rule that reaches it is reported, never silently cut.
pub const PERIOD_BUDGET: u32 = 2_000_000;

const SECONDS_PER_DAY: i64 = 86_400;

/// A rule ready to expand from a civil start.
///
/// Built by [`CivilRule::new`], which checks the rule against RFC 5545 and refuses one it cannot
/// read. Expand it with [`CivilRule::instances`].
#[derive(Clone, Debug)]
pub struct CivilRule {
    frequency: Frequency,
    interval: i64,
    week_start: CivilWeekday,
    months: Vec<i8>,
    week_numbers: Vec<i8>,
    year_days: Vec<i16>,
    month_days: Vec<i8>,
    weekdays: Vec<CivilWeekday>,
    nth_weekdays: Vec<(i32, CivilWeekday)>,
    /// Empty means every hour; only for a frequency finer than daily.
    hours: Vec<i8>,
    /// Empty means every minute; only for a frequency finer than hourly.
    minutes: Vec<i8>,
    /// Empty means every second; only for `SECONDLY`.
    seconds: Vec<i8>,
    set_positions: Vec<i32>,
    skip: Skip,
    count: Option<u32>,
    until: Option<DateTime>,
    start: DateTime,
    start_counted: bool,
}

impl CivilRule {
    /// Reads `rule` for a series starting at `start`.
    ///
    /// `until` is the rule's `UNTIL` already placed in the civil time `start` is written in; the
    /// rule's own [`RuleEnd::Until`] is not read here, because placing it may need a time zone.
    /// The rule's [`RuleEnd::Count`] is read.
    ///
    /// Parts the rule leaves out are taken from `start`, as RFC 5545 section 3.3.10 says: a
    /// yearly rule with no `BYxxx` day part repeats on `start`'s month and day, and every
    /// frequency coarser than hourly keeps `start`'s time of day.
    ///
    /// # Errors
    ///
    /// A [`Loss`] of [`Field::Recurrence`]:
    ///
    /// - [`LossReason::Unsupported`] for an `RSCALE` other than Gregorian, and for
    ///   `BYSECOND=60` (a leap second has no civil time here);
    /// - [`LossReason::Malformed`] for a value out of its range, a part the frequency forbids
    ///   (RFC 5545 section 3.3.10: `BYWEEKNO` outside `YEARLY`, `BYYEARDAY` with `DAILY`, `WEEKLY`
    ///   or `MONTHLY`, `BYMONTHDAY` with `WEEKLY`, a numbered `BYDAY` outside `MONTHLY` and
    ///   `YEARLY` or beside `BYWEEKNO`), `BYSETPOS` with no other `BYxxx` part, a leap month,
    ///   and `COUNT=0`.
    pub fn new(
        rule: &RecurrenceRule,
        start: DateTime,
        until: Option<DateTime>,
    ) -> Result<Self, Loss> {
        if !rule.is_gregorian() {
            return Err(unsupported(format!(
                "RSCALE={} is not a calendar this library expands",
                rule.rscale.as_deref().unwrap_or_default()
            )));
        }
        validate(rule)?;

        let frequency = rule.frequency;
        let mut months: Vec<i8> = rule
            .by_month
            .iter()
            .filter_map(|m| i8::try_from(m.month).ok())
            .collect();
        let mut month_days = rule.by_month_day.clone();
        let mut weekdays = Vec::new();
        let mut nth_weekdays = Vec::new();
        for &NDay { day, nth_of_period } in &rule.by_day {
            let day = CivilWeekday::from(day);
            match nth_of_period {
                None => weekdays.push(day),
                Some(nth) => nth_weekdays.push((nth, day)),
            }
        }

        // RFC 5545 section 3.3.10: what the rule does not say comes from DTSTART.
        if rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty()
        {
            match frequency {
                Frequency::Yearly => {
                    if months.is_empty() {
                        months.push(start.month());
                    }
                    month_days.push(start.day());
                }
                Frequency::Monthly => month_days.push(start.day()),
                Frequency::Weekly => weekdays.push(start.weekday()),
                _ => {}
            }
        }
        let coarser = |than: Frequency| frequency < than;
        let hours = defaulted(&rule.by_hour, start.hour(), coarser(Frequency::Hourly));
        let minutes = defaulted(
            &rule.by_minute,
            start.minute(),
            coarser(Frequency::Minutely),
        );
        let seconds = defaulted(
            &rule.by_second,
            start.second(),
            coarser(Frequency::Secondly),
        );

        let mut set_positions = rule.by_set_position.clone();
        sort_dedup(&mut months);
        sort_dedup(&mut month_days);
        weekdays.sort_unstable_by_key(|day| day.to_monday_zero_offset());
        weekdays.dedup();
        nth_weekdays.sort_unstable_by_key(|&(nth, day)| (nth, day.to_monday_zero_offset()));
        nth_weekdays.dedup();
        sort_dedup(&mut set_positions);
        let mut week_numbers = rule.by_week_no.clone();
        sort_dedup(&mut week_numbers);
        let mut year_days = rule.by_year_day.clone();
        sort_dedup(&mut year_days);

        Ok(Self {
            frequency,
            interval: i64::from(rule.interval.get()),
            week_start: rule.first_day_of_week.into(),
            months,
            week_numbers,
            year_days,
            month_days,
            weekdays,
            nth_weekdays,
            hours,
            minutes,
            seconds,
            set_positions,
            skip: rule.skip,
            count: match rule.end {
                RuleEnd::Count { count } => Some(count),
                RuleEnd::Never | RuleEnd::Until { .. } => None,
            },
            until,
            start,
            start_counted: false,
        })
    }

    /// Counts `start` as the rule's first instance even where the rule does not produce it.
    ///
    /// RFC 5545 section 3.3.10 says of `COUNT` that "the DTSTART property value always counts as
    /// the first occurrence". Without this call the iterator counts only the instances the rule
    /// itself produces, as python-dateutil does; with it, a `start` the rule does not produce
    /// takes one from the count.
    #[must_use]
    pub fn counting_start(mut self) -> Self {
        self.start_counted = true;
        self
    }

    /// The civil start the rule expands from.
    #[must_use]
    pub fn start(&self) -> DateTime {
        self.start
    }

    /// Whether the rule ends after a number of instances (`COUNT`). Such a rule is always
    /// expanded from its start, since the count runs from there.
    #[must_use]
    pub fn is_counted(&self) -> bool {
        self.count.is_some()
    }

    /// The instances, in increasing order, with no duplicates, none before the start and none
    /// after `UNTIL`, until the first period that begins after `horizon`.
    ///
    /// Where `from` is given and the rule has no `COUNT`, whole periods ending before `from` are
    /// skipped without being examined. Some instances before `from` may still be yielded; every
    /// instance at or after it is.
    #[must_use]
    pub fn instances(&self, from: Option<DateTime>, horizon: DateTime) -> Instances<'_> {
        let mut period = 0;
        if let (Some(from), None) = (from, self.count)
            && let Some(index) = self.period_index_of(from)
        {
            // One interval of slack: a SKIP=FORWARD date or a week-numbering year can spill
            // past its period's end.
            let aligned = index.div_euclid(self.interval) * self.interval - self.interval;
            period = aligned.max(0);
        }
        Instances {
            rule: self,
            period,
            horizon,
            pending: PeriodSet::default(),
            next_pending: 0,
            remaining: self.count,
            last: None,
            first_seen: false,
            examined: 0,
            done: false,
            exhausted: false,
        }
    }

    /// The index, in frequency units from the start's period, of the period containing `at`.
    fn period_index_of(&self, at: DateTime) -> Option<i64> {
        let start = self.start;
        Some(match self.frequency {
            Frequency::Yearly => i64::from(at.year()) - i64::from(start.year()),
            Frequency::Monthly => month_number(at.date()) - month_number(start.date()),
            Frequency::Weekly => {
                let from = week_start(at.date(), self.week_start)?;
                let origin = week_start(start.date(), self.week_start)?;
                days_between(origin, from).div_euclid(7)
            }
            Frequency::Daily => days_between(start.date(), at.date()),
            Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
                let unit = self.unit_seconds();
                let origin = self.truncate(start);
                at.duration_since(origin).as_secs().div_euclid(unit)
            }
        })
    }

    /// The length in seconds of one period of a frequency finer than daily.
    fn unit_seconds(&self) -> i64 {
        match self.frequency {
            Frequency::Hourly => 3_600,
            Frequency::Minutely => 60,
            _ => 1,
        }
    }

    /// `at` with the parts finer than the frequency cleared, for a frequency finer than daily.
    fn truncate(&self, at: DateTime) -> DateTime {
        let time = at.time();
        let (minute, second) = match self.frequency {
            Frequency::Hourly => (0, 0),
            Frequency::Minutely => (time.minute(), 0),
            _ => (time.minute(), time.second()),
        };
        at.date()
            .to_datetime(Time::constant(time.hour(), minute, second, 0))
    }

    /// The first day or instant of period `n`, or `None` beyond the civil range.
    fn period_start(&self, n: i64) -> Option<DateTime> {
        let midnight = |date: Date| Some(date.to_datetime(Time::midnight()));
        match self.frequency {
            Frequency::Yearly => {
                let year = i16::try_from(i64::from(self.start.year()).checked_add(n)?).ok()?;
                if self.week_numbers.is_empty() {
                    midnight(Date::new(year, 1, 1).ok()?)
                } else {
                    midnight(week_one(year, self.week_start)?)
                }
            }
            Frequency::Monthly => midnight(month_from_number(
                month_number(self.start.date()).checked_add(n)?,
            )?),
            Frequency::Weekly => {
                let origin = week_start(self.start.date(), self.week_start)?;
                midnight(add_days(origin, n.checked_mul(7)?)?)
            }
            Frequency::Daily => midnight(add_days(self.start.date(), n)?),
            Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
                let seconds = n.checked_mul(self.unit_seconds())?;
                self.truncate(self.start)
                    .checked_add(SignedDuration::from_secs(seconds))
                    .ok()
            }
        }
    }

    /// The days period `n` holds, before `BYxxx` filtering.
    fn period_days(&self, n: i64, period_start: DateTime) -> Option<Vec<Date>> {
        let first = period_start.date();
        let length = match self.frequency {
            Frequency::Yearly => {
                if self.week_numbers.is_empty() {
                    i64::from(first.days_in_year())
                } else {
                    let year = i16::try_from(i64::from(self.start.year()).checked_add(n)?).ok()?;
                    days_between(first, week_one(year.checked_add(1)?, self.week_start)?)
                }
            }
            Frequency::Monthly => i64::from(first.days_in_month()),
            Frequency::Weekly => 7,
            _ => 1,
        };
        (0..length).map(|offset| add_days(first, offset)).collect()
    }

    /// Whether `day` passes every day-level `BYxxx` part. `week_year` is the period's
    /// week-numbering year, for `BYWEEKNO`.
    fn day_matches(&self, day: Date, week_year: Option<i16>) -> bool {
        if !self.months.is_empty() && !self.months.contains(&day.month()) {
            return false;
        }
        if !self.month_days.is_empty() {
            let length = day.days_in_month();
            let matches = self
                .month_days
                .iter()
                .any(|&v| v == day.day() || (v < 0 && length + 1 + v == day.day()));
            if !matches {
                return false;
            }
        }
        self.day_matches_after_month_day(day, week_year)
    }

    /// The day-level parts RFC 7529 applies after `BYMONTHDAY` and its `SKIP`, together with
    /// those before it that a moved date is still held to.
    fn day_matches_after_month_day(&self, day: Date, week_year: Option<i16>) -> bool {
        if !self.week_numbers.is_empty() {
            let Some(year) = week_year else { return false };
            let Some(number) = week_number(day, year, self.week_start) else {
                return false;
            };
            if !self.week_numbers.contains(&number.0) && !self.week_numbers.contains(&number.1) {
                return false;
            }
        }
        if !self.year_days.is_empty() {
            let length = day.days_in_year();
            let matches = self
                .year_days
                .iter()
                .any(|&v| v == day.day_of_year() || (v < 0 && length + 1 + v == day.day_of_year()));
            if !matches {
                return false;
            }
        }
        if self.weekdays.is_empty() && self.nth_weekdays.is_empty() {
            return true;
        }
        let weekday = day.weekday();
        if self.weekdays.contains(&weekday) {
            return true;
        }
        let by_month = self.frequency == Frequency::Monthly
            || (self.frequency == Frequency::Yearly && !self.months.is_empty());
        let (position, length) = if by_month {
            (i32::from(day.day()), i32::from(day.days_in_month()))
        } else {
            (i32::from(day.day_of_year()), i32::from(day.days_in_year()))
        };
        let from_start = (position - 1) / 7 + 1;
        let from_end = -((length - position) / 7 + 1);
        self.nth_weekdays
            .iter()
            .any(|&(nth, day)| day == weekday && (nth == from_start || nth == from_end))
    }

    /// The days RFC 7529's `SKIP` moves into period `n` from a `BYMONTHDAY` past a month's end.
    fn skipped_days(&self, period_start: DateTime, week_year: Option<i16>) -> Vec<Date> {
        let mut moved = Vec::new();
        if self.skip == Skip::Omit
            || !matches!(self.frequency, Frequency::Yearly | Frequency::Monthly)
        {
            return moved;
        }
        let first = period_start.date();
        let months: Vec<i8> = match self.frequency {
            Frequency::Monthly => vec![first.month()],
            _ => (1..=12).collect(),
        };
        // A week-numbering year starts in late December: its months are the next year's.
        let year = if first.month() == 12 && first.day() > 1 {
            first.year().checked_add(1)
        } else {
            Some(first.year())
        };
        let Some(year) = year else { return moved };
        for month in months {
            if !self.months.is_empty() && !self.months.contains(&month) {
                continue;
            }
            let Ok(first_of_month) = Date::new(year, month, 1) else {
                continue;
            };
            let length = first_of_month.days_in_month();
            if !self.month_days.iter().any(|&v| v > length) {
                continue;
            }
            let day = match self.skip {
                Skip::Backward => Some(first_of_month.last_of_month()),
                Skip::Forward => first_of_month.last_of_month().tomorrow().ok(),
                Skip::Omit => None,
            };
            if let Some(day) = day
                && self.day_matches_after_month_day(day, week_year)
            {
                moved.push(day);
            }
        }
        moved
    }

    /// The times of day the rule selects on each day of a period that begins at `period_start`.
    /// For a frequency finer than daily whose period's own hour, minute or second is not
    /// selected, `Err` holds the first later instant that might be.
    fn period_times(&self, period_start: DateTime) -> Result<Vec<Time>, DateTime> {
        let subsec = self.start.subsec_nanosecond();
        let hour = period_start.hour();
        let minute = period_start.minute();
        let second = period_start.second();
        let (hours, minutes, seconds) = match self.frequency {
            Frequency::Hourly => {
                if !self.hours.is_empty() && !self.hours.contains(&hour) {
                    return Err(next_unit(period_start, 3_600));
                }
                (vec![hour], self.minutes.clone(), self.seconds.clone())
            }
            Frequency::Minutely => {
                if !self.hours.is_empty() && !self.hours.contains(&hour) {
                    return Err(next_unit(period_start, 3_600));
                }
                if !self.minutes.is_empty() && !self.minutes.contains(&minute) {
                    return Err(next_unit(period_start, 60));
                }
                (vec![hour], vec![minute], self.seconds.clone())
            }
            Frequency::Secondly => {
                if !self.hours.is_empty() && !self.hours.contains(&hour) {
                    return Err(next_unit(period_start, 3_600));
                }
                if !self.minutes.is_empty() && !self.minutes.contains(&minute) {
                    return Err(next_unit(period_start, 60));
                }
                if !self.seconds.is_empty() && !self.seconds.contains(&second) {
                    return Err(next_unit(period_start, 1));
                }
                (vec![hour], vec![minute], vec![second])
            }
            _ => (
                self.hours.clone(),
                self.minutes.clone(),
                self.seconds.clone(),
            ),
        };
        let mut times = Vec::with_capacity(hours.len() * minutes.len() * seconds.len());
        for &h in &hours {
            for &m in &minutes {
                for &s in &seconds {
                    if let Ok(time) = Time::new(h, m, s, subsec) {
                        times.push(time);
                    }
                }
            }
        }
        Ok(times)
    }

    /// The instances period `n` selects, sorted, before the start, `UNTIL` and `COUNT` apply.
    fn expand_period(&self, n: i64, period_start: DateTime) -> Result<PeriodSet, Skipped> {
        let week_year = if self.week_numbers.is_empty() {
            None
        } else {
            let year = i64::from(self.start.year()).checked_add(n);
            Some(
                year.and_then(|y| i16::try_from(y).ok())
                    .ok_or(Skipped::End)?,
            )
        };
        let times = self.period_times(period_start).map_err(Skipped::Until)?;
        let mut days: Vec<Date> = self
            .period_days(n, period_start)
            .ok_or(Skipped::End)?
            .into_iter()
            .filter(|&day| self.day_matches(day, week_year))
            .collect();
        if days.is_empty() && self.frequency > Frequency::Daily {
            let next_day = period_start.date().tomorrow().map_err(|_| Skipped::End)?;
            return Err(Skipped::Until(next_day.to_datetime(Time::midnight())));
        }
        days.extend(self.skipped_days(period_start, week_year));
        days.sort_unstable();
        days.dedup();

        let mut set = PeriodSet {
            days,
            times,
            chosen: None,
        };
        if !self.set_positions.is_empty() {
            let length = i64::try_from(set.len()).unwrap_or(i64::MAX);
            let mut chosen: Vec<DateTime> = self
                .set_positions
                .iter()
                .filter_map(|&position| {
                    let position = i64::from(position);
                    let index = if position > 0 {
                        position - 1
                    } else {
                        length + position
                    };
                    usize::try_from(index)
                        .ok()
                        .filter(|&i| i < set.len())
                        .map(|i| set.get(i))
                })
                .collect();
            chosen.sort_unstable();
            chosen.dedup();
            set.chosen = Some(chosen);
        }
        Ok(set)
    }
}

/// The sorted instances one period selects: every selected day at every selected time, or the
/// positions `BYSETPOS` chose from them. The product is indexed, never built, since a rule with
/// every hour, minute and second selected has tens of millions of instances in a year.
#[derive(Debug, Default)]
struct PeriodSet {
    days: Vec<Date>,
    times: Vec<Time>,
    chosen: Option<Vec<DateTime>>,
}

impl PeriodSet {
    fn len(&self) -> usize {
        match &self.chosen {
            Some(chosen) => chosen.len(),
            None => self.days.len() * self.times.len(),
        }
    }

    /// The instance at `index`, which must be below [`PeriodSet::len`].
    fn get(&self, index: usize) -> DateTime {
        match &self.chosen {
            Some(chosen) => chosen[index],
            None => {
                let per_day = self.times.len();
                self.days[index / per_day].to_datetime(self.times[index % per_day])
            }
        }
    }

    /// The first index whose instance is not below `bound` (or not at or below it, where
    /// `inclusive`).
    fn first_after(&self, bound: DateTime, inclusive: bool) -> usize {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let at = self.get(middle);
            if at < bound || (inclusive && at == bound) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }
}

/// Why a period yielded nothing.
enum Skipped {
    /// The civil range ended.
    End,
    /// Nothing before this instant can be selected.
    Until(DateTime),
}

/// The instances of a [`CivilRule`], from [`CivilRule::instances`].
#[derive(Debug)]
pub struct Instances<'r> {
    rule: &'r CivilRule,
    period: i64,
    horizon: DateTime,
    pending: PeriodSet,
    next_pending: usize,
    remaining: Option<u32>,
    last: Option<DateTime>,
    first_seen: bool,
    examined: u32,
    done: bool,
    exhausted: bool,
}

impl Instances<'_> {
    /// Whether the iterator stopped at [`PERIOD_BUDGET`] rather than at the horizon, `UNTIL`,
    /// `COUNT` or the end of the civil range. Instances after the last one yielded were not
    /// expanded.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Moves to the first period starting at or after `at`, for a frequency finer than daily.
    fn advance_to(&mut self, at: DateTime, period_start: DateTime) {
        let step = self.rule.unit_seconds().saturating_mul(self.rule.interval);
        let gap = at.duration_since(period_start).as_secs().max(1);
        let periods = gap.div_euclid(step) + i64::from(gap.rem_euclid(step) != 0);
        match periods
            .checked_mul(self.rule.interval)
            .and_then(|units| self.period.checked_add(units))
        {
            Some(period) => self.period = period,
            None => self.done = true,
        }
    }

    /// Fills `pending` from the next period that selects anything.
    fn refill(&mut self) {
        while !self.done {
            if self.examined >= PERIOD_BUDGET {
                self.exhausted = true;
                self.done = true;
                return;
            }
            self.examined += 1;
            let Some(period_start) = self.rule.period_start(self.period) else {
                self.done = true;
                return;
            };
            if period_start > self.horizon {
                self.done = true;
                return;
            }
            if self.rule.until.is_some_and(|until| period_start > until) {
                self.done = true;
                return;
            }
            match self.rule.expand_period(self.period, period_start) {
                Ok(set) => {
                    match self.period.checked_add(self.rule.interval) {
                        Some(next) => self.period = next,
                        None => self.done = true,
                    }
                    if set.len() > 0 {
                        // Skip what is before the start or already yielded without visiting it.
                        self.next_pending = match self.last {
                            Some(last) => set.first_after(last, true),
                            None => set.first_after(self.rule.start, false),
                        };
                        self.pending = set;
                        return;
                    }
                }
                Err(Skipped::End) => self.done = true,
                Err(Skipped::Until(at)) => self.advance_to(at, period_start),
            }
        }
    }
}

impl Iterator for Instances<'_> {
    type Item = DateTime;

    fn next(&mut self) -> Option<DateTime> {
        loop {
            if self.remaining == Some(0) {
                return None;
            }
            if self.next_pending >= self.pending.len() {
                if self.done {
                    return None;
                }
                self.refill();
                continue;
            }
            let candidate = self.pending.get(self.next_pending);
            self.next_pending += 1;
            if self.rule.until.is_some_and(|until| candidate > until) {
                self.done = true;
                self.pending = PeriodSet::default();
                return None;
            }
            if !self.first_seen {
                self.first_seen = true;
                if self.rule.start_counted
                    && candidate != self.rule.start
                    && let Some(remaining) = &mut self.remaining
                {
                    *remaining = remaining.saturating_sub(1);
                    if *remaining == 0 {
                        return None;
                    }
                }
            }
            if let Some(remaining) = &mut self.remaining {
                *remaining -= 1;
            }
            self.last = Some(candidate);
            return Some(candidate);
        }
    }
}

/// Checks the rule's parts against RFC 5545 section 3.3.10.
fn validate(rule: &RecurrenceRule) -> Result<(), Loss> {
    let frequency = rule.frequency;
    let name = frequency_name(frequency);
    if let RuleEnd::Count { count: 0 } = rule.end {
        return Err(malformed("COUNT=0 selects no instance"));
    }
    if let Some(month) = rule.by_month.iter().find(|m| m.leap) {
        return Err(malformed(format!(
            "BYMONTH={}L names a leap month, which the Gregorian calendar has none of",
            month.month
        )));
    }
    if let Some(month) = rule.by_month.iter().find(|m| !(1..=12).contains(&m.month)) {
        return Err(malformed(format!(
            "BYMONTH={} is out of range",
            month.month
        )));
    }
    if let Some(day) = rule
        .by_month_day
        .iter()
        .find(|&&d| d == 0 || !(-31..=31).contains(&d))
    {
        return Err(malformed(format!("BYMONTHDAY={day} is out of range")));
    }
    if let Some(day) = rule
        .by_year_day
        .iter()
        .find(|&&d| d == 0 || !(-366..=366).contains(&d))
    {
        return Err(malformed(format!("BYYEARDAY={day} is out of range")));
    }
    if let Some(week) = rule
        .by_week_no
        .iter()
        .find(|&&w| w == 0 || !(-53..=53).contains(&w))
    {
        return Err(malformed(format!("BYWEEKNO={week} is out of range")));
    }
    if let Some(hour) = rule.by_hour.iter().find(|&&h| h > 23) {
        return Err(malformed(format!("BYHOUR={hour} is out of range")));
    }
    if let Some(minute) = rule.by_minute.iter().find(|&&m| m > 59) {
        return Err(malformed(format!("BYMINUTE={minute} is out of range")));
    }
    if rule.by_second.contains(&60) {
        return Err(unsupported(
            "BYSECOND=60 names a leap second, which has no civil time here",
        ));
    }
    if let Some(second) = rule.by_second.iter().find(|&&s| s > 60) {
        return Err(malformed(format!("BYSECOND={second} is out of range")));
    }
    if let Some(position) = rule
        .by_set_position
        .iter()
        .find(|&&p| p == 0 || !(-366..=366).contains(&p))
    {
        return Err(malformed(format!("BYSETPOS={position} is out of range")));
    }
    for nday in &rule.by_day {
        if let Some(nth) = nday.nth_of_period {
            if nth == 0 || !(-53..=53).contains(&nth) {
                return Err(malformed(format!(
                    "BYDAY={nth}{} is out of range",
                    weekday_name(nday.day)
                )));
            }
            if !matches!(frequency, Frequency::Monthly | Frequency::Yearly) {
                return Err(malformed(format!(
                    "BYDAY={nth}{} is numbered, which FREQ={name} does not allow",
                    weekday_name(nday.day)
                )));
            }
            if !rule.by_week_no.is_empty() {
                return Err(malformed(format!(
                    "BYDAY={nth}{} is numbered beside BYWEEKNO",
                    weekday_name(nday.day)
                )));
            }
        }
    }
    if !rule.by_week_no.is_empty() && frequency != Frequency::Yearly {
        return Err(malformed(format!(
            "BYWEEKNO is only valid with FREQ=YEARLY, not FREQ={name}"
        )));
    }
    if !rule.by_year_day.is_empty()
        && matches!(
            frequency,
            Frequency::Daily | Frequency::Weekly | Frequency::Monthly
        )
    {
        return Err(malformed(format!(
            "BYYEARDAY is not valid with FREQ={name}"
        )));
    }
    if !rule.by_month_day.is_empty() && frequency == Frequency::Weekly {
        return Err(malformed("BYMONTHDAY is not valid with FREQ=WEEKLY"));
    }
    let other_parts = !rule.by_month.is_empty()
        || !rule.by_week_no.is_empty()
        || !rule.by_year_day.is_empty()
        || !rule.by_month_day.is_empty()
        || !rule.by_day.is_empty()
        || !rule.by_hour.is_empty()
        || !rule.by_minute.is_empty()
        || !rule.by_second.is_empty();
    if !rule.by_set_position.is_empty() && !other_parts {
        return Err(malformed("BYSETPOS is used with no other BYxxx part"));
    }
    Ok(())
}

fn malformed(detail: impl Into<String>) -> Loss {
    Loss::new(Field::Recurrence, LossReason::Malformed, detail)
}

fn unsupported(detail: impl Into<String>) -> Loss {
    Loss::new(Field::Recurrence, LossReason::Unsupported, detail)
}

/// A `BYHOUR`, `BYMINUTE` or `BYSECOND` list, or the start's value where the list is empty and
/// the frequency is coarser than the part. Empty means every value.
fn defaulted(values: &[u8], start: i8, coarser: bool) -> Vec<i8> {
    let mut values: Vec<i8> = values
        .iter()
        .filter_map(|&v| i8::try_from(v).ok())
        .collect();
    if values.is_empty() && coarser {
        values.push(start);
    }
    sort_dedup(&mut values);
    values
}

fn sort_dedup<T: Ord>(values: &mut Vec<T>) {
    values.sort_unstable();
    values.dedup();
}

/// Months since year 0, so that consecutive months differ by one.
fn month_number(date: Date) -> i64 {
    i64::from(date.year()) * 12 + i64::from(date.month()) - 1
}

/// The first of the month [`month_number`] numbers.
fn month_from_number(number: i64) -> Option<Date> {
    let year = i16::try_from(number.div_euclid(12)).ok()?;
    let month = i8::try_from(number.rem_euclid(12) + 1).ok()?;
    Date::new(year, month, 1).ok()
}

fn days_between(from: Date, to: Date) -> i64 {
    to.duration_since(from)
        .as_secs()
        .div_euclid(SECONDS_PER_DAY)
}

fn add_days(date: Date, days: i64) -> Option<Date> {
    let seconds = days.checked_mul(SECONDS_PER_DAY)?;
    date.checked_add(SignedDuration::from_secs(seconds)).ok()
}

/// The day the week holding `date` starts on, for weeks starting on `week_start`.
fn week_start(date: Date, week_start: CivilWeekday) -> Option<Date> {
    let back = date.weekday().since(week_start);
    add_days(date, -i64::from(back))
}

/// The first day of week 1 of `year`: the first week, starting on `week_start`, that holds at
/// least four days of the year (RFC 5545 section 3.3.10, `BYWEEKNO`).
fn week_one(year: i16, week_start_day: CivilWeekday) -> Option<Date> {
    let january_first = Date::new(year, 1, 1).ok()?;
    let start = week_start(january_first, week_start_day)?;
    let days_in_year = 7 - days_between(start, january_first);
    if days_in_year >= 4 {
        Some(start)
    } else {
        add_days(start, 7)
    }
}

/// The week number of `date` within week-numbering `year`, counted from the start (from 1) and
/// from the end (from -1), or `None` where `date` is outside that year's weeks.
fn week_number(date: Date, year: i16, week_start_day: CivilWeekday) -> Option<(i8, i8)> {
    let first = week_one(year, week_start_day)?;
    let next = week_one(year.checked_add(1)?, week_start_day)?;
    if date < first || date >= next {
        return None;
    }
    let weeks = days_between(first, next) / 7;
    let index = days_between(first, date) / 7;
    let from_start = i8::try_from(index + 1).ok()?;
    let from_end = i8::try_from(index - weeks).ok()?;
    Some((from_start, from_end))
}

/// The start of the next `unit`-second boundary after `at`.
fn next_unit(at: DateTime, unit: i64) -> DateTime {
    let into_day = i64::from(at.hour()) * 3_600 + i64::from(at.minute()) * 60;
    let into_day = into_day + i64::from(at.second());
    let boundary = (into_day.div_euclid(unit) + 1) * unit;
    at.date()
        .to_datetime(Time::midnight())
        .checked_add(SignedDuration::from_secs(boundary))
        .unwrap_or(DateTime::MAX)
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
