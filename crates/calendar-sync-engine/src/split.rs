//! Series splits: "this and following" planned as writes that any device can finish.
//!
//! Neither Google nor Graph splits a series for us, so a change to one instance and every later
//! one is two series: the old one, ended just before the instance, and a new one from it. The
//! planner writes them in this order:
//!
//! 1. **Create** the new series' master, under ids derived from the [`Split`]. The old series'
//!    cancelled instances and excluded dates at or after the split are excluded dates of the new
//!    master from the start, so no cancelled instance shows again in between.
//! 2. **Carry** each modified override at or after the split to the new series, one at a time.
//! 3. **Trim** the old master: every rule ends before the split, and its extra dates at or after
//!    the split are dropped.
//! 4. **Remove** each override at or after the split from the old series, one at a time.
//!
//! Until step 3, every occurrence at or after the split shows in both series: a crash leaves a
//! visible duplicate, never a lost occurrence, and never a cancelled one shown again. Between a
//! create and the carry of a modified instance, the new series shows that instance unmodified
//! beside the old series' modified one. Nothing is removed from the old series until the
//! new one holds everything carried to it.
//!
//! # Recovering
//!
//! [`Splitter::next`] reads only the two series as the provider holds them now, and returns the
//! one write that comes next. It keeps no state between calls, so after a crash, a lost response
//! or a repeated write, calling it again on what the provider holds continues where the split
//! stopped. Repeating a write it already made is harmless: the next call sees it done.
//!
//! The device that started the split holds the new series' fields (the *tail*). Any other device
//! does not, and finds the split from the new series alone: [`Split::correlate`] reads it back
//! from the new master's ids, which are derived from the [`Split`]. Such a device can finish a
//! split that was started, and never starts one. Before the create, there is nothing to finish.
//!
//! # Carrying overrides forward
//!
//! An override keeps its own fields. Where the new series starts at another time than the
//! instance it was split at, every carried key moves by the same amount, and so does an
//! override's own time where it had not been moved from its instance: civil time where the key is
//! in the split instance's frame, elapsed time otherwise. Extra dates, excluded dates and cancelled
//! instances at or after the split move the same way into the new series' recurrence, where it
//! has one. An override that
//! cannot be moved, because an all-day series became a timed one or the reverse, is not carried,
//! and [`SplitStep::Create`] names it, so the application can report it.
//!
//! ```
//! use calendar_sync_engine::split::Split;
//! use calendar_sync_engine::{EditId, ResourceId};
//! use calendar_sync_model::{InstanceKey, TimeValue};
//!
//! let at = InstanceKey::new(TimeValue::instant("2026-03-02T15:00:00Z".parse().unwrap()));
//! let split = Split::new(EditId::new("e1"), ResourceId::new("weekly"), at);
//! // The new series' id names the split, so any device can tell where it came from.
//! let id = split.ids().google_id();
//! assert_eq!(Split::from_key(id.as_str()), Some(split));
//! ```

use calendar_sync_model::{
    EventSeries, EventTimes, InstanceKey, InstanceRange, NormalizedEvent, Override, Passthrough,
    ProviderEvent, ProviderIds, ProviderRevision, Recurrence, RecurrenceRule, RuleEnd, TimeValue,
    TimeZoneId,
};
use calendar_sync_recurrence::{CivilRule, ZoneSource};
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::ids::{DerivedIds, GOOGLE_ID_MAX, PREFIX};
use crate::seam::ResourceId;
use crate::sync::EditId;

/// What the derived edit id of a split's create starts with, before its fields.
const TAG: &str = "split1:";

/// The suffix [`DerivedIds::ical_uid`] adds to a key.
const UID_SUFFIX: &str = "@calendar-sync";

/// One split: the edit that asked for it, the series it splits, and the instance it splits at.
///
/// Everything a device needs to finish the split is derivable from it, and it is derivable from
/// the new series' ids, so it is never stored apart from the provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Split {
    /// The user's edit.
    pub edit: EditId,
    /// The old series' master resource.
    pub series: ResourceId,
    /// The first instance the new series takes over.
    pub at: InstanceKey,
}

