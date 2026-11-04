//! The provider-neutral fields of one event.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{EventTimes, Recurrence};

/// The fields of one event, or of one override of a series, independent of any provider.
///
/// Every provider mapper reads into and writes from this type. What a provider holds that this
/// type does not goes in the record's [`Passthrough`](crate::Passthrough); what a mapping cannot
/// carry either way is reported as a [`Loss`](crate::Loss).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedEvent {
    /// The title (`SUMMARY`, Google `summary`, Graph `subject`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When the event starts and ends.
    pub times: EventTimes,
    /// How the event repeats. Set on a series master only; `None` on a single event and on an
    /// override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Recurrence>,
    /// Who organizes the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Organizer>,
    /// Who is invited, in the provider's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Attendee>,
    /// Where the event happens, as free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The description, as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the event is confirmed, tentative or cancelled.
    #[serde(default)]
    pub status: EventStatus,
    /// Whether the event blocks time.
    #[serde(default)]
    pub transparency: Transparency,
    /// Who may see the event's details.
    #[serde(default)]
    pub visibility: Visibility,
    /// When to remind the user.
    #[serde(default)]
    pub reminders: Reminders,
}

impl NormalizedEvent {
    /// An event at `times` with every other field empty or at its default.
    #[must_use]
    pub fn new(times: EventTimes) -> Self {
        Self {
            title: None,
            times,
            recurrence: None,
            organizer: None,
            attendees: Vec::new(),
            location: None,
            description: None,
            status: EventStatus::default(),
            transparency: Transparency::default(),
            visibility: Visibility::default(),
            reminders: Reminders::default(),
        }
    }
}

/// Whether an event is happening (`STATUS`, JSCalendar `status`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventStatus {
    /// It is happening.
    #[default]
    Confirmed,
    /// It may happen.
    Tentative,
    /// It is not happening.
    Cancelled,
}

/// Whether an event blocks time (`TRANSP`, JSCalendar `freeBusyStatus`, Google `transparency`,
/// Graph `showAs`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Transparency {
    /// It blocks time: busy.
    #[default]
    Opaque,
    /// It does not block time: free.
    Transparent,
}

/// Who may see an event's details (`CLASS`, JSCalendar `privacy`, Google `visibility`, Graph
/// `sensitivity`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Visibility {
    /// The calendar's default. Google `default`, and an absent `CLASS`.
    #[default]
    Default,
    /// Anyone the calendar is shared with sees the details.
    Public,
    /// Others see only that the time is taken.
    Private,
    /// Others see nothing beyond what the owner allows: iCalendar `CONFIDENTIAL`, Google
    /// `confidential`, Graph `confidential`, JSCalendar `secret`.
    Confidential,
}

/// The person or resource organizing an event (`ORGANIZER`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Organizer {
    /// The calendar address, usually an email address without `mailto:`.
    pub address: String,
    /// The display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One invitee (`ATTENDEE`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// The calendar address, usually an email address without `mailto:`.
    pub address: String,
    /// The display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether attendance is required (`ROLE`).
    #[serde(default)]
    pub role: AttendeeRole,
    /// The reply (`PARTSTAT`).
    #[serde(default)]
    pub participation: Participation,
    /// What kind of participant this is (`CUTYPE`).
    #[serde(default)]
    pub kind: ParticipantKind,
}

impl Attendee {
    /// A required individual who has not replied.
    #[must_use]
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            name: None,
            role: AttendeeRole::default(),
            participation: Participation::default(),
            kind: ParticipantKind::default(),
        }
    }
}

/// Whether an attendee's attendance is required (`ROLE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttendeeRole {
    /// Attendance is required.
    #[default]
    Required,
    /// Attendance is optional.
    Optional,
    /// Chairs the meeting.
    Chair,
    /// Informed only.
    NonParticipant,
}

/// An attendee's reply (`PARTSTAT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Participation {
    /// No reply yet.
    #[default]
    NeedsAction,
    /// Accepted.
    Accepted,
    /// Declined.
    Declined,
    /// Tentatively accepted.
    Tentative,
    /// Delegated to someone else.
    Delegated,
}

/// What kind of participant an attendee is (`CUTYPE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParticipantKind {
    /// A person.
    #[default]
    Individual,
    /// A group of people.
    Group,
    /// A physical resource, such as a projector.
    Resource,
    /// A room.
    Room,
    /// The provider does not say.
    Unknown,
}

/// When to remind the user of an event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminders {
    /// Whether the calendar's default reminders apply (Google `useDefault`, JSCalendar
    /// `useDefaultAlerts`).
    #[serde(default)]
    pub use_default: bool,
    /// The event's own reminders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<Alert>,
}

/// One reminder (`VALARM`, JSCalendar `Alert`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    /// When it fires.
    pub trigger: AlertTrigger,
    /// How it reaches the user.
    #[serde(default)]
    pub action: AlertAction,
}

/// When a reminder fires.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AlertTrigger {
    /// At an offset from the start or end. Negative is before.
    Offset {
        /// The offset.
        offset: SignedDuration,
        /// What the offset is from.
        #[serde(default)]
        relative_to: RelativeTo,
    },
    /// At an absolute instant.
    Absolute {
        /// The instant.
        at: Timestamp,
    },
}

/// What a reminder's offset is measured from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelativeTo {
    /// The event's start.
    #[default]
    Start,
    /// The event's end.
    End,
}

/// How a reminder reaches the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AlertAction {
    /// A notification on the device (`DISPLAY`, Google `popup`).
    #[default]
    Display,
    /// An email (`EMAIL`, Google `email`).
    Email,
}
