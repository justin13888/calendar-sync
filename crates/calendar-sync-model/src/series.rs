//! Series and overrides, keyed by the provider's own instance identity.

use core::fmt;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ProviderEvent, ProviderIds, ProviderRevision, TimeValue};

/// The provider's identity for one instance of a series: its original start, before any move.
///
/// This is Google's `originalStartTime`, Graph's `originalStart`, and `CalDAV`'s `RECURRENCE-ID`,
/// in whichever form the provider wrote it. It is not derived from a `UID`, because Graph's
/// `iCalUId` differs per occurrence and Google's is shared.
///
/// The derived ordering is structural, for use as a map key; it is not chronological.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceKey(TimeValue);

impl InstanceKey {
    /// The key for the instance originally at `original_start`.
    #[must_use]
    pub fn new(original_start: TimeValue) -> Self {
        Self(original_start)
    }

    /// The instance's original start.
    #[must_use]
    pub fn original_start(&self) -> &TimeValue {
        &self.0
    }
}

/// Which instances an override applies to (the `RANGE` parameter of `RECURRENCE-ID`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstanceRange {
    /// Only the keyed instance.
    #[default]
    ThisInstance,
    /// The keyed instance and every later one (`RANGE=THISANDFUTURE`).
    ThisAndFuture,
}

/// How one instance of a series differs from the master.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Override {
    /// The instance was changed. `event` holds all of its fields, not a patch, and its
    /// `recurrence` is `None`.
    Modified {
        /// The instance as the provider holds it.
        event: Box<ProviderEvent>,
        /// Which instances the change applies to.
        #[serde(default)]
        range: InstanceRange,
    },
    /// The instance was removed. Google keeps a cancelled instance as its own resource, with ids
    /// and a revision; a removal written as an `EXDATE` lives in the master's recurrence instead.
    Cancelled {
        /// The provider's ids for the cancelled instance, where it has its own resource.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ids: Option<ProviderIds>,
        /// The cancelled instance's revision, where it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<ProviderRevision>,
    },
}

/// One provider event: a single event, or a recurring series with its overrides.
///
/// A single event is a master with no recurrence and no overrides. A series without a master is
/// also valid: an invitation to one instance of someone else's series arrives as the override
/// alone, in iCalendar and in Google.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSeries {
    /// The master: the single event, or the series' rule-bearing resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<ProviderEvent>,
    /// The overrides, by the instance each replaces.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "serialize_overrides",
        deserialize_with = "deserialize_overrides"
    )]
    pub overrides: BTreeMap<InstanceKey, Override>,
}

impl EventSeries {
    /// A single event, or a series master with no overrides yet.
    #[must_use]
    pub fn single(master: ProviderEvent) -> Self {
        Self {
            master: Some(master),
            overrides: BTreeMap::new(),
        }
    }

    /// Whether this holds neither a master nor an override.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.master.is_none() && self.overrides.is_empty()
    }

    /// Whether the master recurs.
    #[must_use]
    pub fn is_recurring(&self) -> bool {
        self.master
            .as_ref()
            .is_some_and(|master| master.fields.recurrence.is_some())
    }
}

/// One override as serialized: a map with non-string keys does not survive JSON, so overrides are
/// a list of entries.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverrideEntry<T> {
    instance: InstanceKey,
    #[serde(rename = "override")]
    value: T,
}

fn serialize_overrides<S: Serializer>(
    overrides: &BTreeMap<InstanceKey, Override>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(overrides.iter().map(|(instance, value)| OverrideEntry {
        instance: instance.clone(),
        value,
    }))
}

fn deserialize_overrides<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<InstanceKey, Override>, D::Error> {
    let entries = Vec::<OverrideEntry<Override>>::deserialize(deserializer)?;
    let mut overrides = BTreeMap::new();
    for entry in entries {
        match overrides.entry(entry.instance) {
            Entry::Vacant(slot) => {
                slot.insert(entry.value);
            }
            Entry::Occupied(slot) => {
                return Err(D::Error::custom(DuplicateInstance(slot.key().clone())));
            }
        }
    }
    Ok(overrides)
}

/// Two overrides for one instance.
struct DuplicateInstance(InstanceKey);

impl fmt::Display for DuplicateInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "two overrides for the instance {:?}",
            self.0.original_start()
        )
    }
}