impl Split {
    /// The split of `series` at `at`, asked for by `edit`.
    #[must_use]
    pub fn new(edit: EditId, series: ResourceId, at: InstanceKey) -> Self {
        Self { edit, series, at }
    }

    /// The edit id the new series is created under. It encodes the whole split, so the ids
    /// derived from it ([`Split::ids`]) name it, and two splits never share them. Google accepts
    /// the derived id while this is at most 636 bytes: the series id, the edit id and the
    /// instance together, with about 40 bytes besides.
    ///
    /// The series id is kept whole, so a split of a series that was itself made by a split
    /// derives a key about 1.6 times as long as that series' own. [`Splitter::next`] refuses a
    /// create whose key would be longer than [`GOOGLE_ID_MAX`] with [`SplitError::TooLong`].
    #[must_use]
    pub fn create_edit(&self) -> EditId {
        let mut text = String::from(TAG);
        for field in [
            self.series.as_str(),
            self.edit.as_str(),
            &instance_text(&self.at),
        ] {
            text.push_str(&field.len().to_string());
            text.push(':');
            text.push_str(field);
        }
        EditId::new(text)
    }

    /// The new series' ids: [`DerivedIds`] of [`Split::create_edit`].
    #[must_use]
    pub fn ids(&self) -> DerivedIds {
        DerivedIds::of(&self.create_edit())
    }

    /// The split whose new series has the derived key `key`, as [`DerivedIds::key`], the Google
    /// id or the Graph idempotency key spell it. `None` for any other string, including the key of
    /// an ordinary create.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        let text = decode(key.strip_prefix(PREFIX)?)?;
        let mut rest = text.strip_prefix(TAG)?;
        let mut fields = [""; 3];
        for field in &mut fields {
            let (length, tail) = rest.split_once(':')?;
            let length: usize = length.parse().ok()?;
            *field = tail.get(..length)?;
            rest = &tail[length..];
        }
        if !rest.is_empty() {
            return None;
        }
        let [series, edit, at] = fields;
        let split = Self::new(
            EditId::new(edit),
            ResourceId::new(series),
            parse_instance(at)?,
        );
        // Only the canonical spelling is accepted, so a key names at most one split.
        (split.ids().key() == key).then_some(split)
    }

    /// The split `event` is the new series of, read from its ids without the provider's help:
    /// its id (a Google id, or a `CalDAV` href ending in `KEY.ics`), or its iCalendar `UID`
    /// (`KEY@calendar-sync`). `None` where `event` was not created by a split.
    ///
    /// A provider that assigns both its own id and the `UID` (Graph) keeps the key only in the
    /// extended property its seam creates under; its seam reads it back and passes it to
    /// [`Split::from_key`].
    #[must_use]
    pub fn correlate(event: &ProviderEvent) -> Option<Self> {
        let id = event.ids.id.as_str();
        let segment = id.rsplit('/').next().unwrap_or(id);
        let href = segment.strip_suffix(".ics");
        let uid = event
            .ids
            .ical_uid
            .as_deref()
            .and_then(|uid| uid.strip_suffix(UID_SUFFIX));
        [Some(id), href, uid]
            .into_iter()
            .flatten()
            .find_map(Self::from_key)
    }
}

/// The next write of a split, from [`Splitter::next`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a step is handed to the application once, never stored"
)]
pub enum SplitStep {
    /// Create the new series' master. A create like any other: under [`DerivedIds`], so a
    /// repeat from any device names the same resource.
    Create {
        /// The edit id to create under: [`Split::create_edit`].
        edit: EditId,
        /// The ids to create under.
        ids: DerivedIds,
        /// The master. Its fields are the tail's, with the old series' extra and excluded
        /// dates at or after the split carried into its recurrence.
        master: ProviderEvent,
        /// The overrides at or after the split that cannot be carried, because the series
        /// changed between all-day and timed. They end with the old series.
        uncarried: Vec<InstanceKey>,
    },
    /// Add a modified instance to the new series.
    Carry {
        /// The new series' master resource.
        series: ResourceId,
        /// The instance it overrides in the new series.
        instance: InstanceKey,
        /// The instance's fields. Its ids name the new series, and it carries no revision.
        event: ProviderEvent,
        /// Which instances it applies to, as in the old series.
        range: InstanceRange,
    },
    /// Replace the old master's fields, conditional on its revision.
    Trim {
        /// The old series' master resource.
        series: ResourceId,
        /// The revision the trim was planned from.
        if_match: Option<ProviderRevision>,
        /// The fields as read.
        base: NormalizedEvent,
        /// The fields with every rule ended before the split.
        ours: NormalizedEvent,
    },
    /// Remove an override from the old series, now that it ends before it.
    Remove {
        /// The old series' master resource.
        series: ResourceId,
        /// The instance.
        instance: InstanceKey,
        /// The override as the old series holds it, with its own ids and revision.
        value: Override,
    },
    /// Nothing to write: the split is complete, was never started, or its old series is gone.
    /// Also where the old series has no instance at or after the split and no new series
    /// exists, even with a tail: the tail is not created.
    Done,
}

