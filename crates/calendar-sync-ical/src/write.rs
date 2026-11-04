//! Writing: the model's fields into a `VEVENT`, changing only the properties whose fields changed.
//!
//! Every edit is a difference between two readings: the fields the component reads as now, and
//! the fields the caller wants. A field that reads the same is not written, so its bytes stay as
//! they were, and so do the bytes of every property the model does not map.

use std::collections::BTreeMap;

use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, Field, Loss, LossReason,
    NormalizedEvent, Organizer, ParticipantKind, Participation, Recurrence, RelativeTo, TimeValue,
    Transparency, Visibility,
};
use ical::tree::cst::{IcalCst, IcalItem};
use jiff::tz::TimeZone;

use crate::line;
use crate::read;
use crate::value;
use crate::zone::{self, Zones};

/// What one write collects beside its edits.
pub(crate) struct Writer<'z> {
    /// The zones of the calendar being edited.
    pub(crate) zones: &'z Zones,
    /// What could not be written.
    pub(crate) losses: Vec<Loss>,
    /// The IANA zones written in a `TZID`, each with the earliest time written in it (Unix
    /// seconds), so the calendar can be given their `VTIMEZONE`s.
    pub(crate) needed: BTreeMap<String, i64>,
}

impl<'z> Writer<'z> {
    pub(crate) fn new(zones: &'z Zones) -> Self {
        Self {
            zones,
            losses: Vec::new(),
            needed: BTreeMap::new(),
        }
    }

    /// Reports the control characters a text value loses: RFC 5545 section 3.3.11 allows none
    /// but tab and the line break written as `\n`, so a carriage return becomes a line break and
    /// any other control character is dropped.
    pub(crate) fn text_loss(&mut self, field: Field, value: Option<&str>) {
        if value.is_some_and(|value| {
            value
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n'))
        }) {
            self.losses.push(Loss::new(
                field,
                LossReason::Approximated,
                "control characters iCalendar text cannot hold are dropped or written as line breaks",
            ));
        }
    }

