//! The instance index: every series' occurrences over a rolling horizon, kept in a store the
//! application supplies, and free/busy read from it.
//!
//! The index is a cache. Everything in it is derived from the series and the time zone data, so
//! a store may drop it at any time ([`IndexStore::clear`]) and [`InstanceIndex::refresh`] builds
//! it again. It holds, for each series, the occurrences that fall in the horizon, and beside
//! them the horizon, the zone database version and the floating zone they were expanded with.
//!
//! # Invalidation
//!
//! - **A series changes:** [`InstanceIndex::upsert`] expands it again over the horizon;
//!   [`InstanceIndex::remove`] drops it.
//! - **The horizon moves:** [`InstanceIndex::refresh`] drops the occurrences that left it and
//!   expands each series over only the time that entered it. A series whose expansion reports
//!   a loss, such as one that ran out of the recurrence crate's period budget, or a horizon that
//!   grew, is expanded over the whole horizon instead, so its occurrences and losses are those
//!   a rebuild finds.
//! - **Time zone data changes** (a new database version, or a new floating zone): every entry
//!   is stale. Reads and [`InstanceIndex::upsert`] refuse a stale index with
//!   [`IndexError::Stale`], and [`InstanceIndex::refresh`] expands every series again.
//!
//! A refresh marks the index stale while it writes, so one interrupted part-way leaves an index
//! the next refresh rebuilds in full, never one it extends from the wrong horizon.
//!
//! # Bounds
//!
//! An index holds only what overlaps its horizon, so a series that never ends holds at most the
//! occurrences of one horizon, and its losses of one expansion, however often the horizon
//! rolls.
//!
//! ```
//! use calendar_sync_engine::ResourceId;
//! use calendar_sync_engine::freebusy::{BusyInterval, BusyKind};
//! use calendar_sync_engine::index::{Horizon, InstanceIndex, MemoryStore, ZoneDataVersion};
//! use calendar_sync_model::{
//!     EventSeries, EventTimes, Frequency, MapperVersion, NormalizedEvent, Passthrough, Provider,
//!     ProviderEvent, ProviderIds, Recurrence, RecurrenceRule, TimeValue,
//! };
//! use jiff::SignedDuration;
//! use jiff::tz::{TimeZone, TimeZoneDatabase};
//!
//! let at = |text: &str| text.parse::<jiff::Timestamp>().unwrap();
//! // A daily stand-up that never ends.
//! let mut fields = NormalizedEvent::new(
//!     EventTimes::new(
//!         TimeValue::instant(at("2026-03-02T09:00:00Z")),
//!         Some(TimeValue::instant(at("2026-03-02T09:15:00Z"))),
//!     )
//!     .unwrap(),
//! );
//! fields.recurrence = Some(Recurrence::rule(RecurrenceRule::new(Frequency::Daily)));
//! let series = EventSeries::single(ProviderEvent {
//!     ids: ProviderIds::new(Provider::Google, "standup"),
//!     revision: None,
//!     mapper_version: MapperVersion::new(1),
//!     fields,
//!     passthrough: Passthrough::new(),
//! });
//! let id = ResourceId::new("standup");
//!
//! let zones = TimeZoneDatabase::none();
//! let index = InstanceIndex::new(&zones, TimeZone::UTC, ZoneDataVersion::new("2026a"));
//! let horizon = Horizon::new(SignedDuration::ZERO, SignedDuration::from_hours(7 * 24));
//! let mut store = MemoryStore::new();
//!
//! // The application passes the time; the index reads no clock.
//! index.refresh(&mut store, horizon.window_at(at("2026-03-02T00:00:00Z")).unwrap(), [(&id, &series)]).unwrap();
//! assert_eq!(store.entry_ref(&id).unwrap().instances.len(), 7);
//!
//! let day = horizon.window_at(at("2026-03-03T00:00:00Z")).unwrap();
//! index.refresh(&mut store, day, [(&id, &series)]).unwrap();
//! let monday = calendar_sync_recurrence::Window::new(at("2026-03-04T00:00:00Z"), at("2026-03-05T00:00:00Z")).unwrap();
//! assert_eq!(
//!     index.free_busy(&store, &monday).unwrap(),
//!     [BusyInterval::new(at("2026-03-04T09:00:00Z"), at("2026-03-04T09:15:00Z"), BusyKind::Busy)]
//! );
//! ```

use core::convert::Infallible;
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use calendar_sync_model::{EventSeries, InstanceKey, Loss};
use calendar_sync_recurrence::{EmptyWindow, Expander, Window, ZoneSource};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ResourceId;
use crate::freebusy::{self, BusyInterval, BusyKind};