/// Why a split cannot be planned.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SplitError {
    /// The old series does not recur.
    NotRecurring,
    /// A time the split needs names a zone the [`ZoneSource`] does not know, or does not exist.
    Unplaceable(TimeValue),
    /// The split instance is at or before the series' start: that is an edit of the whole
    /// series, not a split.
    AtStart,
    /// A counted rule could not be read, or ran past the recurrence crate's budget before the
    /// split.
    Rule(String),
    /// The new series' derived key would be `length` characters, more than [`GOOGLE_ID_MAX`].
    /// It names the old series whole, so this happens to a series split again and again; the
    /// new series is never created.
    TooLong {
        /// The length of the key [`Split::ids`] derives.
        length: usize,
    },
}

impl core::fmt::Display for SplitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotRecurring => f.write_str("the series does not recur"),
            Self::Unplaceable(value) => write!(f, "{value:?} cannot be placed in time"),
            Self::AtStart => f.write_str("a split at the series' start edits the whole series"),
            Self::Rule(detail) => write!(f, "a counted rule cannot be trimmed: {detail}"),
            Self::TooLong { length } => write!(
                f,
                "the new series' id would be {length} characters, over {GOOGLE_ID_MAX}"
            ),
        }
    }
}

impl core::error::Error for SplitError {}

/// Plans splits, placing times with zones from a [`ZoneSource`].
#[derive(Debug)]
pub struct Splitter<'z, Z: ?Sized> {
    zones: &'z Z,
    floating: TimeZone,
}

impl<'z, Z: ZoneSource + ?Sized> Splitter<'z, Z> {
    /// A splitter resolving named zones through `zones`, and floating and all-day times in
    /// `floating`, as [`Expander`](calendar_sync_recurrence::Expander) does.
    pub fn new(zones: &'z Z, floating: TimeZone) -> Self {
        Self { zones, floating }
    }

    /// The next write of `split`, from the old series and the new one as the provider holds them
    /// now. `new` is the series under [`Split::ids`], or `None` where the provider has no master
    /// there. `tail` is the new series' fields, known only to the device that started the split;
    /// without it, a split that has not been created is left alone.
    ///
    /// # Errors
    ///
    /// [`SplitError`] where the old master does not recur, a time cannot be placed, the split is
    /// at the series' start, a counted rule cannot be trimmed, or the new series' key would be
    /// too long to create it under.
    pub fn next(
        &self,
        split: &Split,
        tail: Option<&NormalizedEvent>,
        old: &EventSeries,
        new: Option<&EventSeries>,
    ) -> Result<SplitStep, SplitError> {
        let Some(master) = &old.master else {
            return Ok(SplitStep::Done);
        };
        let at = self.instant(split.at.original_start())?;
        let trimmed = self.trim(&master.fields, at)?;
        let untrimmed = trimmed != master.fields;
        let new = new.and_then(|series| Some((series, series.master.as_ref()?)));
        let Some((new, new_master)) = new else {
            // A trimmed old series without a new one is a split whose new series was deleted
            // after the trim, or a series that already ended before the split: either way there
            // is nothing left to split, and the tail is not created.
            return match tail {
                Some(tail) if untrimmed => self.create(split, tail, old, master, at),
                _ => Ok(SplitStep::Done),
            };
        };
        let later = self.later_overrides(old, at)?;
        if untrimmed {
            let shift = self.shift(&split.at, new_master.fields.times.start());
            for (key, value) in &later {
                let Some(shift) = &shift else { break };
                // A cancelled instance went into the new master's excluded dates.
                let Override::Modified { event, range } = value else {
                    continue;
                };
                let Some(moved) = self.shift_value(key.original_start(), shift) else {
                    continue;
                };
                let moved = InstanceKey::new(moved);
                if !new.overrides.contains_key(&moved) {
                    return Ok(SplitStep::Carry {
                        series: ResourceId::of(new_master),
                        instance: moved,
                        event: self.carried(event, key, shift, new_master),
                        range: *range,
                    });
                }
            }
            return Ok(SplitStep::Trim {
                series: ResourceId::of(master),
                if_match: master.revision.clone(),
                base: master.fields.clone(),
                ours: trimmed,
            });
        }
        Ok(match later.into_iter().next() {
            Some((key, value)) => SplitStep::Remove {
                series: ResourceId::of(master),
                instance: key.clone(),
                value: value.clone(),
            },
            None => SplitStep::Done,
        })
    }

