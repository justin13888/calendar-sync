//! Reading: a `VEVENT` into the model's fields, and the `VEVENT`s of a calendar into series.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, EventTimes, Field,
    InstanceKey, InstanceRange, Loss, LossReason, NormalizedEvent, Organizer, ParticipantKind,
    Participation, Passthrough, Recurrence, RelativeTo, Reminders, TimeValue, Transparency,
    Visibility,
};
use ical::tree::cst::{IcalCst, IcalItem};
use ical::tree::line::IcalLine;
use jiff::Span;
use jiff::tz::TimeZone;

use crate::line;
use crate::value::{self, Duration};
use crate::zone::Zones;

/// One `VEVENT`, read.
#[derive(Debug)]
pub(crate) struct Component {
    /// Its `UID`.
    pub(crate) uid: Option<String>,
    /// Its `RECURRENCE-ID`: `None` for a master or single event, `Some(None)` where the value is
    /// malformed.
    pub(crate) instance: Option<Option<(InstanceKey, InstanceRange)>>,
    /// Whether its `STATUS` is `CANCELLED`.
    pub(crate) cancelled: bool,
    /// Its fields, or `None` where it has no readable `DTSTART`.
    pub(crate) event: Option<NormalizedEvent>,
    /// The properties and subcomponents the model has no place for.
    pub(crate) passthrough: Passthrough,
    /// What could not be carried.
    pub(crate) losses: Vec<Loss>,
}

/// Reads one `VEVENT`.
#[allow(clippy::too_many_lines)]
pub(crate) fn event(component: &IcalCst<'_>, zones: &Zones) -> Component {
    let mut losses = Vec::new();
    let mut passthrough = Passthrough::new();
    let mut uid = None;
    let mut start = None;
    let mut end_line = None;
    let mut duration_line = None;
    let mut title = None;
    let mut location = None;
    let mut description = None;
    let mut status_line = None;
    let mut transparency_line = None;
    let mut class_line = None;
    let mut organizer_line = None;
    let mut recurrence_id = None;
    let mut attendees = Vec::new();
    let mut recurrence = Recurrence::default();
    let mut alerts = Vec::new();

    for item in &component.items {
        let line = match item {
            IcalItem::Prop(line) => line,
            IcalItem::Component(child) => {
                if line::component_is(child, "VALARM") {
                    let (alert, alert_losses) = alarm(child);
                    losses.extend(alert_losses);
                    if let Some(alert) = alert {
                        alerts.push(alert);
                        continue;
                    }
                }
                let name = line::component_name(child).into_owned();
                passthrough.push(name, component_text(child));
                continue;
            }
            // Bytes a recovering parse could not structure. The document reports them.
            IcalItem::Opaque(_) => continue,
        };
        let slot = match line::name(line).to_ascii_uppercase().as_str() {
            "UID" => &mut uid,
            "DTSTART" => &mut start,
            "DTEND" => &mut end_line,
            "DURATION" => &mut duration_line,
            "SUMMARY" => &mut title,
            "LOCATION" => &mut location,
            "DESCRIPTION" => &mut description,
            "STATUS" => &mut status_line,
            "TRANSP" => &mut transparency_line,
            "CLASS" => &mut class_line,
            "ORGANIZER" => &mut organizer_line,
            "RECURRENCE-ID" => &mut recurrence_id,
            "ATTENDEE" => {
                attendees.push(attendee(line, &mut losses));
                continue;
            }
            "RRULE" | "EXRULE" => {
                rule_into(line, &mut recurrence, &mut losses);
                continue;
            }
            "RDATE" | "EXDATE" => {
                let (dates, _) = date_list(line, zones, &mut losses);
                if line::is(line, "RDATE") {
                    recurrence.dates.extend(dates);
                } else {
                    recurrence.excluded_dates.extend(dates);
                }
                continue;
            }
            _ => {
                passthrough.push(line.name.get(), line::logical(line));
                continue;
            }
        };
        if slot.is_some() {
            // A second instance of a property the model holds once: kept, not mapped.
            passthrough.push(line.name.get(), line::logical(line));
        } else {
            *slot = Some(line);
        }
    }

    if let (Some(_), Some(duration)) = (end_line, duration_line) {
        losses.push(Loss::new(
            Field::Times,
            LossReason::Malformed,
            "both DTEND and DURATION are present; DTEND is read",
        ));
        passthrough.push(duration.name.get(), line::logical(duration));
    }

    let instance = recurrence_id.map(|line| {
        let key = time(line, zones, &mut losses, &Field::Overrides)?;
        let range = match line::param(line, "RANGE") {
            Some(range) if range.eq_ignore_ascii_case("THISANDFUTURE") => {
                InstanceRange::ThisAndFuture
            }
            _ => InstanceRange::ThisInstance,
        };
        Some((InstanceKey::new(key), range))
    });

    let status = status_line.map(line::text);
    let cancelled = status
        .as_deref()
        .is_some_and(|status| status.trim().eq_ignore_ascii_case("CANCELLED"));

    let times = start.and_then(|start| times(start, end_line, duration_line, zones, &mut losses));
    let event = match times {
        None => {
            if start.is_none() {
                losses.push(Loss::new(
                    Field::Times,
                    LossReason::Malformed,
                    "the VEVENT has no DTSTART",
                ));
            }
            None
        }
        Some(times) => {
            let mut event = NormalizedEvent::new(times);
            event.title = title.map(line::text);
            event.location = location.map(line::text);
            event.description = description.map(line::text);
            event.status = status.map_or(EventStatus::Confirmed, |status| {
                event_status(&status, &mut losses)
            });
            event.transparency = transparency_line.map_or(Transparency::Opaque, |line| {
                transparency(&line::text(line), &mut losses)
            });
            event.visibility = class_line.map_or(Visibility::Default, |line| {
                visibility(&line::text(line), &mut losses)
            });
            event.organizer = organizer_line.map(organizer);
            event.attendees = attendees;
            event.recurrence = (recurrence != Recurrence::default()).then_some(recurrence);
            event.reminders = Reminders {
                use_default: false,
                alerts,
            };
            Some(event)
        }
    };

    Component {
        uid: uid.map(|line| line::text(line).trim().to_owned()),
        instance,
        cancelled,
        event,
        passthrough,
        losses,
    }
}