/// The version of the time zone database expansion reads, such as `2026a`.
///
/// Opaque: compared only for equality. A different version makes every entry in an index stale,
/// because a zone's rules may have changed under it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ZoneDataVersion(String);

impl ZoneDataVersion {
    /// Wraps a database version.
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// The version.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How far a rolling horizon reaches either side of the time it is placed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Horizon {
    behind: SignedDuration,
    ahead: SignedDuration,
}

impl Horizon {
    /// A horizon from `behind` before a time to `ahead` after it. A negative length counts as
    /// zero.
    #[must_use]
    pub fn new(behind: SignedDuration, ahead: SignedDuration) -> Self {
        Self {
            behind: behind.max(SignedDuration::ZERO),
            ahead: ahead.max(SignedDuration::ZERO),
        }
    }

    /// The window this horizon covers at `now`, which the caller supplies. Saturates at the
    /// ends of time.
    ///
    /// # Errors
    ///
    /// [`EmptyWindow`] if the horizon has no length at `now`: both its lengths are zero, or
    /// `now` is at an end of time it reaches no further than.
    pub fn window_at(&self, now: Timestamp) -> Result<Window, EmptyWindow> {
        let start = now.saturating_sub(self.behind).unwrap_or(Timestamp::MIN);
        let end = now.saturating_add(self.ahead).unwrap_or(Timestamp::MAX);
        Window::new(start, end)
    }
}

/// One occurrence held in the index.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedInstance {
    /// When the occurrence starts, after any override.
    pub start: Timestamp,
    /// When it ends; equal to `start` for an occurrence that takes no time.
    pub end: Timestamp,
    /// The instance's identity, as [`Occurrence::key`](calendar_sync_recurrence::Occurrence::key).
    pub key: InstanceKey,
    /// How the occurrence takes its time, or `None` if it takes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<BusyKind>,
}

/// One series' entry in the index: its occurrences in the horizon, and what expansion could not
/// honour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesEntry {
    /// The occurrences that overlap the horizon, ordered by start, then end, then key.
    pub instances: Vec<IndexedInstance>,
    /// What expansion could not honour, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub losses: Vec<Loss>,
}

/// What every entry in an index was expanded with.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexMeta {
    /// Where the horizon starts, inclusive.
    pub start: Timestamp,
    /// Where the horizon ends, exclusive.
    pub end: Timestamp,
    /// The time zone database version.
    pub zone_data: ZoneDataVersion,
    /// The floating zone, by name: floating and all-day times are placed in it.
    pub floating: String,
}

impl IndexMeta {
    /// The horizon, or `None` if a store returned one that ends before it starts.
    #[must_use]
    pub fn horizon(&self) -> Option<Window> {
        Window::new(self.start, self.end).ok()
    }
}

/// Where the index lives. The application supplies it: memory, a database table, a file.
///
/// Everything in it may be dropped at any time; the index is rebuilt from the series. A store
/// need not be transactional: the index orders its writes so that an interrupted one leaves a
/// store the next [`InstanceIndex::refresh`] rebuilds.
pub trait IndexStore {
    /// Why the store failed.
    type Error;

    /// What the entries were expanded with, or `None` if the store holds no usable index.
    ///
    /// # Errors
    ///
    /// If the store cannot be read.
    fn meta(&self) -> Result<Option<IndexMeta>, Self::Error>;

    /// Records what the entries were expanded with; `None` marks the index unusable.
    ///
    /// # Errors
    ///
    /// If the store cannot be written.
    fn set_meta(&mut self, meta: Option<&IndexMeta>) -> Result<(), Self::Error>;

    /// The entry for series `id`, if it holds one.
    ///
    /// # Errors
    ///
    /// If the store cannot be read.
    fn entry(&self, id: &ResourceId) -> Result<Option<SeriesEntry>, Self::Error>;

    /// Stores `entry` for series `id`, replacing any entry it held.
    ///
    /// # Errors
    ///
    /// If the store cannot be written.
    fn put(&mut self, id: &ResourceId, entry: SeriesEntry) -> Result<(), Self::Error>;

    /// Drops the entry for series `id`, if it holds one.
    ///
    /// # Errors
    ///
    /// If the store cannot be written.
    fn remove(&mut self, id: &ResourceId) -> Result<(), Self::Error>;

    /// The ids of every series it holds an entry for.
    ///
    /// # Errors
    ///
    /// If the store cannot be read.
    fn ids(&self) -> Result<Vec<ResourceId>, Self::Error>;