    /// The create of the new master.
    fn create(
        &self,
        split: &Split,
        tail: &NormalizedEvent,
        old: &EventSeries,
        master: &ProviderEvent,
        at: Timestamp,
    ) -> Result<SplitStep, SplitError> {
        let edit = split.create_edit();
        let ids = DerivedIds::of(&edit);
        let length = ids.key().len();
        if length > GOOGLE_ID_MAX {
            return Err(SplitError::TooLong { length });
        }
        let shift = self.shift(&split.at, tail.times.start());
        let mut fields = tail.clone();
        if let (Some(theirs), Some(ours), Some(shift)) = (
            master.fields.recurrence.as_ref(),
            fields.recurrence.as_mut(),
            &shift,
        ) {
            let carry = |from: &[TimeValue], into: &mut Vec<TimeValue>| {
                for value in from {
                    if self.instant(value).is_ok_and(|start| start >= at)
                        && let Some(moved) = self.shift_value(value, shift)
                        && !into.contains(&moved)
                    {
                        into.push(moved);
                    }
                }
            };
            let cancelled: Vec<TimeValue> = old
                .overrides
                .iter()
                .filter(|(_, value)| matches!(value, Override::Cancelled { .. }))
                .map(|(key, _)| key.original_start().clone())
                .collect();
            carry(&theirs.dates, &mut ours.dates);
            carry(&theirs.excluded_dates, &mut ours.excluded_dates);
            carry(&cancelled, &mut ours.excluded_dates);
        }
        let uncarried = match &shift {
            Some(_) => Vec::new(),
            None => old
                .overrides
                .keys()
                .filter(|key| {
                    self.instant(key.original_start())
                        .is_ok_and(|start| start >= at)
                })
                .cloned()
                .collect(),
        };
        let master = ProviderEvent {
            ids: ProviderIds {
                provider: master.ids.provider,
                id: ids.key().to_owned(),
                ical_uid: Some(ids.ical_uid()),
                series_id: None,
            },
            revision: None,
            mapper_version: master.mapper_version,
            fields,
            passthrough: Passthrough::new(),
        };
        Ok(SplitStep::Create {
            edit,
            ids,
            master,
            uncarried,
        })
    }

