//! One Google `Event` resource to and from a [`ProviderEvent`].
//!
//! Field names and types follow the `Event` schema of `references/google/calendar-v3-discovery.json`.

use core::fmt;

use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, EventTimes, Field, Loss,
    LossReason, MapperVersion, NormalizedEvent, Organizer, ParticipantKind, Participation,
    Passthrough, Provider, ProviderEvent, ProviderIds, ProviderRevision, RelativeTo, Reminders,
    Transparency, Visibility,
};
use jiff::SignedDuration;
use serde_json::{Map, Value};

use crate::{recurrence, time};

/// The version of this crate's mapping. It changes whenever the same Google JSON would read into
/// different fields.
pub const MAPPER_VERSION: MapperVersion = MapperVersion::new(1);

/// The top-level fields this mapping reads. Every other field is kept in the record's
/// [`Passthrough`] under its own name, and written back unchanged.
const MAPPED: [&str; 16] = [
    "id",
    "etag",
    "iCalUID",
    "recurringEventId",
    "summary",
    "description",
    "location",
    "status",
    "transparency",
    "visibility",
    "start",
    "end",
    "recurrence",
    "organizer",
    "attendees",
    "reminders",
];

/// The organizer fields this mapping reads; the rest are kept as `organizer.NAME`.
const ORGANIZER_MAPPED: [&str; 2] = ["email", "displayName"];

/// The attendee fields this mapping reads; the rest are kept as `attendees[EMAIL].NAME`.
const ATTENDEE_MAPPED: [&str; 5] = [
    "email",
    "displayName",
    "optional",
    "resource",
    "responseStatus",
];

/// What a Google event resource is.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "an item is moved once into a page or an outcome"
)]
pub enum Item {
    /// An event, or a cancelled instance of a recurring series, which Google keeps as a resource
    /// of its own: its [`ProviderIds::series_id`] is set and its status is
    /// [`EventStatus::Cancelled`].
    Event(ProviderEvent),
    /// A deleted event: `status` is `cancelled` and it belongs to no series. Google reports these
    /// in an incremental list.
    Deleted {
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
    /// The `Event` JSON object, without `id` or `etag`.
    pub body: Map<String, Value>,
    /// What Google could not carry.
    pub losses: Vec<Loss>,
}

/// Why a resource could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError {
    /// What was wrong.
    pub message: String,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a Google event: {}", self.message)
    }
}

impl core::error::Error for MapError {}

fn error(message: impl Into<String>) -> MapError {
    MapError {
        message: message.into(),
    }
}

fn string(object: &Map<String, Value>, name: &str) -> Option<String> {
    object.get(name).and_then(Value::as_str).map(str::to_owned)
}

