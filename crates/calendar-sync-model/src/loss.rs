//! Loss: what a mapping could not carry, and why.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Something a mapping could not carry faithfully.
///
/// A mapper never drops a value silently: it returns a `Loss` beside its result, naming the field
/// and the reason, and the caller decides whether to proceed, warn, or refuse.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loss {
    /// The field affected.
    pub field: Field,
    /// Why it could not be carried.
    pub reason: LossReason,
    /// What exactly was lost, for a human: the value or rule part concerned.
    pub detail: String,
}

impl Loss {
    /// A loss of `field` for `reason`.
    #[must_use]
    pub fn new(field: Field, reason: LossReason, detail: impl Into<String>) -> Self {
        Self {
            field,
            reason,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Loss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.field, self.reason, self.detail)
    }
}

/// A field of [`NormalizedEvent`](crate::NormalizedEvent), or a provider field outside it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Field {
    /// [`NormalizedEvent::title`](crate::NormalizedEvent::title).
    Title,
    /// [`NormalizedEvent::times`](crate::NormalizedEvent::times).
    Times,
    /// [`NormalizedEvent::recurrence`](crate::NormalizedEvent::recurrence).
    Recurrence,
    /// [`NormalizedEvent::organizer`](crate::NormalizedEvent::organizer).
    Organizer,
    /// [`NormalizedEvent::attendees`](crate::NormalizedEvent::attendees).
    Attendees,
    /// [`NormalizedEvent::location`](crate::NormalizedEvent::location).
    Location,
    /// [`NormalizedEvent::description`](crate::NormalizedEvent::description).
    Description,
    /// [`NormalizedEvent::status`](crate::NormalizedEvent::status).
    Status,
    /// [`NormalizedEvent::transparency`](crate::NormalizedEvent::transparency).
    Transparency,
    /// [`NormalizedEvent::visibility`](crate::NormalizedEvent::visibility).
    Visibility,
    /// [`NormalizedEvent::reminders`](crate::NormalizedEvent::reminders).
    Reminders,
    /// A series' overrides.
    Overrides,
    /// A provider field the model has no place for, by the provider's name for it.
    Other(String),
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Title => "title",
            Self::Times => "times",
            Self::Recurrence => "recurrence",
            Self::Organizer => "organizer",
            Self::Attendees => "attendees",
            Self::Location => "location",
            Self::Description => "description",
            Self::Status => "status",
            Self::Transparency => "transparency",
            Self::Visibility => "visibility",
            Self::Reminders => "reminders",
            Self::Overrides => "overrides",
            Self::Other(name) => name,
        };
        f.write_str(name)
    }
}

/// Why a value could not be carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum LossReason {
    /// The target has no way to hold the value, so it was left out.
    Unrepresentable,
    /// The target holds a nearby value instead, such as a coarser reminder offset.
    Approximated,
    /// The value is valid, but this library does not handle it yet, such as an `RSCALE` it
    /// cannot expand.
    Unsupported,
    /// The source value is malformed and could not be read.
    Malformed,
}

impl fmt::Display for LossReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unrepresentable => "unrepresentable",
            Self::Approximated => "approximated",
            Self::Unsupported => "unsupported",
            Self::Malformed => "malformed",
        })
    }
}