    /// The old series' overrides at or after `at`, in time order.
    fn later_overrides<'s>(
        &self,
        old: &'s EventSeries,
        at: Timestamp,
    ) -> Result<Vec<(&'s InstanceKey, &'s Override)>, SplitError> {
        let mut later = Vec::new();
        for (key, value) in &old.overrides {
            let start = self.instant(key.original_start())?;
            if start >= at {
                later.push((start, key, value));
            }
        }
        later.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        Ok(later
            .into_iter()
            .map(|(_, key, value)| (key, value))
            .collect())
    }

    /// `event`, a modified instance of the old series at `key`, as the new series holds it.
    fn carried(
        &self,
        event: &ProviderEvent,
        key: &InstanceKey,
        shift: &Shift,
        new_master: &ProviderEvent,
    ) -> ProviderEvent {
        let mut event = event.clone();
        event.ids = ProviderIds {
            provider: new_master.ids.provider,
            id: new_master.ids.id.clone(),
            ical_uid: new_master.ids.ical_uid.clone(),
            series_id: Some(new_master.ids.id.clone()),
        };
        event.revision = None;
        // An instance the user moved stays where they put it; one they did not moves with the
        // series.
        if event.fields.times.start() == key.original_start()
            && let Some(times) = self.shift_times(&event.fields.times, shift)
        {
            event.fields.times = times;
        }
        event
    }

    /// `fields` with every rule ended before `at`, and every extra date at or after it dropped.
    fn trim(&self, fields: &NormalizedEvent, at: Timestamp) -> Result<NormalizedEvent, SplitError> {
        let Some(recurrence) = &fields.recurrence else {
            return Err(SplitError::NotRecurring);
        };
        let start = fields.times.start();
        if self.instant(start)? >= at {
            return Err(SplitError::AtStart);
        }
        let zone = self.zone_of(start)?;
        let at_civil = zone.to_datetime(at);
        let until = until_before(start, at, at_civil);
        let mut trimmed = Recurrence {
            rules: Vec::with_capacity(recurrence.rules.len()),
            exclusion_rules: recurrence.exclusion_rules.clone(),
            dates: Vec::with_capacity(recurrence.dates.len()),
            excluded_dates: recurrence.excluded_dates.clone(),
        };
        for rule in &recurrence.rules {
            let mut rule = rule.clone();
            rule.end = match rule.end {
                RuleEnd::Count { count } => RuleEnd::Count {
                    count: count_before(&rule, civil(start), at_civil, count)?,
                },
                RuleEnd::Until { until: ref own } if self.until_before(own, start, at)? => {
                    rule.end.clone()
                }
                RuleEnd::Never | RuleEnd::Until { .. } => RuleEnd::Until {
                    until: until.clone(),
                },
            };
            trimmed.rules.push(rule);
        }
        for date in &recurrence.dates {
            if self.instant(date)? < at {
                trimmed.dates.push(date.clone());
            }
        }
        let mut fields = fields.clone();
        fields.recurrence = Some(trimmed);
        Ok(fields)
    }

    /// Whether a rule's own `UNTIL` already ends it before `at`.
    fn until_before(
        &self,
        until: &TimeValue,
        start: &TimeValue,
        at: Timestamp,
    ) -> Result<bool, SplitError> {
        // A day bounds a timed series at its end, as the expander reads it.
        let bound = match (until, start) {
            (TimeValue::AllDay { date }, start) if !matches!(start, TimeValue::AllDay { .. }) => {
                self.instant(&TimeValue::floating(date.to_datetime(Time::MAX)))?
            }
            _ => self.instant(until)?,
        };
        Ok(bound < at)
    }

    /// How the new series' start, `to`, moved from the split instance, `at`.
    fn shift(&self, at: &InstanceKey, to: &TimeValue) -> Option<Shift> {
        let at = at.original_start();
        let all_day = |value: &TimeValue| matches!(value, TimeValue::AllDay { .. });
        if all_day(at) != all_day(to) {
            return None;
        }
        let elapsed = self
            .instant(to)
            .ok()?
            .duration_since(self.instant(at).ok()?);
        let civil = same_frame(at, to).then(|| civil(to).duration_since(civil(at)));
        Some(Shift {
            frame: at.clone(),
            civil,
            elapsed,
        })
    }

    /// `value` moved by `shift`, in its own form.
    fn shift_value(&self, value: &TimeValue, shift: &Shift) -> Option<TimeValue> {
        if let Some(civil_shift) = shift.civil
            && same_frame(value, &shift.frame)
        {
            let moved = civil(value).checked_add(civil_shift).ok()?;
            return Some(with_civil(value, moved));
        }
        let zone = self.zone_of(value).ok()?;
        let moved = self.instant(value).ok()?.checked_add(shift.elapsed).ok()?;
        Some(match value {
            TimeValue::Instant { .. } => TimeValue::instant(moved),
            // A day or floating time outside the split's frame moves by whole civil time.
            TimeValue::AllDay { .. } | TimeValue::Floating { .. } | TimeValue::Zoned { .. } => {
                with_civil(value, zone.to_datetime(moved))
            }
        })
    }

    /// `times` moved by `shift`, or `None` where the result is not a valid span.
    fn shift_times(&self, times: &EventTimes, shift: &Shift) -> Option<EventTimes> {
        let start = self.shift_value(times.start(), shift)?;
        let end = match times.end() {
            Some(end) => Some(self.shift_value(end, shift)?),
            None => None,
        };
        EventTimes::new(start, end).ok()
    }

    /// The zone a value's civil time is read in.
    fn zone_of(&self, value: &TimeValue) -> Result<TimeZone, SplitError> {
        match value {
            TimeValue::Instant { .. } => Ok(TimeZone::UTC),
            TimeValue::Zoned { time_zone, .. } => self
                .zones
                .time_zone(time_zone)
                .ok_or_else(|| SplitError::Unplaceable(value.clone())),
            TimeValue::Floating { .. } | TimeValue::AllDay { .. } => Ok(self.floating.clone()),
        }
    }

    /// `value` as an instant, a civil time in a gap or fold placed as RFC 5545 section 3.3.5
    /// says (jiff's "compatible" disambiguation), as the expander places it.
    fn instant(&self, value: &TimeValue) -> Result<Timestamp, SplitError> {
        if let TimeValue::Instant { timestamp } = value {
            return Ok(*timestamp);
        }
        self.zone_of(value)?
            .to_ambiguous_timestamp(civil(value))
            .compatible()
            .map_err(|_| SplitError::Unplaceable(value.clone()))
    }
}