/// Reads one Google `Event` resource.
///
/// # Errors
///
/// [`MapError`] when the value is not an object, has no `id`, or a live event has no readable
/// `start`.
pub fn read(value: &Value) -> Result<ReadEvent, MapError> {
    let object = value.as_object().ok_or_else(|| error("not an object"))?;
    let id = string(object, "id").ok_or_else(|| error("no id"))?;
    let series_id = string(object, "recurringEventId");
    let cancelled = object.get("status").and_then(Value::as_str) == Some("cancelled");
    if cancelled && series_id.is_none() {
        return Ok(ReadEvent {
            item: Item::Deleted { id },
            losses: Vec::new(),
        });
    }

    let mut losses = Vec::new();
    let mut lossy =
        |message: String| losses.push(Loss::new(Field::Times, LossReason::Malformed, message));
    let mut time_losses = Vec::new();
    // A cancelled instance may carry only its `originalStartTime`.
    let start = match object.get("start") {
        Some(start) => time::read(start, "start", &mut time_losses).map_err(error)?,
        None if cancelled => object
            .get("originalStartTime")
            .ok_or_else(|| error("a cancelled instance has no originalStartTime"))
            .and_then(|original| {
                time::read(original, "originalStartTime", &mut time_losses).map_err(error)
            })?,
        None => return Err(error("no start")),
    };
    let end = match object.get("end") {
        Some(end) => match time::read(end, "end", &mut time_losses) {
            Ok(end) => Some(end),
            Err(message) => {
                lossy(message);
                None
            }
        },
        None => None,
    };
    let times = match EventTimes::new(start.clone(), end) {
        Ok(times) => times,
        Err(problem) => {
            lossy(format!("end dropped: {problem}"));
            EventTimes::at(start.clone())
        }
    };
    losses.append(&mut time_losses);

    let mut fields = NormalizedEvent::new(times);
    fields.title = string(object, "summary");
    fields.description = string(object, "description");
    fields.location = string(object, "location");
    fields.status = match object.get("status").and_then(Value::as_str) {
        None | Some("confirmed") => EventStatus::Confirmed,
        Some("tentative") => EventStatus::Tentative,
        Some("cancelled") => EventStatus::Cancelled,
        Some(other) => {
            losses.push(unknown(Field::Status, other));
            EventStatus::Confirmed
        }
    };
    fields.transparency = match object.get("transparency").and_then(Value::as_str) {
        None | Some("opaque") => Transparency::Opaque,
        Some("transparent") => Transparency::Transparent,
        Some(other) => {
            losses.push(unknown(Field::Transparency, other));
            Transparency::Opaque
        }
    };
    fields.visibility = match object.get("visibility").and_then(Value::as_str) {
        None | Some("default") => Visibility::Default,
        Some("public") => Visibility::Public,
        Some("private") => Visibility::Private,
        Some("confidential") => Visibility::Confidential,
        Some(other) => {
            losses.push(unknown(Field::Visibility, other));
            Visibility::Default
        }
    };
    if let Some(lines) = object.get("recurrence").and_then(Value::as_array) {
        let lines: Vec<String> = lines
            .iter()
            .filter_map(|line| line.as_str().map(str::to_owned))
            .collect();
        fields.recurrence = recurrence::read(&lines, &start, &mut losses);
    }

    let mut passthrough = Passthrough::new();
    if let Some(organizer) = object.get("organizer").and_then(Value::as_object) {
        fields.organizer = string(organizer, "email").map(|address| Organizer {
            address,
            name: string(organizer, "displayName"),
        });
        for (name, value) in organizer {
            if !ORGANIZER_MAPPED.contains(&name.as_str()) {
                passthrough.push(format!("organizer.{name}"), value.to_string());
            }
        }
    }
    if let Some(attendees) = object.get("attendees").and_then(Value::as_array) {
        for attendee in attendees.iter().filter_map(Value::as_object) {
            let (read, extras) = read_attendee(attendee, &mut losses);
            for (name, value) in extras {
                passthrough.push(format!("attendees[{}].{name}", read.address), value);
            }
            fields.attendees.push(read);
        }
    }
    if let Some(reminders) = object.get("reminders").and_then(Value::as_object) {
        fields.reminders = read_reminders(reminders, &mut losses);
    }
    for (name, value) in object {
        if !MAPPED.contains(&name.as_str()) {
            passthrough.push(name.clone(), value.to_string());
        }
    }

    Ok(ReadEvent {
        item: Item::Event(ProviderEvent {
            ids: ProviderIds {
                provider: Provider::Google,
                id,
                ical_uid: string(object, "iCalUID"),
                series_id,
            },
            revision: string(object, "etag").map(ProviderRevision::new),
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
    let mut read = Attendee::new(string(attendee, "email").unwrap_or_default());
    read.name = string(attendee, "displayName");
    if attendee.get("optional").and_then(Value::as_bool) == Some(true) {
        read.role = AttendeeRole::Optional;
    }
    if attendee.get("resource").and_then(Value::as_bool) == Some(true) {
        read.kind = ParticipantKind::Resource;
    }
    read.participation = match attendee.get("responseStatus").and_then(Value::as_str) {
        None | Some("needsAction") => Participation::NeedsAction,
        Some("accepted") => Participation::Accepted,
        Some("declined") => Participation::Declined,
        Some("tentative") => Participation::Tentative,
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

fn read_reminders(reminders: &Map<String, Value>, losses: &mut Vec<Loss>) -> Reminders {
    let mut read = Reminders {
        use_default: reminders
            .get("useDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        alerts: Vec::new(),
    };
    for reminder in reminders
        .get("overrides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let method = reminder.get("method").and_then(Value::as_str);
        let minutes = reminder.get("minutes").and_then(Value::as_i64);
        let action = match method {
            Some("popup") => AlertAction::Display,
            Some("email") => AlertAction::Email,
            other => {
                losses.push(Loss::new(
                    Field::Reminders,
                    LossReason::Unsupported,
                    format!("reminder method {other:?}"),
                ));
                continue;
            }
        };
        let Some(minutes) = minutes else {
            losses.push(Loss::new(
                Field::Reminders,
                LossReason::Malformed,
                "a reminder has no minutes",
            ));
            continue;
        };
        read.alerts.push(Alert {
            trigger: AlertTrigger::Offset {
                offset: SignedDuration::from_mins(-minutes),
                relative_to: RelativeTo::Start,
            },
            action,
        });
    }
    read
}

/// Writes `event` as a Google `Event` JSON object.
///
/// The record's [`Passthrough`] is written back first, so every field this mapping does not read
/// survives a read-modify-write unchanged; the mapped fields are written over it. A record read
/// by another provider's mapper carries no Google fields, and its passthrough is not written.
#[must_use]
pub fn write(event: &ProviderEvent) -> WrittenEvent {
    let mut losses = Vec::new();
    let mut body = Map::new();
    let mut organizer_extras = Map::new();
    let mut attendee_extras: Vec<(String, String, Value)> = Vec::new();
    if event.ids.provider == Provider::Google {
        for field in &event.passthrough {
            let Ok(value) = serde_json::from_str::<Value>(&field.raw) else {
                losses.push(Loss::new(
                    Field::Other(field.name.clone()),
                    LossReason::Malformed,
                    "the kept value is not JSON",
                ));
                continue;
            };
            if let Some(name) = field.name.strip_prefix("organizer.") {
                organizer_extras.insert(name.to_owned(), value);
            } else if let Some((address, name)) = field
                .name
                .strip_prefix("attendees[")
                .and_then(|rest| rest.rsplit_once("]."))
            {
                attendee_extras.push((address.to_owned(), name.to_owned(), value));
            } else {
                body.insert(field.name.clone(), value);
            }
        }
    }

    let fields = &event.fields;
    let set = |body: &mut Map<String, Value>, name: &str, value: Option<&String>| match value {
        Some(value) => {
            body.insert(name.to_owned(), Value::String(value.clone()));
        }
        None => {
            body.remove(name);
        }
    };
    set(&mut body, "summary", fields.title.as_ref());
    set(&mut body, "description", fields.description.as_ref());
    set(&mut body, "location", fields.location.as_ref());

    let start = fields.times.start();
    let end = fields
        .times
        .end()
        .cloned()
        .unwrap_or_else(|| time::implied_end(start, &mut losses));
    let write_time = if fields.recurrence.is_some() {
        time::write_recurring
    } else {
        time::write
    };
    body.insert(
        "start".to_owned(),
        write_time(start, Field::Times, &mut losses),
    );
    body.insert(
        "end".to_owned(),
        write_time(&end, Field::Times, &mut losses),
    );
    match &fields.recurrence {
        Some(rule) => {
            let lines = recurrence::write(rule, start, &mut losses);
            body.insert(
                "recurrence".to_owned(),
                Value::Array(lines.into_iter().map(Value::String).collect()),
            );
        }
        None => {
            body.remove("recurrence");
        }
    }

    body.insert(
        "status".to_owned(),
        Value::String(
            match fields.status {
                EventStatus::Confirmed => "confirmed",
                EventStatus::Tentative => "tentative",
                EventStatus::Cancelled => "cancelled",
            }
            .to_owned(),
        ),
    );
    body.insert(
        "transparency".to_owned(),
        Value::String(
            match fields.transparency {
                Transparency::Opaque => "opaque",
                Transparency::Transparent => "transparent",
            }
            .to_owned(),
        ),
    );
    body.insert(
        "visibility".to_owned(),
        Value::String(
            match fields.visibility {
                Visibility::Default => "default",
                Visibility::Public => "public",
                Visibility::Private => "private",
                Visibility::Confidential => "confidential",
            }
            .to_owned(),
        ),
    );

    match &fields.organizer {
        Some(organizer) => {
            let mut object = organizer_extras;
            object.insert("email".to_owned(), Value::String(organizer.address.clone()));
            match &organizer.name {
                Some(name) => {
                    object.insert("displayName".to_owned(), Value::String(name.clone()));
                }
                None => {
                    object.remove("displayName");
                }
            }
            body.insert("organizer".to_owned(), Value::Object(object));
        }
        None => {
            body.remove("organizer");
        }
    }

    if fields.attendees.is_empty() {
        body.remove("attendees");
    } else {
        let attendees = fields
            .attendees
            .iter()
            .map(|attendee| {
                let mut object: Map<String, Value> = attendee_extras
                    .iter()
                    .filter(|(address, _, _)| *address == attendee.address)
                    .map(|(_, name, value)| (name.clone(), value.clone()))
                    .collect();
                write_attendee(attendee, &mut object, &mut losses);
                Value::Object(object)
            })
            .collect();
        body.insert("attendees".to_owned(), Value::Array(attendees));
    }

    body.insert(
        "reminders".to_owned(),
        write_reminders(&fields.reminders, &mut losses),
    );

    WrittenEvent { body, losses }
}

fn write_attendee(attendee: &Attendee, object: &mut Map<String, Value>, losses: &mut Vec<Loss>) {
    let mut approximated = |detail: &str| {
        losses.push(Loss::new(
            Field::Attendees,
            LossReason::Approximated,
            format!("{}: {detail}", attendee.address),
        ));
    };
    object.insert("email".to_owned(), Value::String(attendee.address.clone()));
    match &attendee.name {
        Some(name) => {
            object.insert("displayName".to_owned(), Value::String(name.clone()));
        }
        None => {
            object.remove("displayName");
        }
    }
    let optional = match attendee.role {
        AttendeeRole::Required => false,
        AttendeeRole::Optional => true,
        AttendeeRole::Chair => {
            approximated("Google has no chair; written as required");
            false
        }
        AttendeeRole::NonParticipant => {
            approximated("Google has no non-participant; written as optional");
            true
        }
    };
    if optional {
        object.insert("optional".to_owned(), Value::Bool(true));
    } else {
        object.remove("optional");
    }
    let resource = match attendee.kind {
        ParticipantKind::Individual | ParticipantKind::Unknown => false,
        ParticipantKind::Resource => true,
        ParticipantKind::Room => {
            approximated("Google has no room; written as a resource");
            true
        }
        ParticipantKind::Group => {
            approximated("Google has no group; written as an individual");
            false
        }
    };
    if resource {
        object.insert("resource".to_owned(), Value::Bool(true));
    } else {
        object.remove("resource");
    }
    let status = match attendee.participation {
        Participation::NeedsAction => "needsAction",
        Participation::Accepted => "accepted",
        Participation::Declined => "declined",
        Participation::Tentative => "tentative",
        Participation::Delegated => {
            approximated("Google has no delegated reply; written as needsAction");
            "needsAction"
        }
    };
    object.insert(
        "responseStatus".to_owned(),
        Value::String(status.to_owned()),
    );
}

fn write_reminders(reminders: &Reminders, losses: &mut Vec<Loss>) -> Value {
    let mut object = Map::new();
    object.insert("useDefault".to_owned(), Value::Bool(reminders.use_default));
    if reminders.use_default {
        if !reminders.alerts.is_empty() {
            losses.push(Loss::new(
                Field::Reminders,
                LossReason::Unrepresentable,
                "Google cannot hold the default reminders and the event's own together",
            ));
        }
        return Value::Object(object);
    }
    let mut overrides = Vec::new();
    for alert in &reminders.alerts {
        let mut drop = |detail: &str| {
            losses.push(Loss::new(
                Field::Reminders,
                LossReason::Unrepresentable,
                detail.to_owned(),
            ));
        };
        let method = match alert.action {
            AlertAction::Display => "popup",
            AlertAction::Email => "email",
            _ => {
                drop("Google reminds only by popup or email");
                continue;
            }
        };
        let AlertTrigger::Offset {
            offset,
            relative_to: RelativeTo::Start,
        } = alert.trigger
        else {
            drop("Google reminds only at an offset before the start");
            continue;
        };
        let seconds = offset.as_secs();
        if seconds > 0 || seconds % 60 != 0 || offset.subsec_nanos() != 0 {
            drop("Google reminds only a whole number of minutes before the start");
            continue;
        }
        let mut reminder = Map::new();
        reminder.insert("method".to_owned(), Value::String(method.to_owned()));
        reminder.insert("minutes".to_owned(), Value::from(-seconds / 60));
        overrides.push(Value::Object(reminder));
    }
    object.insert("overrides".to_owned(), Value::Array(overrides));
    Value::Object(object)
}

#[cfg(test)]
mod tests;