/// A subcomponent's bytes as text, without its final line ending.
pub(crate) fn component_text(component: &IcalCst<'_>) -> String {
    let text = String::from_utf8_lossy(&component.to_bytes()).into_owned();
    text.trim_end_matches(['\r', '\n']).to_owned()
}

/// The event's start and end.
fn times(
    start: &IcalLine<'_>,
    end: Option<&IcalLine<'_>>,
    duration: Option<&IcalLine<'_>>,
    zones: &Zones,
    losses: &mut Vec<Loss>,
) -> Option<EventTimes> {
    let start = time(start, zones, losses, &Field::Times)?;
    let end = if let Some(end) = end {
        time(end, zones, losses, &Field::Times)
    } else if let Some(duration) = duration {
        let text = line::text(duration);
        let end =
            value::parse_duration(text.trim()).and_then(|duration| end_after(&start, duration));
        if end.is_none() {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Malformed,
                format!("DURATION `{text}` is not a duration this event can take"),
            ));
        }
        end
    } else if let TimeValue::AllDay { date } = &start {
        // RFC 5545 section 3.6.1: a `DATE` start with no end lasts the day.
        date.tomorrow().ok().map(TimeValue::all_day)
    } else {
        None
    };
    match EventTimes::new(start.clone(), end) {
        Ok(times) => Some(times),
        Err(error) => {
            losses.push(Loss::new(
                Field::Times,
                LossReason::Malformed,
                format!("{error}; the end is dropped"),
            ));
            Some(EventTimes::at(start))
        }
    }
}