    /// The `VALUE` and `TZID` parameters and the value of a `DATE` or `DATE-TIME` property.
    pub(crate) fn time(
        &mut self,
        value: &TimeValue,
        field: &Field,
    ) -> ([(&'static str, Option<String>); 2], String) {
        let fraction = match value {
            TimeValue::Instant { timestamp } => timestamp.subsec_nanosecond(),
            TimeValue::Zoned { date_time, .. } | TimeValue::Floating { date_time } => {
                date_time.subsec_nanosecond()
            }
            TimeValue::AllDay { .. } => 0,
        };
        if fraction != 0 {
            self.losses.push(Loss::new(
                field.clone(),
                LossReason::Approximated,
                "a fraction of a second is dropped",
            ));
        }
        match value {
            TimeValue::AllDay { date } => (
                [("VALUE", Some("DATE".to_owned())), ("TZID", None)],
                value::format_date(*date),
            ),
            TimeValue::Floating { date_time } => (
                [("VALUE", None), ("TZID", None)],
                value::format_date_time(*date_time),
            ),
            TimeValue::Instant { timestamp } => (
                [("VALUE", None), ("TZID", None)],
                value::format_instant(*timestamp),
            ),
            TimeValue::Zoned {
                date_time,
                time_zone,
            } => {
                let anchor = date_time
                    .to_zoned(TimeZone::UTC)
                    .map_or(0, |zoned| zoned.timestamp().as_second());
                self.needed
                    .entry(time_zone.as_str().to_owned())
                    .and_modify(|earliest| *earliest = (*earliest).min(anchor))
                    .or_insert(anchor);
                (
                    [
                        ("VALUE", None),
                        ("TZID", Some(time_zone.as_str().to_owned())),
                    ],
                    value::format_date_time(*date_time),
                )
            }
        }
    }
}

/// Writes into `component` every field of `new` that differs from `old`, which is how the
/// component reads now.
pub(crate) fn apply(
    component: &mut IcalCst<'static>,
    old: &NormalizedEvent,
    new: &NormalizedEvent,
    writer: &mut Writer<'_>,
) {
    if old.title != new.title {
        writer.text_loss(Field::Title, new.title.as_deref());
        let value = new.title.as_deref().map(line::escape_text);
        line::set_single(component, "SUMMARY", &[], value.as_deref());
    }
    if old.location != new.location {
        writer.text_loss(Field::Location, new.location.as_deref());
        let value = new.location.as_deref().map(line::escape_text);
        line::set_single(component, "LOCATION", &[("ALTREP", None)], value.as_deref());
    }
    if old.description != new.description {
        writer.text_loss(Field::Description, new.description.as_deref());
        let value = new.description.as_deref().map(line::escape_text);
        line::set_single(
            component,
            "DESCRIPTION",
            &[("ALTREP", None)],
            value.as_deref(),
        );
    }
    if old.status != new.status {
        line::set_single(component, "STATUS", &[], Some(status_name(new.status)));
    }
    if old.transparency != new.transparency {
        let value = match new.transparency {
            Transparency::Opaque => "OPAQUE",
            Transparency::Transparent => "TRANSPARENT",
        };
        line::set_single(component, "TRANSP", &[], Some(value));
    }
    if old.visibility != new.visibility {
        let value = match new.visibility {
            Visibility::Default => None,
            Visibility::Public => Some("PUBLIC"),
            Visibility::Private => Some("PRIVATE"),
            Visibility::Confidential => Some("CONFIDENTIAL"),
        };
        line::set_single(component, "CLASS", &[], value);
    }
    times(component, old, new, writer);
    recurrence(component, old, new, writer);
    if old.organizer != new.organizer {
        organizer(component, new.organizer.as_ref());
    }
    if old.attendees != new.attendees {
        attendees(component, &new.attendees);
    }
    if new.reminders.use_default {
        writer.losses.push(Loss::new(
            Field::Reminders,
            LossReason::Unrepresentable,
            "iCalendar has no calendar-default reminders",
        ));
    }
    if old.reminders.alerts != new.reminders.alerts {
        alarms(
            component,
            &new.reminders.alerts,
            new.title.as_deref(),
            writer,
        );
    }
}

fn status_name(status: EventStatus) -> &'static str {
    match status {
        EventStatus::Confirmed => "CONFIRMED",
        EventStatus::Tentative => "TENTATIVE",
        EventStatus::Cancelled => "CANCELLED",
    }
}

/// `DTSTART`, and `DTEND` or `DURATION`.
fn times(
    component: &mut IcalCst<'static>,
    old: &NormalizedEvent,
    new: &NormalizedEvent,
    writer: &mut Writer<'_>,
) {
    let start_changed = old.times.start() != new.times.start();
    if start_changed {
        let (params, value) = writer.time(new.times.start(), &Field::Times);
        line::set_single(component, "DTSTART", &params, Some(&value));
    }
    // A `DURATION` counts from the start, so a moved start moves the end it describes.
    let has_duration = !line::positions(component, "DURATION").is_empty();
    if old.times.end() == new.times.end() && !(start_changed && has_duration) {
        return;
    }
    line::remove_all(component, "DURATION");
    match new.times.end() {
        Some(end) => {
            let (params, value) = writer.time(end, &Field::Times);
            line::set_single(component, "DTEND", &params, Some(&value));
        }
        None => {
            line::remove_all(component, "DTEND");
            if new.times.is_all_day() {
                writer.losses.push(Loss::new(
                    Field::Times,
                    LossReason::Approximated,
                    "an all-day event with no end is written with no DTEND, which lasts one day",
                ));
            }
        }
    }
}

/// `RRULE`, `EXRULE`, `RDATE` and `EXDATE`.
fn recurrence(
    component: &mut IcalCst<'static>,
    old: &NormalizedEvent,
    new: &NormalizedEvent,
    writer: &mut Writer<'_>,
) {
    let empty = Recurrence::default();
    let old = old.recurrence.as_ref().unwrap_or(&empty);
    let new = new.recurrence.as_ref().unwrap_or(&empty);
    for (name, old_rules, new_rules) in [
        ("RRULE", &old.rules, &new.rules),
        ("EXRULE", &old.exclusion_rules, &new.exclusion_rules),
    ] {
        if old_rules == new_rules {
            continue;
        }
        line::remove_all(component, name);
        for rule in new_rules {
            let text = value::format_recur(rule, &mut writer.losses);
            component.push_line(line::build(name, &[], &text));
        }
    }
    for (name, old_dates, new_dates) in [
        ("RDATE", &old.dates, &new.dates),
        ("EXDATE", &old.excluded_dates, &new.excluded_dates),
    ] {
        if old_dates == new_dates {
            continue;
        }
        // A line holding a value the model cannot (an `RDATE` period) is kept: removing it would
        // lose that value.
        let mut scratch = Vec::new();
        let read_whole: Vec<usize> = line::positions(component, name)
            .into_iter()
            .filter(|&at| match &component.items[at] {
                IcalItem::Prop(old) => read::date_list(old, writer.zones, &mut scratch).1,
                _ => false,
            })
            .collect();
        for at in read_whole.into_iter().rev() {
            component.items.remove(at);
        }
        for date in new_dates {
            let (params, value) = writer.time(date, &Field::Recurrence);
            let params: Vec<(&str, String)> = params
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect();
            component.push_line(line::build(name, &params, &value));
        }
    }
}

/// A calendar address as a `CAL-ADDRESS` value: a URI kept as it is, anything else as `mailto:`.
pub(crate) fn address_value(address: &str) -> String {
    let address: String = address.chars().filter(|c| !c.is_control()).collect();
    let scheme = address.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    if scheme {
        address
    } else {
        format!("mailto:{address}")
    }
}

fn organizer(component: &mut IcalCst<'static>, new: Option<&Organizer>) {
    match new {
        None => line::remove_all(component, "ORGANIZER"),
        Some(organizer) => line::set_single(
            component,
            "ORGANIZER",
            &[("CN", organizer.name.clone())],
            Some(&address_value(&organizer.address)),
        ),
    }
}

/// The parameters of an `ATTENDEE` line. A parameter at its default is left out unless the line
/// already has it.
fn attendee_params(
    attendee: &Attendee,
    old: Option<&ical::tree::line::IcalLine<'_>>,
) -> Vec<(&'static str, Option<String>)> {
    let had = |name: &str| old.is_some_and(|line| line::has_param(line, name));
    let role = match attendee.role {
        AttendeeRole::Required => "REQ-PARTICIPANT",
        AttendeeRole::Optional => "OPT-PARTICIPANT",
        AttendeeRole::Chair => "CHAIR",
        AttendeeRole::NonParticipant => "NON-PARTICIPANT",
    };
    let participation = match attendee.participation {
        Participation::NeedsAction => "NEEDS-ACTION",
        Participation::Accepted => "ACCEPTED",
        Participation::Declined => "DECLINED",
        Participation::Tentative => "TENTATIVE",
        Participation::Delegated => "DELEGATED",
    };
    let kind = match attendee.kind {
        ParticipantKind::Individual => "INDIVIDUAL",
        ParticipantKind::Group => "GROUP",
        ParticipantKind::Resource => "RESOURCE",
        ParticipantKind::Room => "ROOM",
        ParticipantKind::Unknown => "UNKNOWN",
    };
    let keep =
        |default: bool, name: &str, value: &str| (!default || had(name)).then(|| value.to_owned());
    vec![
        ("CN", attendee.name.clone()),
        (
            "ROLE",
            keep(attendee.role == AttendeeRole::default(), "ROLE", role),
        ),
        (
            "PARTSTAT",
            keep(
                attendee.participation == Participation::default(),
                "PARTSTAT",
                participation,
            ),
        ),
        (
            "CUTYPE",
            keep(attendee.kind == ParticipantKind::default(), "CUTYPE", kind),
        ),
    ]
}

/// Rewrites the `ATTENDEE` lines in the model's order: an attendee kept is matched to its line by
/// address and keeps that line's bytes, a changed one is rewritten keeping the parameters the
/// model does not map, and a new one gets a new line.
///
/// The lines fill the positions the `ATTENDEE` lines held, in order, so where only one attendee
/// changes, only its line does. Lines beyond those positions follow the last of them, and
/// positions left over are removed.
fn attendees(component: &mut IcalCst<'static>, new: &[Attendee]) {
    let mut scratch = Vec::new();
    let slots = line::positions(component, "ATTENDEE");
    let existing: Vec<(ical::tree::line::IcalLine<'static>, Attendee)> = slots
        .iter()
        .filter_map(|&at| match &component.items[at] {
            IcalItem::Prop(old) => Some((old.clone(), read::attendee(old, &mut scratch))),
            _ => None,
        })
        .collect();
    let mut used = vec![false; existing.len()];
    let mut lines = Vec::with_capacity(new.len());
    for attendee in new {
        let matched = existing.iter().enumerate().position(|(index, (_, old))| {
            !used[index] && old.address.eq_ignore_ascii_case(&attendee.address)
        });
        let value = address_value(&attendee.address);
        lines.push(match matched {
            Some(index) => {
                used[index] = true;
                let (old_line, old) = &existing[index];
                if old == attendee {
                    old_line.clone()
                } else {
                    line::rewrite(old_line, &attendee_params(attendee, Some(old_line)), &value)
                }
            }
            None => {
                let params: Vec<(&str, String)> = attendee_params(attendee, None)
                    .into_iter()
                    .filter_map(|(name, value)| value.map(|value| (name, value)))
                    .collect();
                line::build("ATTENDEE", &params, &value)
            }
        });
    }
    let mut lines = lines.into_iter();
    for &at in &slots {
        match lines.next() {
            Some(line) => component.items[at] = IcalItem::Prop(line),
            None => break,
        }
    }
    let extra: Vec<_> = lines.collect();
    if let Some(&last) = slots.last() {
        for (offset, line) in extra.into_iter().enumerate() {
            component
                .items
                .insert(last + 1 + offset, IcalItem::Prop(line));
        }
    } else {
        for line in extra {
            component.push_line(line);
        }
    }
    for &at in slots.iter().skip(new.len()).rev() {
        component.items.remove(at);
    }
}

/// Rewrites the `VALARM`s: an alarm whose alert is kept stays as it is, one whose alert is gone is
/// removed, and a new alert gets a new `VALARM`. An alarm the model cannot hold is never touched.
fn alarms(
    component: &mut IcalCst<'static>,
    new: &[Alert],
    title: Option<&str>,
    writer: &mut Writer<'_>,
) {
    let existing: Vec<(usize, Alert)> = component
        .items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| match item {
            IcalItem::Component(child) if line::component_is(child, "VALARM") => {
                read::alarm(child).0.map(|alert| (at, alert))
            }
            _ => None,
        })
        .collect();
    let mut wanted: Vec<Option<&Alert>> = new.iter().map(Some).collect();
    let mut removed = Vec::new();
    for (at, alert) in &existing {
        match wanted
            .iter_mut()
            .find(|slot| slot.is_some_and(|new| new == alert))
        {
            Some(slot) => *slot = None,
            None => removed.push(*at),
        }
    }
    for at in removed.into_iter().rev() {
        component.items.remove(at);
    }
    for alert in wanted.into_iter().flatten() {
        component.push_component(valarm(alert, title, writer));
    }
}

/// A new `VALARM` for `alert`.
fn valarm(alert: &Alert, title: Option<&str>, writer: &mut Writer<'_>) -> IcalCst<'static> {
    let mut alarm = IcalCst::empty("VALARM");
    let action = match alert.action {
        AlertAction::Email => "EMAIL",
        // `AlertAction` may grow; a display alarm is the one every client shows.
        _ => "DISPLAY",
    };
    alarm.push_line(line::build("ACTION", &[], action));
    let trigger = match &alert.trigger {
        AlertTrigger::Offset {
            offset,
            relative_to,
        } => {
            if offset.subsec_nanos() != 0 {
                writer.losses.push(Loss::new(
                    Field::Reminders,
                    LossReason::Approximated,
                    "a fraction of a second is dropped from a reminder offset",
                ));
            }
            let params = match relative_to {
                RelativeTo::Start => Vec::new(),
                RelativeTo::End => vec![("RELATED", "END".to_owned())],
            };
            line::build("TRIGGER", &params, &value::format_duration(*offset))
        }
        AlertTrigger::Absolute { at } => line::build(
            "TRIGGER",
            &[("VALUE", "DATE-TIME".to_owned())],
            &value::format_instant(*at),
        ),
    };
    alarm.push_line(trigger);
    let description = line::escape_text(title.unwrap_or("Reminder"));
    alarm.push_line(line::build("DESCRIPTION", &[], &description));
    if alert.action == AlertAction::Email {
        alarm.push_line(line::build("SUMMARY", &[], &description));
        writer.losses.push(Loss::new(
            Field::Reminders,
            LossReason::Approximated,
            "an EMAIL alarm is written with no ATTENDEE to send to",
        ));
    }
    alarm
}

/// Gives `calendar` a `VTIMEZONE` for every zone written that it does not define yet, before its
/// first component.
pub(crate) fn add_vtimezones(
    calendar: &mut IcalCst<'static>,
    needed: &BTreeMap<String, i64>,
    losses: &mut Vec<Loss>,
) {
    for (tzid, anchor) in needed {
        if zone::defines(calendar, tzid) {
            continue;
        }
        match zone::vtimezone(tzid, *anchor) {
            Some(vtimezone) => {
                let at = calendar
                    .items
                    .iter()
                    .position(|item| matches!(item, IcalItem::Component(_)))
                    .unwrap_or(calendar.items.len());
                calendar
                    .items
                    .insert(at, IcalItem::Component(Box::new(vtimezone)));
            }
            None => losses.push(Loss::new(
                Field::Times,
                LossReason::Unrepresentable,
                format!(
                    "TZID `{tzid}` is written with no VTIMEZONE, since no IANA zone has that name"
                ),
            )),
        }
    }
}
