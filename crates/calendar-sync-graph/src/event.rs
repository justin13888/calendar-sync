//! One Graph `event` resource to and from a [`ProviderEvent`].
//!
//! Property names and types follow `references/graph/event.md`.

use core::fmt;

use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, EventTimes, Field, Loss,
    LossReason, MapperVersion, NormalizedEvent, Organizer, ParticipantKind, Participation,
    Passthrough, Provider, ProviderEvent, ProviderIds, ProviderRevision, RelativeTo, Reminders,
    Transparency, Visibility,
};
use jiff::SignedDuration;
use serde_json::{Map, Value, json};

use crate::{recurrence, time};

/// The version of this crate's mapping. It changes whenever the same Graph JSON would read into
/// different fields.
pub const MAPPER_VERSION: MapperVersion = MapperVersion::new(1);

/// The top-level properties this mapping reads. Every other property is kept in the record's
/// [`Passthrough`] under its own name.
const MAPPED: [&str; 18] = [
    "id",
    "changeKey",
    "iCalUId",
    "seriesMasterId",
    "subject",
    "body",
    "location",
    "start",
    "end",
    "isAllDay",
    "recurrence",
    "showAs",
    "isCancelled",
    "sensitivity",
    "organizer",
    "attendees",
    "isReminderOn",
    "reminderMinutesBeforeStart",
];

/// The attendee properties this mapping reads; the rest are kept as `attendees[ADDRESS].NAME`.
const ATTENDEE_MAPPED: [&str; 3] = ["emailAddress", "type", "status"];

/// What a Graph event resource, or a row of a delta page, is.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "an item is moved once into a page or an outcome"
)]
pub enum Item {
    /// An event: a single event, a series master, or an occurrence or exception of a series,
    /// whose [`ProviderIds::series_id`] is set.
    Event(ProviderEvent),
    /// A delta row marked `@removed`. Graph also marks this way an event that left the delta's
    /// window, so a removal is confirmed with a read before it is believed.
    Removed {
        /// The event id.
        id: String,
    },
}

/// What [`read`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadEvent {
    /// The resource.
    pub item: Item,
    /// What the model could not carry.
    pub losses: Vec<Loss>,
}

/// What [`write()`] produced.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenEvent {
    /// The `event` JSON object: every property this mapping writes, and no other.
    pub body: Map<String, Value>,
    /// What Graph could not carry.
    pub losses: Vec<Loss>,
}

impl WrittenEvent {
    /// Whether the recurrence could not be written: Graph would hold a different series. A
    /// write that carries it is refused rather than sent.
    #[must_use]
    pub fn refuses_recurrence(&self) -> bool {
        self.losses
            .iter()
            .any(|loss| loss.field == Field::Recurrence && loss.reason != LossReason::Approximated)
    }
}

/// Why a resource could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError {
    /// What was wrong.
    pub message: String,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a Graph event: {}", self.message)
    }
}

impl core::error::Error for MapError {}

fn error(message: impl Into<String>) -> MapError {
    MapError {
        message: message.into(),
    }
}