/// The end of an event `duration` after `start`: days are nominal, on the wall clock, and the rest
/// is exact (RFC 5545 section 3.3.6).
fn end_after(start: &TimeValue, duration: Duration) -> Option<TimeValue> {
    match start {
        TimeValue::AllDay { date } => {
            if duration.seconds % 86_400 != 0 {
                return None;
            }
            let days = duration.days + duration.seconds / 86_400;
            let days = if duration.negative { -days } else { days };
            date.checked_add(Span::new().try_days(days).ok()?)
                .ok()
                .map(TimeValue::all_day)
        }
        TimeValue::Floating { date_time } => date_time
            .checked_add(duration.span()?)
            .ok()
            .map(TimeValue::floating),
        TimeValue::Instant { timestamp } => timestamp
            .checked_add(duration.exact()?)
            .ok()
            .map(TimeValue::instant),
        TimeValue::Zoned {
            date_time,
            time_zone,
        } => {
            let span = duration.span()?;
            let end = match TimeZone::get(time_zone.as_str()) {
                Ok(zone) => date_time
                    .to_zoned(zone)
                    .ok()?
                    .checked_add(span)
                    .ok()?
                    .datetime(),
                Err(_) => date_time.checked_add(span).ok()?,
            };
            Some(TimeValue::zoned(end, time_zone.clone()))
        }
    }
}

/// A `DATE` or `DATE-TIME` property as a time value: `VALUE=DATE` or an eight-digit value is a
/// whole day, a `Z` an instant, a `TZID` a zoned time, and anything else floating.
pub(crate) fn time(
    line: &IcalLine<'_>,
    zones: &Zones,
    losses: &mut Vec<Loss>,
    field: &Field,
) -> Option<TimeValue> {
    let text = line::text(line);
    let value = time_text(
        text.trim(),
        line::param(line, "VALUE").as_deref(),
        line::param(line, "TZID").as_deref(),
        zones,
        losses,
        field,
    );
    if value.is_none() {
        losses.push(Loss::new(
            field.clone(),
            LossReason::Malformed,
            format!("{} `{text}` is not a date or date-time", line::name(line)),
        ));
    }
    value
}

/// One date or date-time value, as [`time`] reads it.
fn time_text(
    text: &str,
    value_type: Option<&str>,
    tzid: Option<&str>,
    zones: &Zones,
    losses: &mut Vec<Loss>,
    field: &Field,
) -> Option<TimeValue> {
    let date_typed = value_type.is_some_and(|value| value.eq_ignore_ascii_case("DATE"));
    if date_typed || !text.contains(['T', 't']) {
        return value::parse_date(text).map(TimeValue::all_day);
    }
    let (date_time, utc) = value::parse_date_time(text)?;
    if utc {
        let timestamp = date_time.to_zoned(TimeZone::UTC).ok()?.timestamp();
        return Some(TimeValue::instant(timestamp));
    }
    let Some(tzid) = tzid else {
        return Some(TimeValue::floating(date_time));
    };
    let time_zone = zones.resolve(tzid).unwrap_or_else(|written| {
        losses.push(Loss::new(
            field.clone(),
            LossReason::Unsupported,
            format!("TZID `{written}` names no IANA time zone; it is kept as written"),
        ));
        written
    });
    Some(TimeValue::zoned(date_time, time_zone))
}

/// The values of an `RDATE` or `EXDATE` line, and whether every value was read.
pub(crate) fn date_list(
    line: &IcalLine<'_>,
    zones: &Zones,
    losses: &mut Vec<Loss>,
) -> (Vec<TimeValue>, bool) {
    let value_type = line::param(line, "VALUE");
    let tzid = line::param(line, "TZID");
    let raw = line::raw(line);
    let mut values = Vec::new();
    let mut complete = true;
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let period = item.contains('/')
            || value_type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("PERIOD"));
        if period {
            complete = false;
            losses.push(Loss::new(
                Field::Recurrence,
                LossReason::Unrepresentable,
                format!(
                    "{} period `{item}` has no place in the model",
                    line::name(line)
                ),
            ));
            continue;
        }
        match time_text(
            item,
            value_type.as_deref(),
            tzid.as_deref(),
            zones,
            losses,
            &Field::Recurrence,
        ) {
            Some(value) => values.push(value),
            None => {
                complete = false;
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Malformed,
                    format!(
                        "{} value `{item}` is not a date or date-time",
                        line::name(line)
                    ),
                ));
            }
        }
    }
    (values, complete)
}