    /// Drops everything: every entry and the meta.
    ///
    /// # Errors
    ///
    /// If the store cannot be written.
    fn clear(&mut self) -> Result<(), Self::Error>;
}

/// An [`IndexStore`] in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<IndexMeta>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    entries: BTreeMap<ResourceId, SeriesEntry>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The entry for series `id`, borrowed.
    #[must_use]
    pub fn entry_ref(&self, id: &ResourceId) -> Option<&SeriesEntry> {
        self.entries.get(id)
    }

    /// Every entry, by series id.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<ResourceId, SeriesEntry> {
        &self.entries
    }

    /// What the entries were expanded with.
    #[must_use]
    pub fn meta_ref(&self) -> Option<&IndexMeta> {
        self.meta.as_ref()
    }
}

impl IndexStore for MemoryStore {
    type Error = Infallible;

    fn meta(&self) -> Result<Option<IndexMeta>, Self::Error> {
        Ok(self.meta.clone())
    }

    fn set_meta(&mut self, meta: Option<&IndexMeta>) -> Result<(), Self::Error> {
        self.meta = meta.cloned();
        Ok(())
    }

    fn entry(&self, id: &ResourceId) -> Result<Option<SeriesEntry>, Self::Error> {
        Ok(self.entries.get(id).cloned())
    }

    fn put(&mut self, id: &ResourceId, entry: SeriesEntry) -> Result<(), Self::Error> {
        self.entries.insert(id.clone(), entry);
        Ok(())
    }

    fn remove(&mut self, id: &ResourceId) -> Result<(), Self::Error> {
        self.entries.remove(id);
        Ok(())
    }

    fn ids(&self) -> Result<Vec<ResourceId>, Self::Error> {
        Ok(self.entries.keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.meta = None;
        self.entries.clear();
        Ok(())
    }
}

/// Why the index could not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexError<E> {
    /// The store failed.
    Store(E),
    /// The store holds no index: it was never refreshed, was cleared, or a refresh was
    /// interrupted. [`InstanceIndex::refresh`] builds one.
    NoHorizon,
    /// The index was expanded with other time zone data or another floating zone.
    /// [`InstanceIndex::refresh`] expands it again.
    Stale,
    /// The range asked about reaches outside the horizon, where the index holds nothing.
    OutsideHorizon,
}

impl<E: fmt::Display> fmt::Display for IndexError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "the index store failed: {error}"),
            Self::NoHorizon => f.write_str("the store holds no index; refresh it"),
            Self::Stale => f.write_str("the index was expanded with other time zone data"),
            Self::OutsideHorizon => f.write_str("the range reaches outside the index's horizon"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> core::error::Error for IndexError<E> {}

impl<E> From<E> for IndexError<E> {
    fn from(error: E) -> Self {
        Self::Store(error)
    }
}

/// Maintains an instance index in an [`IndexStore`], expanding series with time zones from a
/// [`ZoneSource`].
#[derive(Debug)]
pub struct InstanceIndex<'z, Z: ?Sized> {
    expander: Expander<'z, Z>,
    zone_data: ZoneDataVersion,
    floating: String,
}

impl<'z, Z: ZoneSource + ?Sized> InstanceIndex<'z, Z> {
    /// An index expanding series with zones from `zones`, which is the database at
    /// `zone_data`, and floating and all-day times in `floating`.
    pub fn new(zones: &'z Z, floating: TimeZone, zone_data: ZoneDataVersion) -> Self {
        let name = floating
            .iana_name()
            .map_or_else(|| format!("{floating:?}"), str::to_owned);
        Self {
            expander: Expander::new(zones, floating),
            zone_data,
            floating: name,
        }
    }