fn string(object: &Map<String, Value>, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The address and name of an `emailAddress` property.
fn email(value: Option<&Value>) -> Option<(String, Option<String>)> {
    let email = value?.get("emailAddress")?.as_object()?;
    Some((string(email, "address")?, string(email, "name")))
}

/// Reads one Graph `event`, or one row of a delta page.
///
/// # Errors
///
/// [`MapError`] when the value is not an object, has no `id`, or has no readable `start`.
pub fn read(value: &Value) -> Result<ReadEvent, MapError> {
    let object = value.as_object().ok_or_else(|| error("not an object"))?;
    let id = string(object, "id").ok_or_else(|| error("no id"))?;
    if object.contains_key("@removed") {
        return Ok(ReadEvent {
            item: Item::Removed { id },
            losses: Vec::new(),
        });
    }

    let mut losses = Vec::new();
    let all_day = object.get("isAllDay").and_then(Value::as_bool) == Some(true);
    let zone = |name: &str| object.get(name).and_then(Value::as_str);
    let start = time::read(
        object.get("start").ok_or_else(|| error("no start"))?,
        zone("originalStartTimeZone"),
        all_day,
        "start",
        &mut losses,
    )
    .map_err(error)?;
    let end = match object.get("end") {
        Some(end) => match time::read(
            end,
            zone("originalEndTimeZone"),
            all_day,
            "end",
            &mut losses,
        ) {
            Ok(end) => Some(end),
            Err(message) => {
                losses.push(Loss::new(Field::Times, LossReason::Malformed, message));
                None
            }
        },
        None => None,
    };
    let times = EventTimes::new(start.clone(), end).unwrap_or_else(|problem| {
        losses.push(Loss::new(
            Field::Times,
            LossReason::Malformed,
            format!("end dropped: {problem}"),
        ));
        EventTimes::at(start.clone())
    });

    let mut fields = NormalizedEvent::new(times);
    fields.title = string(object, "subject");
    if let Some(body) = object.get("body").and_then(Value::as_object) {
        fields.description = string(body, "content");
        if fields.description.is_some()
            && body
                .get("contentType")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("html"))
        {
            losses.push(Loss::new(
                Field::Description,
                LossReason::Approximated,
                "an HTML body, read as its markup",
            ));
        }
    }
    let mut passthrough = Passthrough::new();
    if let Some(location) = object.get("location").and_then(Value::as_object) {
        fields.location = string(location, "displayName");
        for (name, value) in location {
            if name != "displayName" {
                passthrough.push(format!("location.{name}"), value.to_string());
            }
        }
    }
    let show_as = object.get("showAs").and_then(Value::as_str);
    fields.transparency = match show_as {
        Some("free") => Transparency::Transparent,
        None | Some("busy" | "tentative" | "oof" | "workingElsewhere" | "unknown") => {
            Transparency::Opaque
        }
        Some(other) => {
            losses.push(unknown(Field::Transparency, other));
            Transparency::Opaque
        }
    };
    fields.status = if object.get("isCancelled").and_then(Value::as_bool) == Some(true) {
        EventStatus::Cancelled
    } else if show_as == Some("tentative") {
        EventStatus::Tentative
    } else {
        EventStatus::Confirmed
    };
    fields.visibility = match object.get("sensitivity").and_then(Value::as_str) {
        None | Some("normal") => Visibility::Default,
        Some("private") => Visibility::Private,
        Some("confidential") => Visibility::Confidential,
        Some("personal") => {
            losses.push(Loss::new(
                Field::Visibility,
                LossReason::Approximated,
                "personal, read as private",
            ));
            Visibility::Private
        }
        Some(other) => {
            losses.push(unknown(Field::Visibility, other));
            Visibility::Default
        }
    };
    if let Some(pattern) = object.get("recurrence").filter(|value| !value.is_null()) {
        fields.recurrence = recurrence::read(pattern, &start, &mut losses);
    }
    fields.organizer =
        email(object.get("organizer")).map(|(address, name)| Organizer { address, name });
    for attendee in object
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let (read, extras) = read_attendee(attendee, &mut losses);
        for (name, value) in extras {
            passthrough.push(format!("attendees[{}].{name}", read.address), value);
        }
        fields.attendees.push(read);
    }
    if object.get("isReminderOn").and_then(Value::as_bool) == Some(true) {
        match object
            .get("reminderMinutesBeforeStart")
            .and_then(Value::as_i64)
        {
            Some(minutes) => fields.reminders.alerts.push(Alert {
                trigger: AlertTrigger::Offset {
                    offset: SignedDuration::from_mins(-minutes),
                    relative_to: RelativeTo::Start,
                },
                action: AlertAction::Display,
            }),
            None => losses.push(Loss::new(
                Field::Reminders,
                LossReason::Malformed,
                "a reminder with no reminderMinutesBeforeStart",
            )),
        }
    }
    for (name, value) in object {
        if !MAPPED.contains(&name.as_str()) {
            passthrough.push(name.clone(), value.to_string());
        }
    }

    Ok(ReadEvent {
        item: Item::Event(ProviderEvent {
            ids: ProviderIds {
                provider: Provider::Graph,
                id,
                ical_uid: string(object, "iCalUId"),
                series_id: string(object, "seriesMasterId"),
            },
            revision: string(object, "changeKey").map(ProviderRevision::new),
            mapper_version: MAPPER_VERSION,
            fields,
            passthrough,
        }),
        losses,
    })
}