/// Reads an `RRULE` or `EXRULE` into `recurrence`.
fn rule_into(line: &IcalLine<'_>, recurrence: &mut Recurrence, losses: &mut Vec<Loss>) {
    let text = line::text(line);
    match value::parse_recur(text.trim()) {
        Ok((rule, notes)) => {
            for part in notes.dropped {
                losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Unrepresentable,
                    format!(
                        "{} part `{part}` has no place in the model",
                        line::name(line)
                    ),
                ));
            }
            if line::is(line, "RRULE") {
                recurrence.rules.push(rule);
            } else {
                recurrence.exclusion_rules.push(rule);
            }
        }
        Err(reason) => losses.push(Loss::new(
            Field::Recurrence,
            LossReason::Malformed,
            format!("{} `{text}`: {reason}", line::name(line)),
        )),
    }
}

/// A calendar address without its `mailto:` scheme.
pub(crate) fn address(value: &str) -> String {
    let value = value.trim();
    match value.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => value[7..].to_owned(),
        _ => value.to_owned(),
    }
}

/// An `ORGANIZER`.
fn organizer(line: &IcalLine<'_>) -> Organizer {
    Organizer {
        address: address(&line::text(line)),
        name: line::param(line, "CN"),
    }
}

/// An `ATTENDEE`.
pub(crate) fn attendee(line: &IcalLine<'_>, losses: &mut Vec<Loss>) -> Attendee {
    let mut attendee = Attendee::new(address(&line::text(line)));
    attendee.name = line::param(line, "CN");
    let mut approximated = |param: &str, value: &str| {
        losses.push(Loss::new(
            Field::Attendees,
            LossReason::Approximated,
            format!(
                "{param}={value} of {} is read as the default",
                attendee_label(line)
            ),
        ));
    };
    if let Some(role) = line::param(line, "ROLE") {
        attendee.role = match role.to_ascii_uppercase().as_str() {
            "REQ-PARTICIPANT" => AttendeeRole::Required,
            "OPT-PARTICIPANT" => AttendeeRole::Optional,
            "CHAIR" => AttendeeRole::Chair,
            "NON-PARTICIPANT" => AttendeeRole::NonParticipant,
            _ => {
                approximated("ROLE", &role);
                AttendeeRole::Required
            }
        };
    }
    if let Some(partstat) = line::param(line, "PARTSTAT") {
        attendee.participation = match partstat.to_ascii_uppercase().as_str() {
            "NEEDS-ACTION" => Participation::NeedsAction,
            "ACCEPTED" => Participation::Accepted,
            "DECLINED" => Participation::Declined,
            "TENTATIVE" => Participation::Tentative,
            "DELEGATED" => Participation::Delegated,
            _ => {
                approximated("PARTSTAT", &partstat);
                Participation::NeedsAction
            }
        };
    }
    if let Some(cutype) = line::param(line, "CUTYPE") {
        attendee.kind = match cutype.to_ascii_uppercase().as_str() {
            "INDIVIDUAL" => ParticipantKind::Individual,
            "GROUP" => ParticipantKind::Group,
            "RESOURCE" => ParticipantKind::Resource,
            "ROOM" => ParticipantKind::Room,
            "UNKNOWN" => ParticipantKind::Unknown,
            _ => {
                approximated("CUTYPE", &cutype);
                ParticipantKind::Unknown
            }
        };
    }
    attendee
}

/// Names an attendee line in a loss.
fn attendee_label(line: &IcalLine<'_>) -> String {
    format!("ATTENDEE `{}`", address(&line::text(line)))
}