/// How far a split moved the series.
#[derive(Clone, Debug)]
struct Shift {
    /// The split instance's original start: values in its frame move by civil time.
    frame: TimeValue,
    /// The civil distance, where the new start is in the split instance's frame.
    civil: Option<SignedDuration>,
    /// The elapsed distance.
    elapsed: SignedDuration,
}

/// The `UNTIL` that ends a series starting at `start` just before `at`, in the form RFC 5545
/// section 3.3.10 requires: a date for an all-day series, a floating time for a floating one,
/// and UTC otherwise.
fn until_before(start: &TimeValue, at: Timestamp, at_civil: DateTime) -> TimeValue {
    let second = SignedDuration::from_secs(1);
    match start {
        TimeValue::AllDay { .. } => {
            let last = at_civil
                .checked_sub(SignedDuration::from_nanos(1))
                .map_or(Date::MIN, DateTime::date);
            TimeValue::all_day(last)
        }
        TimeValue::Floating { .. } => {
            TimeValue::floating(at_civil.checked_sub(second).unwrap_or(DateTime::MIN))
        }
        TimeValue::Instant { .. } | TimeValue::Zoned { .. } => {
            TimeValue::instant(at.checked_sub(second).unwrap_or(Timestamp::MIN))
        }
    }
}

/// How many of a counted rule's instances, at most `count`, fall before `at`.
fn count_before(
    rule: &RecurrenceRule,
    start: DateTime,
    at: DateTime,
    count: u32,
) -> Result<u32, SplitError> {
    let compiled = CivilRule::new(rule, start, None)
        .map_err(|loss| SplitError::Rule(loss.detail))?
        .counting_start();
    let mut instances = compiled.instances(None, at);
    let mut before = 0;
    for instance in instances.by_ref() {
        if instance >= at || before == count {
            return Ok(before);
        }
        before += 1;
    }
    if instances.exhausted() {
        return Err(SplitError::Rule(
            "the rule ran past its budget before the split".to_owned(),
        ));
    }
    Ok(before)
}

/// Whether two values read their civil times in the same frame.
fn same_frame(a: &TimeValue, b: &TimeValue) -> bool {
    match (a, b) {
        (TimeValue::Zoned { time_zone: x, .. }, TimeValue::Zoned { time_zone: y, .. }) => x == y,
        _ => a.kind() == b.kind(),
    }
}

/// `value`'s civil date and time in its own terms: an instant's in UTC, a day's at midnight.
fn civil(value: &TimeValue) -> DateTime {
    match value {
        TimeValue::Instant { timestamp } => TimeZone::UTC.to_datetime(*timestamp),
        TimeValue::Zoned { date_time, .. } | TimeValue::Floating { date_time } => *date_time,
        TimeValue::AllDay { date } => date.to_datetime(Time::midnight()),
    }
}

