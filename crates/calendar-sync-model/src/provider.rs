//! What a provider holds: its ids, its revision, the mapper that read it, and the fields the model
//! has no place for.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::NormalizedEvent;

/// The service an event came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Provider {
    /// Google Calendar.
    Google,
    /// Microsoft Graph (Outlook, Exchange Online).
    Graph,
    /// A `CalDAV` server.
    CalDav,
    /// A read-only iCalendar feed (a subscription).
    Ics,
}

/// The provider's identifiers for one event resource.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIds {
    /// The provider.
    pub provider: Provider,
    /// The resource's own id: the Google event `id`, the Graph immutable `id`, the `CalDAV`
    /// object's href, or a feed event's `UID`.
    pub id: String,
    /// The iCalendar `UID`: Google `iCalUID`, Graph `iCalUId`, `CalDAV` and feed `UID`. Graph's
    /// differs per occurrence and Google's is shared, so it never identifies an instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    /// The series master's id, on an override held as its own resource: Google
    /// `recurringEventId`, Graph `seriesMasterId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
}

impl ProviderIds {
    /// Ids for a resource with only its own id.
    #[must_use]
    pub fn new(provider: Provider, id: impl Into<String>) -> Self {
        Self {
            provider,
            id: id.into(),
            ical_uid: None,
            series_id: None,
        }
    }
}

/// A provider's revision of a resource: a Google or `CalDAV` `etag`, or a Graph `changeKey`.
///
/// It is opaque and compared for equality only. It is never ordered: a `CalDAV` etag carries no
/// order, so "newer" cannot be read from two revisions, and this type has no `Ord` to tempt it.
///
/// ```compile_fail
/// use calendar_sync_model::ProviderRevision;
/// let a = ProviderRevision::new("\"1\"");
/// let b = ProviderRevision::new("\"2\"");
/// let _ = a < b;
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderRevision(String);

impl ProviderRevision {
    /// Wraps a revision exactly as the provider sent it, quotes and weak prefix included.
    #[must_use]
    pub fn new(revision: impl Into<String>) -> Self {
        Self(revision.into())
    }

    /// The revision as the provider sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The version of the mapper that produced a record.
///
/// A mapper bumps it whenever the same provider payload would map to different fields. A reader
/// never overwrites a record written by a newer mapper version than its own.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MapperVersion(u32);

impl MapperVersion {
    /// A mapper version.
    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    /// The version number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for MapperVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Provider fields the model has no place for, kept opaque so a write can send them back.
///
/// Entries keep the provider's order, and a name may repeat, as an iCalendar `X-` property can.
/// Each value is the provider's own serialization of the field (JSON text for Google and Graph,
/// the content line for iCalendar); only the mapper that wrote it reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Passthrough(Vec<PassthroughField>);

impl Passthrough {
    /// No fields.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a field.
    pub fn push(&mut self, name: impl Into<String>, raw: impl Into<String>) {
        self.0.push(PassthroughField {
            name: name.into(),
            raw: raw.into(),
        });
    }

    /// Every field, in order.
    pub fn iter(&self) -> impl Iterator<Item = &PassthroughField> {
        self.0.iter()
    }

    /// The raw values of every field named `name`, in order.
    pub fn get<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |field| field.name == name)
            .map(|field| field.raw.as_str())
    }

    /// Whether there are no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many fields there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl FromIterator<PassthroughField> for Passthrough {
    fn from_iter<I: IntoIterator<Item = PassthroughField>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a> IntoIterator for &'a Passthrough {
    type Item = &'a PassthroughField;
    type IntoIter = core::slice::Iter<'a, PassthroughField>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// One opaque provider field.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PassthroughField {
    /// The provider's name for the field.
    pub name: String,
    /// The provider's serialization of its value.
    pub raw: String,
}

/// One event resource as a provider holds it: its ids and revision, the mapper version that read
/// it, its normalized fields, and the fields the model has no place for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEvent {
    /// The provider's ids for the resource.
    pub ids: ProviderIds,
    /// The provider's revision, where it gives one. A feed event has none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<ProviderRevision>,
    /// The mapper version that produced `fields` and `passthrough`.
    pub mapper_version: MapperVersion,
    /// The normalized fields.
    pub fields: NormalizedEvent,
    /// The provider fields the model has no place for.
    #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
    pub passthrough: Passthrough,
}