/// A `STATUS`. RFC 5545 section 3.8.1.11 gives a `VEVENT` three; any other reads as confirmed.
fn event_status(value: &str, losses: &mut Vec<Loss>) -> EventStatus {
    match value.trim().to_ascii_uppercase().as_str() {
        "CONFIRMED" => EventStatus::Confirmed,
        "TENTATIVE" => EventStatus::Tentative,
        "CANCELLED" => EventStatus::Cancelled,
        other => {
            losses.push(Loss::new(
                Field::Status,
                LossReason::Approximated,
                format!("STATUS `{other}` is read as CONFIRMED"),
            ));
            EventStatus::Confirmed
        }
    }
}

/// A `TRANSP`.
fn transparency(value: &str, losses: &mut Vec<Loss>) -> Transparency {
    match value.trim().to_ascii_uppercase().as_str() {
        "OPAQUE" => Transparency::Opaque,
        "TRANSPARENT" => Transparency::Transparent,
        other => {
            losses.push(Loss::new(
                Field::Transparency,
                LossReason::Approximated,
                format!("TRANSP `{other}` is read as OPAQUE"),
            ));
            Transparency::Opaque
        }
    }
}

/// A `CLASS`. RFC 5545 section 3.8.1.3 treats a class it does not know as `PRIVATE`.
fn visibility(value: &str, losses: &mut Vec<Loss>) -> Visibility {
    match value.trim().to_ascii_uppercase().as_str() {
        "PUBLIC" => Visibility::Public,
        "PRIVATE" => Visibility::Private,
        "CONFIDENTIAL" => Visibility::Confidential,
        other => {
            losses.push(Loss::new(
                Field::Visibility,
                LossReason::Approximated,
                format!("CLASS `{other}` is read as PRIVATE"),
            ));
            Visibility::Private
        }
    }
}

/// A `VALARM`, where the model can hold it: a `DISPLAY` or `EMAIL` action, and a `TRIGGER`.
pub(crate) fn alarm(component: &IcalCst<'_>) -> (Option<Alert>, Vec<Loss>) {
    let mut losses = Vec::new();
    let action =
        line::first(component, "ACTION").map(|line| line::text(line).trim().to_ascii_uppercase());
    let action = match action.as_deref() {
        Some("DISPLAY") => AlertAction::Display,
        Some("EMAIL") => AlertAction::Email,
        other => {
            losses.push(Loss::new(
                Field::Reminders,
                LossReason::Unrepresentable,
                format!(
                    "a VALARM with ACTION `{}` has no place in the model",
                    other.unwrap_or("")
                ),
            ));
            return (None, losses);
        }
    };
    let Some(trigger_line) = line::first(component, "TRIGGER") else {
        losses.push(Loss::new(
            Field::Reminders,
            LossReason::Malformed,
            "a VALARM has no TRIGGER",
        ));
        return (None, losses);
    };
    let text = line::text(trigger_line);
    let text = text.trim();
    let date_time_typed = line::param(trigger_line, "VALUE")
        .is_some_and(|value| value.eq_ignore_ascii_case("DATE-TIME"));
    let trigger = if date_time_typed || text.contains(['T', 't']) && !text.contains(['P', 'p']) {
        value::parse_date_time(text)
            .filter(|(_, utc)| *utc)
            .and_then(|(date_time, _)| date_time.to_zoned(TimeZone::UTC).ok())
            .map(|zoned| AlertTrigger::Absolute {
                at: zoned.timestamp(),
            })
    } else {
        value::parse_duration(text)
            .and_then(Duration::exact)
            .map(|offset| AlertTrigger::Offset {
                offset,
                relative_to: match line::param(trigger_line, "RELATED") {
                    Some(related) if related.eq_ignore_ascii_case("END") => RelativeTo::End,
                    _ => RelativeTo::Start,
                },
            })
    };
    let Some(trigger) = trigger else {
        losses.push(Loss::new(
            Field::Reminders,
            LossReason::Malformed,
            format!("VALARM TRIGGER `{text}` is not a duration or a UTC date-time"),
        ));
        return (None, losses);
    };
    if line::first(component, "REPEAT").is_some() {
        losses.push(Loss::new(
            Field::Reminders,
            LossReason::Unrepresentable,
            "a VALARM's REPEAT and DURATION have no place in the model",
        ));
    }
    (Some(Alert { trigger, action }), losses)
}