fn unknown(field: Field, value: &str) -> Loss {
    Loss::new(
        field,
        LossReason::Unsupported,
        format!("unknown value {value:?}"),
    )
}

fn read_attendee(
    attendee: &Map<String, Value>,
    losses: &mut Vec<Loss>,
) -> (Attendee, Vec<(String, String)>) {
    let (address, name) = email(Some(&Value::Object(attendee.clone()))).unwrap_or_default();
    let mut read = Attendee::new(address);
    read.name = name;
    match attendee.get("type").and_then(Value::as_str) {
        None | Some("required") => {}
        Some("optional") => read.role = AttendeeRole::Optional,
        Some("resource") => read.kind = ParticipantKind::Resource,
        Some(other) => losses.push(unknown(Field::Attendees, other)),
    }
    read.participation = match attendee
        .get("status")
        .and_then(|status| status.get("response"))
        .and_then(Value::as_str)
    {
        None | Some("none" | "notResponded") => Participation::NeedsAction,
        Some("accepted" | "organizer") => Participation::Accepted,
        Some("declined") => Participation::Declined,
        Some("tentativelyAccepted") => Participation::Tentative,
        Some(other) => {
            losses.push(unknown(Field::Attendees, other));
            Participation::NeedsAction
        }
    };
    let extras = attendee
        .iter()
        .filter(|(name, _)| !ATTENDEE_MAPPED.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.to_string()))
        .collect();
    (read, extras)
}

/// Writes `event` as a Graph `event` JSON object.
///
/// The body holds every property this mapping writes, always, so two bodies can be compared
/// property by property: the seam patches only those that differ from the event as Graph holds
/// it. Nothing from the record's [`Passthrough`] is written: a `PATCH` leaves every property it
/// does not name as it was. An attendee's reply is not written either; Graph changes it only
/// through `accept`, `decline` and `tentativelyAccept`.
///
/// What Graph cannot hold (a floating time, a chair, a cancelled status, a second reminder) is
/// written as the nearest value and reported. A recurrence Graph cannot hold is written as `null`
/// and reported; see [`WrittenEvent::refuses_recurrence`].
#[must_use]
pub fn write(event: &ProviderEvent) -> WrittenEvent {
    let mut losses = Vec::new();
    let mut body = Map::new();
    let fields = &event.fields;
    let text = |value: Option<&String>| json!(value.map_or("", String::as_str));

    body.insert("subject".to_owned(), text(fields.title.as_ref()));
    body.insert(
        "body".to_owned(),
        json!({ "contentType": "text", "content": text(fields.description.as_ref()) }),
    );
    body.insert(
        "location".to_owned(),
        json!({ "displayName": text(fields.location.as_ref()) }),
    );

    let start = fields.times.start();
    let end = fields
        .times
        .end()
        .cloned()
        .unwrap_or_else(|| time::implied_end(start, &mut losses));
    body.insert("start".to_owned(), time::write(start, &mut losses));
    body.insert("end".to_owned(), time::write(&end, &mut losses));
    body.insert("isAllDay".to_owned(), json!(fields.times.is_all_day()));
    let pattern = fields
        .recurrence
        .as_ref()
        .and_then(|rule| recurrence::write(rule, start, &mut losses));
    body.insert("recurrence".to_owned(), pattern.unwrap_or(Value::Null));

    let show_as = match (fields.transparency, fields.status) {
        (Transparency::Transparent, _) => "free",
        (Transparency::Opaque, EventStatus::Tentative) => "tentative",
        (Transparency::Opaque, _) => "busy",
    };
    if fields.status == EventStatus::Cancelled {
        losses.push(Loss::new(
            Field::Status,
            LossReason::Unrepresentable,
            "Graph cancels an event only through its cancel action or a delete",
        ));
    }
    body.insert("showAs".to_owned(), json!(show_as));
    let sensitivity = match fields.visibility {
        Visibility::Default => "normal",
        Visibility::Public => {
            losses.push(Loss::new(
                Field::Visibility,
                LossReason::Approximated,
                "Graph has no public sensitivity; written as normal",
            ));
            "normal"
        }
        Visibility::Private => "private",
        Visibility::Confidential => "confidential",
    };
    body.insert("sensitivity".to_owned(), json!(sensitivity));

    body.insert(
        "organizer".to_owned(),
        fields.organizer.as_ref().map_or(Value::Null, |organizer| {
            json!({ "emailAddress": address(&organizer.address, organizer.name.as_ref()) })
        }),
    );
    let attendees = fields
        .attendees
        .iter()
        .map(|attendee| write_attendee(attendee, &mut losses))
        .collect();
    body.insert("attendees".to_owned(), Value::Array(attendees));

    let minutes = write_reminders(&fields.reminders, &mut losses);
    body.insert("isReminderOn".to_owned(), json!(minutes.is_some()));
    if let Some(minutes) = minutes {
        body.insert("reminderMinutesBeforeStart".to_owned(), json!(minutes));
    }

    WrittenEvent { body, losses }
}