    /// Brings the index in `store` to `horizon`, holding exactly the series in `series`.
    ///
    /// A series the store already holds, expanded with this index's zone data, keeps the
    /// occurrences still in the horizon and is expanded over only the time that entered it.
    /// Any other series is expanded over the whole horizon. An entry for a series not in
    /// `series` is dropped.
    ///
    /// A series whose content changed since it was stored must go through
    /// [`InstanceIndex::upsert`]: a refresh trusts the occurrences it holds.
    ///
    /// # Errors
    ///
    /// [`IndexError::Store`] if the store fails. The index is then unusable until a refresh
    /// succeeds.
    pub fn refresh<'s, S, I>(
        &self,
        store: &mut S,
        horizon: Window,
        series: I,
    ) -> Result<(), IndexError<S::Error>>
    where
        S: IndexStore + ?Sized,
        I: IntoIterator<Item = (&'s ResourceId, &'s EventSeries)>,
    {
        let previous = store
            .meta()?
            .filter(|meta| self.is_current(meta))
            .and_then(|meta| meta.horizon());
        // Until every entry is at the new horizon, the index is unusable; an interrupted
        // refresh leaves it so, and the next one rebuilds every entry.
        store.set_meta(None)?;
        let mut kept = BTreeSet::new();
        for (id, series) in series {
            let entry = match (previous, store.entry(id)?) {
                (Some(previous), Some(entry)) => self.extend(entry, series, previous, horizon),
                _ => self.expand(series, &horizon),
            };
            store.put(id, entry)?;
            kept.insert(id.clone());
        }
        for id in store.ids()? {
            if !kept.contains(&id) {
                store.remove(&id)?;
            }
        }
        store.set_meta(Some(&self.meta(horizon)))?;
        Ok(())
    }

    /// Discards everything in `store`, then builds the index for `series` over `horizon`.
    ///
    /// # Errors
    ///
    /// [`IndexError::Store`] if the store fails.
    pub fn rebuild<'s, S, I>(
        &self,
        store: &mut S,
        horizon: Window,
        series: I,
    ) -> Result<(), IndexError<S::Error>>
    where
        S: IndexStore + ?Sized,
        I: IntoIterator<Item = (&'s ResourceId, &'s EventSeries)>,
    {
        store.clear()?;
        self.refresh(store, horizon, series)
    }

    /// Expands `series`, new or changed, over the horizon, and stores it as `id`.
    ///
    /// Where the store holds no current index, the entry for `id` is dropped instead, so that
    /// whichever refresh makes the index current again expands the changed series afresh rather
    /// than trusting the occurrences it held before the change.
    ///
    /// # Errors
    ///
    /// [`IndexError::NoHorizon`] or [`IndexError::Stale`] if the store holds no current index,
    /// and [`IndexError::Store`] if it fails. After a store error, the entry for `id` may still
    /// hold the series as it was: call this again, or [`InstanceIndex::rebuild`].
    pub fn upsert<S: IndexStore + ?Sized>(
        &self,
        store: &mut S,
        id: &ResourceId,
        series: &EventSeries,
    ) -> Result<(), IndexError<S::Error>> {
        match self.current_horizon(store) {
            Ok(horizon) => store.put(id, self.expand(series, &horizon))?,
            Err(error @ (IndexError::NoHorizon | IndexError::Stale)) => {
                store.remove(id)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Drops series `id` from the index.
    ///
    /// # Errors
    ///
    /// [`IndexError::Store`] if the store fails.
    pub fn remove<S: IndexStore + ?Sized>(
        &self,
        store: &mut S,
        id: &ResourceId,
    ) -> Result<(), IndexError<S::Error>> {
        store.remove(id)?;
        Ok(())
    }

    /// Every occurrence in the index that overlaps `range`, with its series' id, ordered by
    /// start, then end, then series, then key.
    ///
    /// # Errors
    ///
    /// [`IndexError::NoHorizon`] or [`IndexError::Stale`] if the store holds no current index,
    /// [`IndexError::OutsideHorizon`] if `range` reaches outside its horizon, and
    /// [`IndexError::Store`] if the store fails.
    pub fn instances<S: IndexStore + ?Sized>(
        &self,
        store: &S,
        range: &Window,
    ) -> Result<Vec<(ResourceId, IndexedInstance)>, IndexError<S::Error>> {
        let mut found = Vec::new();
        self.each_in(store, range, |id, instance| {
            found.push((id.clone(), instance));
        })?;
        found.sort_by(|(a_id, a), (b_id, b)| {
            (a.start, a.end, a_id, &a.key).cmp(&(b.start, b.end, b_id, &b.key))
        });
        Ok(found)
    }

    /// The time taken in `range`, merged: ordered, disjoint intervals, clipped to `range`.
    ///
    /// # Errors
    ///
    /// As [`InstanceIndex::instances`].
    pub fn free_busy<S: IndexStore + ?Sized>(
        &self,
        store: &S,
        range: &Window,
    ) -> Result<Vec<BusyInterval>, IndexError<S::Error>> {
        let mut intervals = Vec::new();
        self.each_in(store, range, |_, instance| {
            if let Some(kind) = instance.busy {
                intervals.push(BusyInterval::new(instance.start, instance.end, kind));
            }
        })?;
        Ok(freebusy::merge(intervals, range))
    }

    /// Calls `found` for each occurrence that overlaps `range`.
    fn each_in<S: IndexStore + ?Sized>(
        &self,
        store: &S,
        range: &Window,
        mut found: impl FnMut(&ResourceId, IndexedInstance),
    ) -> Result<(), IndexError<S::Error>> {
        let horizon = self.current_horizon(store)?;
        if range.start() < horizon.start() || range.end() > horizon.end() {
            return Err(IndexError::OutsideHorizon);
        }
        for id in store.ids()? {
            for instance in store.entry(&id)?.unwrap_or_default().instances {
                if range.overlaps(instance.start, instance.end) {
                    found(&id, instance);
                }
            }
        }
        Ok(())
    }

    /// The horizon of the index in `store`, if it is current.
    fn current_horizon<S: IndexStore + ?Sized>(
        &self,
        store: &S,
    ) -> Result<Window, IndexError<S::Error>> {
        let meta = store.meta()?.ok_or(IndexError::NoHorizon)?;
        if !self.is_current(&meta) {
            return Err(IndexError::Stale);
        }
        meta.horizon().ok_or(IndexError::NoHorizon)
    }

    /// Whether entries expanded as `meta` says are this index's.
    fn is_current(&self, meta: &IndexMeta) -> bool {
        meta.zone_data == self.zone_data && meta.floating == self.floating
    }

    fn meta(&self, horizon: Window) -> IndexMeta {
        IndexMeta {
            start: horizon.start(),
            end: horizon.end(),
            zone_data: self.zone_data.clone(),
            floating: self.floating.clone(),
        }
    }

    /// `series` expanded over `window`.
    fn expand(&self, series: &EventSeries, window: &Window) -> SeriesEntry {
        let expansion = self.expander.expand(series, window);
        let mut entry = SeriesEntry {
            instances: Vec::new(),
            losses: Vec::new(),
        };
        entry.instances = expansion
            .occurrences
            .iter()
            .map(|occurrence| IndexedInstance {
                start: occurrence.start,
                end: occurrence.end,
                key: occurrence.key.clone(),
                busy: (occurrence.end > occurrence.start)
                    .then(|| BusyKind::of(&occurrence.event.fields))
                    .flatten(),
            })
            .collect();
        entry.instances.sort();
        add_losses(&mut entry.losses, expansion.losses);
        entry
    }

    /// `entry`, expanded over `previous`, moved to `horizon`: what left is dropped, and only
    /// what entered is expanded.
    ///
    /// Only an entry that expansion honoured in full is extended. A loss may depend on the
    /// window expanded: an expansion that runs out of the recurrence crate's period budget
    /// stops part-way through it, and its loss names where. So where the stored entry or any
    /// slice that entered reports a loss, or the horizon grew, so that expanding it whole may
    /// run out where its parts did not, the series is expanded over the whole horizon instead,
    /// and its losses are those of the horizon, never accumulated from earlier ones.
    fn extend(
        &self,
        mut entry: SeriesEntry,
        series: &EventSeries,
        previous: Window,
        horizon: Window,
    ) -> SeriesEntry {
        let grew = horizon.end().duration_since(horizon.start())
            > previous.end().duration_since(previous.start());
        if grew || !entry.losses.is_empty() {
            return self.expand(series, &horizon);
        }
        entry
            .instances
            .retain(|instance| horizon.overlaps(instance.start, instance.end));
        for part in uncovered(previous, horizon) {
            let added = self.expand(series, &part);
            if !added.losses.is_empty() {
                return self.expand(series, &horizon);
            }
            entry.instances.extend(added.instances);
        }
        entry.instances.sort();
        entry.instances.dedup();
        entry
    }
}

/// Adds each of `losses` not already in `into`, keeping `into` in one order however the losses
/// arrived.
fn add_losses(into: &mut Vec<Loss>, losses: Vec<Loss>) {
    for loss in losses {
        if !into.contains(&loss) {
            into.push(loss);
        }
    }
    into.sort_by_cached_key(|loss| {
        (
            loss.field.clone(),
            loss.detail.clone(),
            loss.reason.to_string(),
        )
    });
}

/// The parts of `horizon` that `previous` does not cover.
fn uncovered(previous: Window, horizon: Window) -> Vec<Window> {
    if horizon.end() <= previous.start() || horizon.start() >= previous.end() {
        return vec![horizon];
    }
    let before = Window::new(horizon.start(), previous.start().min(horizon.end()));
    let after = Window::new(previous.end().max(horizon.start()), horizon.end());
    before.into_iter().chain(after).collect()
}