/// A `VEVENT` read, and where it is: the calendar and the item within it.
#[derive(Debug)]
pub(crate) struct Located {
    pub(crate) calendar: usize,
    pub(crate) item: usize,
    pub(crate) read: Component,
}

/// The `VEVENT`s sharing one `UID`.
#[derive(Debug)]
pub(crate) struct Group {
    pub(crate) uid: String,
    /// The master, where it reads.
    pub(crate) master: Option<Located>,
    /// The overrides, by instance.
    pub(crate) overrides: BTreeMap<InstanceKey, Located>,
    /// `VEVENT`s with this `UID` that could not be read as a master or an override: no `DTSTART`,
    /// a malformed `RECURRENCE-ID`, or a duplicate. Writing never touches them.
    pub(crate) unread: Vec<(usize, usize)>,
    pub(crate) losses: Vec<Loss>,
}

/// Groups every `VEVENT` directly under each calendar by `UID`, in order of first appearance.
/// Returns the groups and the losses of `VEVENT`s with no `UID`.
pub(crate) fn groups(calendars: &[IcalCst<'_>]) -> (Vec<Group>, Vec<Loss>) {
    let mut groups: Vec<Group> = Vec::new();
    let mut by_uid: BTreeMap<String, usize> = BTreeMap::new();
    let mut losses = Vec::new();
    for (calendar_at, calendar) in calendars.iter().enumerate() {
        let zones = Zones::of(calendar);
        for (item, entry) in calendar.items.iter().enumerate() {
            let IcalItem::Component(component) = entry else {
                continue;
            };
            if !line::component_is(component, "VEVENT") {
                continue;
            }
            let read = event(component, &zones);
            let Some(uid) = read.uid.clone().filter(|uid| !uid.is_empty()) else {
                losses.push(Loss::new(
                    Field::Other("UID".to_owned()),
                    LossReason::Malformed,
                    "a VEVENT has no UID and is not read",
                ));
                continue;
            };
            let at = *by_uid.entry(uid.clone()).or_insert_with(|| {
                groups.push(Group {
                    uid,
                    master: None,
                    overrides: BTreeMap::new(),
                    unread: Vec::new(),
                    losses: Vec::new(),
                });
                groups.len() - 1
            });
            place(
                &mut groups[at],
                Located {
                    calendar: calendar_at,
                    item,
                    read,
                },
            );
        }
    }
    (groups, losses)
}

/// Places one read `VEVENT` in its group.
fn place(group: &mut Group, mut located: Located) {
    group.losses.append(&mut located.read.losses);
    if located.read.event.is_none() {
        group.unread.push((located.calendar, located.item));
        return;
    }
    match located.read.instance.clone() {
        None => {
            if group.master.is_some() {
                group.losses.push(Loss::new(
                    Field::Other("VEVENT".to_owned()),
                    LossReason::Malformed,
                    format!("a second master VEVENT for UID `{}` is not read", group.uid),
                ));
                group.unread.push((located.calendar, located.item));
            } else {
                group.master = Some(located);
            }
        }
        Some(None) => group.unread.push((located.calendar, located.item)),
        Some(Some((key, _))) => {
            if let Some(event) = located.read.event.as_mut()
                && event.recurrence.take().is_some()
            {
                group.losses.push(Loss::new(
                    Field::Recurrence,
                    LossReason::Unrepresentable,
                    "an override VEVENT's own recurrence has no place in the model",
                ));
            }
            match group.overrides.entry(key) {
                Entry::Vacant(slot) => {
                    slot.insert(located);
                }
                Entry::Occupied(slot) => {
                    group.losses.push(Loss::new(
                        Field::Overrides,
                        LossReason::Malformed,
                        format!(
                            "a second override of {:?} for UID `{}` is not read",
                            slot.key().original_start(),
                            group.uid
                        ),
                    ));
                    group.unread.push((located.calendar, located.item));
                }
            }
        }
    }
}