fn address(address: &str, name: Option<&String>) -> Value {
    let mut object = Map::new();
    object.insert("address".to_owned(), json!(address));
    if let Some(name) = name {
        object.insert("name".to_owned(), json!(name));
    }
    Value::Object(object)
}

fn write_attendee(attendee: &Attendee, losses: &mut Vec<Loss>) -> Value {
    let mut approximated = |detail: &str| {
        losses.push(Loss::new(
            Field::Attendees,
            LossReason::Approximated,
            format!("{}: {detail}", attendee.address),
        ));
    };
    let kind = match (attendee.kind, attendee.role) {
        (ParticipantKind::Resource, _) => "resource",
        (ParticipantKind::Room, _) => {
            approximated("Graph has no room attendee; written as a resource");
            "resource"
        }
        (ParticipantKind::Group, _) => {
            approximated("Graph has no group attendee; written as an individual");
            "required"
        }
        (_, AttendeeRole::Required) => "required",
        (_, AttendeeRole::Optional) => "optional",
        (_, AttendeeRole::Chair) => {
            approximated("Graph has no chair; written as required");
            "required"
        }
        (_, AttendeeRole::NonParticipant) => {
            approximated("Graph has no non-participant; written as optional");
            "optional"
        }
    };
    json!({
        "emailAddress": address(&attendee.address, attendee.name.as_ref()),
        "type": kind,
    })
}

/// The minutes before the start Graph's one reminder fires, or `None` for no reminder.
fn write_reminders(reminders: &Reminders, losses: &mut Vec<Loss>) -> Option<i64> {
    let mut lose = |reason: LossReason, detail: &str| {
        losses.push(Loss::new(Field::Reminders, reason, detail.to_owned()));
    };
    if reminders.use_default {
        lose(
            LossReason::Unrepresentable,
            "Graph has no default reminders; the event's own are written",
        );
    }
    let mut minutes = None;
    for alert in &reminders.alerts {
        let AlertTrigger::Offset {
            offset,
            relative_to: RelativeTo::Start,
        } = alert.trigger
        else {
            lose(
                LossReason::Unrepresentable,
                "Graph reminds only at an offset before the start",
            );
            continue;
        };
        let seconds = offset.as_secs();
        if seconds > 0 || seconds % 60 != 0 || offset.subsec_nanos() != 0 {
            lose(
                LossReason::Unrepresentable,
                "Graph reminds only a whole number of minutes before the start",
            );
            continue;
        }
        if alert.action != AlertAction::Display {
            lose(
                LossReason::Approximated,
                "Graph reminds only on the device; written as a display reminder",
            );
        }
        if minutes.is_some() {
            lose(
                LossReason::Unrepresentable,
                "Graph holds one reminder; the first is written",
            );
            continue;
        }
        minutes = Some(-seconds / 60);
    }
    minutes
}

#[cfg(test)]
mod tests;