/// A value of `form`'s form at civil time `at`, read in `form`'s own terms.
fn with_civil(form: &TimeValue, at: DateTime) -> TimeValue {
    match form {
        TimeValue::Instant { .. } => match TimeZone::UTC.to_ambiguous_timestamp(at).unambiguous() {
            Ok(timestamp) => TimeValue::instant(timestamp),
            Err(_) => TimeValue::floating(at),
        },
        TimeValue::Zoned { time_zone, .. } => TimeValue::zoned(at, time_zone.clone()),
        TimeValue::Floating { .. } => TimeValue::floating(at),
        TimeValue::AllDay { .. } => TimeValue::all_day(at.date()),
    }
}

/// An instance key as text, one letter for its form and then its value.
fn instance_text(key: &InstanceKey) -> String {
    match key.original_start() {
        TimeValue::Instant { timestamp } => format!("i{timestamp}"),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => format!("z{date_time} {time_zone}"),
        TimeValue::Floating { date_time } => format!("f{date_time}"),
        TimeValue::AllDay { date } => format!("d{date}"),
    }
}

/// The instance key [`instance_text`] wrote.
fn parse_instance(text: &str) -> Option<InstanceKey> {
    let mut chars = text.chars();
    let form = chars.next()?;
    let value = chars.as_str();
    let value = match form {
        'i' => TimeValue::instant(value.parse().ok()?),
        'z' => {
            let (date_time, zone) = value.split_once(' ')?;
            TimeValue::zoned(date_time.parse().ok()?, TimeZoneId::new(zone))
        }
        'f' => TimeValue::floating(value.parse().ok()?),
        'd' => TimeValue::all_day(value.parse().ok()?),
        _ => return None,
    };
    Some(InstanceKey::new(value))
}

/// The UTF-8 text whose unpadded, lower-case base32hex (RFC 4648, section 7) is `encoded`.
fn decode(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len() * 5 / 8);
    let mut buffer: u16 = 0;
    let mut bits = 0;
    for symbol in encoded.bytes() {
        let value = match symbol {
            b'0'..=b'9' => symbol - b'0',
            b'a'..=b'v' => symbol - b'a' + 10,
            _ => return None,
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from(buffer >> bits).ok()?);
            buffer &= (1 << bits) - 1;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::{Split, decode, parse_instance};
    use crate::ids::DerivedIds;
    use crate::seam::ResourceId;
    use crate::sync::EditId;
    use calendar_sync_model::{InstanceKey, TimeValue, TimeZoneId};

    #[test]
    fn decode_inverts_the_derivation() {
        for text in ["", "a", "ab", "abc", "abcd", "abcde", "abcdef", "édit/ü"] {
            let key = DerivedIds::of(&EditId::new(text)).key().to_owned();
            assert_eq!(decode(&key["csedit".len()..]).as_deref(), Some(text));
        }
    }

    #[test]
    fn every_instance_form_round_trips() {
        let values = [
            TimeValue::instant("2026-01-05T10:00:00.5Z".parse().unwrap()),
            TimeValue::zoned(
                "2026-01-05T10:00:00".parse().unwrap(),
                TimeZoneId::new("America/New_York"),
            ),
            TimeValue::floating("2026-01-05T10:00:00".parse().unwrap()),
            TimeValue::all_day("2026-01-05".parse().unwrap()),
        ];
        for value in values {
            let at = InstanceKey::new(value);
            let split = Split::new(EditId::new("e:1"), ResourceId::new("a:b"), at.clone());
            assert_eq!(parse_instance(&super::instance_text(&at)), Some(at));
            assert_eq!(Split::from_key(split.ids().key()), Some(split));
        }
    }

    #[test]
    fn an_ordinary_create_is_not_a_split() {
        let key = DerivedIds::of(&EditId::new("e1")).key().to_owned();
        assert_eq!(Split::from_key(&key), None);
        assert_eq!(Split::from_key("not a key"), None);
        let forged = DerivedIds::of(&EditId::new("split1:1:a1:e2:dx"))
            .key()
            .to_owned();
        assert_eq!(Split::from_key(&forged), None);
    }
}
