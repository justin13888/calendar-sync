//! An iCalendar document: its bytes, read into series, and edited in place.

use core::fmt;
use std::collections::BTreeSet;

use calendar_sync_model::{
    EventSeries, EventTimes, Field, InstanceKey, InstanceRange, Loss, LossReason, MapperVersion,
    NormalizedEvent, Override, Provider, ProviderEvent, ProviderIds, ProviderRevision,
};
use ical::tree::cst::{IcalCst, IcalItem};
use jiff::Timestamp;

use crate::line;
use crate::read::{self, Group, Located};
use crate::value;
use crate::write::{self, Writer};
use crate::zone::Zones;

/// The version of this crate's mapping. It changes whenever the same iCalendar bytes would read
/// into different fields.
pub const MAPPER_VERSION: MapperVersion = MapperVersion::new(1);

/// Where a document came from, for the ids and revision its events are read with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The provider holding the document: [`Provider::CalDav`] for a `CalDAV` object,
    /// [`Provider::Ics`] for a feed.
    pub provider: Provider,
    /// The resource's own id, such as a `CalDAV` object's href. `None` uses each event's `UID`, as
    /// a feed does.
    pub resource_id: Option<String>,
    /// The resource's revision, such as a `CalDAV` etag.
    pub revision: Option<ProviderRevision>,
}

impl Source {
    /// A read-only iCalendar feed (a subscription): every event is identified by its `UID`, and
    /// has no revision of its own.
    #[must_use]
    pub fn feed() -> Self {
        Self {
            provider: Provider::Ics,
            resource_id: None,
            revision: None,
        }
    }

    /// A `CalDAV` calendar object resource at `href`, with its etag where the server gave one.
    #[must_use]
    pub fn caldav(href: impl Into<String>, etag: Option<ProviderRevision>) -> Self {
        Self {
            provider: Provider::CalDav,
            resource_id: Some(href.into()),
            revision: etag,
        }
    }

    fn ids(&self, uid: &str) -> ProviderIds {
        ProviderIds {
            provider: self.provider,
            id: self.resource_id.clone().unwrap_or_else(|| uid.to_owned()),
            ical_uid: Some(uid.to_owned()),
            series_id: None,
        }
    }

    fn event(&self, uid: &str, located: &Located) -> Option<ProviderEvent> {
        Some(ProviderEvent {
            ids: self.ids(uid),
            revision: self.revision.clone(),
            mapper_version: MAPPER_VERSION,
            fields: located.read.event.clone()?,
            passthrough: located.read.passthrough.clone(),
        })
    }
}

/// What reading a document found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    /// Every series, one per `UID`, in the order each `UID` first appears.
    pub series: Vec<ReadSeries>,
    /// What could not be read outside any series: bytes the parser could not structure, and
    /// `VEVENT`s with no `UID`.
    pub losses: Vec<Loss>,
}

/// One series read from a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSeries {
    /// The `UID` the series' `VEVENT`s share.
    pub uid: String,
    /// The master and its overrides.
    pub series: EventSeries,
    /// What could not be carried into the model.
    pub losses: Vec<Loss>,
}

/// Why a series could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteError {
    /// Neither the master nor any override carries an iCalendar `UID` in its
    /// [`ProviderIds::ical_uid`].
    NoUid,
    /// The document holds no `VCALENDAR` to write into.
    NoCalendar,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoUid => "the series carries no iCalendar UID",
            Self::NoCalendar => "the document holds no VCALENDAR",
        })
    }
}

impl core::error::Error for WriteError {}

/// An iCalendar document: a `CalDAV` object, or a feed of one or more `VCALENDAR`s.
///
/// It keeps every byte it was parsed from. [`IcalDocument::to_bytes`] returns them unchanged until
/// [`IcalDocument::write`] edits the document, and an edit rewrites only the properties whose
/// fields changed.
#[derive(Clone, Debug)]
pub struct IcalDocument {
    calendars: Vec<IcalCst<'static>>,
    problems: Vec<String>,
}

impl IcalDocument {
    /// Parses a document. Parsing never fails: a line that cannot be structured is kept as it is,
    /// so the bytes still round trip, and is reported in [`Read::losses`].
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let recovery = IcalCst::parse_recovering(bytes);
        Self {
            problems: recovery.problems.iter().map(ToString::to_string).collect(),
            calendars: recovery
                .calendars
                .into_iter()
                .map(IcalCst::into_static)
                .collect(),
        }
    }

    /// An empty iCalendar 2.0 calendar, with the `PRODID` given.
    #[must_use]
    pub fn new(product_id: &str) -> Self {
        let mut calendar = IcalCst::v2();
        calendar.push_line(line::build("PRODID", &[], &line::escape_text(product_id)));
        Self {
            calendars: vec![calendar],
            problems: Vec::new(),
        }
    }

    /// The document's bytes: exactly those parsed, except where a write changed them.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for calendar in &self.calendars {
            out.extend(calendar.to_bytes());
        }
        out
    }

    /// Whether the document parsed with nothing the parser had to work around.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    /// Reads every `VEVENT` into series, grouped by `UID`.
    ///
    /// A `VEVENT` with a `RECURRENCE-ID` is an override, keyed by it; one whose `STATUS` is
    /// `CANCELLED` reads as [`Override::Cancelled`]. `VTODO`, `VJOURNAL` and `VFREEBUSY` are not
    /// events and are not read, but are kept.
    #[must_use]
    pub fn read(&self, source: &Source) -> Read {
        let (groups, mut losses) = read::groups(&self.calendars);
        let mut problems: Vec<Loss> = self
            .problems
            .iter()
            .map(|problem| {
                Loss::new(
                    Field::Other("VCALENDAR".to_owned()),
                    LossReason::Malformed,
                    problem.clone(),
                )
            })
            .collect();
        problems.append(&mut losses);
        Read {
            series: groups
                .into_iter()
                .map(|group| series(source, group))
                .collect(),
            losses: dedup(problems),
        }
    }

    /// Writes `series` into the document, replacing the series with the same `UID`.
    ///
    /// Only the difference is written. Each `VEVENT` already in the document is read, and only the
    /// properties whose fields differ from `series` are rewritten; every other byte stays as it
    /// was, unknown properties and parameters included. A master or override that `series` drops
    /// is removed, and one it adds is appended, its `DTSTAMP` set to `stamp` unless its
    /// passthrough carries one. A new `TZID` gets the `VTIMEZONE` it needs.
    ///
    /// `SEQUENCE`, `DTSTAMP` and `LAST-MODIFIED` of an existing `VEVENT` are not changed: they are
    /// the caller's to manage, as an iTIP exchange requires.
    ///
    /// Returns what could not be written.
    ///
    /// # Errors
    ///
    /// [`WriteError::NoUid`] when no event in `series` carries an iCalendar `UID`, and
    /// [`WriteError::NoCalendar`] when the document holds no `VCALENDAR`.
    pub fn write(
        &mut self,
        series: &EventSeries,
        stamp: Timestamp,
    ) -> Result<Vec<Loss>, WriteError> {
        let uid = series_uid(series).ok_or(WriteError::NoUid)?.to_owned();
        let (groups, _) = read::groups(&self.calendars);
        let group = groups.into_iter().find(|group| group.uid == uid);
        let home = group
            .as_ref()
            .and_then(|group| {
                group
                    .master
                    .iter()
                    .chain(group.overrides.values())
                    .map(|located| located.calendar)
                    .chain(group.unread.iter().map(|&(calendar, _)| calendar))
                    .next()
            })
            .or_else(|| {
                self.calendars
                    .iter()
                    .position(|calendar| line::component_is(calendar, "VCALENDAR"))
            })
            .ok_or(WriteError::NoCalendar)?;

        let mut edit = Edit {
            uid: &uid,
            stamp,
            losses: Vec::new(),
            removed: BTreeSet::new(),
            added: Vec::new(),
            touched: BTreeSet::new(),
        };
        let mut group = group.unwrap_or_else(|| Group {
            uid: uid.clone(),
            master: None,
            overrides: std::collections::BTreeMap::new(),
            unread: Vec::new(),
            losses: Vec::new(),
        });

        // The master.
        match (group.master.take(), &series.master) {
            (Some(located), Some(new)) => edit.modify(&mut self.calendars, &located, &new.fields),
            (Some(located), None) => {
                edit.removed.insert((located.calendar, located.item));
            }
            (None, Some(new)) => {
                // A master the reader could not read is replaced, rather than kept beside the new.
                for &(calendar, item) in &group.unread {
                    if let IcalItem::Component(component) = &self.calendars[calendar].items[item]
                        && line::first(component, "RECURRENCE-ID").is_none()
                    {
                        edit.removed.insert((calendar, item));
                    }
                }
                let component = edit.create(&self.calendars[home], None, new);
                edit.added.push(component);
            }
            (None, None) => {}
        }

        // The overrides.
        let keys: BTreeSet<&InstanceKey> = group
            .overrides
            .keys()
            .chain(series.overrides.keys())
            .collect();
        for key in keys {
            let existing = group.overrides.get(key);
            match (existing, series.overrides.get(key)) {
                (Some(located), Some(Override::Modified { event, range })) => {
                    edit.override_fields(&mut self.calendars, located, event, *range);
                }
                (Some(located), Some(Override::Cancelled { .. })) => {
                    if !located.read.cancelled {
                        cancel(&mut self.calendars, located);
                    }
                }
                (Some(located), None) => {
                    edit.removed.insert((located.calendar, located.item));
                }
                (None, Some(Override::Modified { event, range })) => {
                    // An override of this instance the reader could not read is replaced, as an
                    // unread master is, rather than kept beside the new one.
                    edit.removed
                        .extend(unread_overrides(&self.calendars, &group.unread, key));
                    let event = edit.without_recurrence(event);
                    let component = edit.create(&self.calendars[home], Some((key, *range)), &event);
                    edit.added.push(component);
                }
                (None, Some(Override::Cancelled { .. })) => {
                    edit.removed
                        .extend(unread_overrides(&self.calendars, &group.unread, key));
                    let component = edit.cancelled(&self.calendars[home], key);
                    edit.added.push(component);
                }
                (None, None) => {}
            }
        }

        let Edit {
            mut losses,
            removed,
            added,
            mut touched,
            ..
        } = edit;
        for &(calendar, item) in removed.iter().rev() {
            self.calendars[calendar].items.remove(item);
        }
        let mut needed = std::collections::BTreeMap::new();
        for (component, zones) in added {
            needed.extend(zones);
            self.calendars[home].push_component(component);
        }
        touched.extend(
            needed
                .into_iter()
                .map(|(tzid, anchor)| (home, tzid, anchor)),
        );
        let mut by_calendar: std::collections::BTreeMap<
            usize,
            std::collections::BTreeMap<String, i64>,
        > = std::collections::BTreeMap::new();
        for (calendar, tzid, anchor) in touched {
            let zones = by_calendar.entry(calendar).or_default();
            let earliest = zones.entry(tzid).or_insert(anchor);
            *earliest = (*earliest).min(anchor);
        }
        for (calendar, zones) in by_calendar {
            write::add_vtimezones(&mut self.calendars[calendar], &zones, &mut losses);
        }
        Ok(dedup(losses))
    }
}

/// One write in progress.
struct Edit<'u> {
    uid: &'u str,
    stamp: Timestamp,
    losses: Vec<Loss>,
    /// `VEVENT`s to remove, by calendar and item.
    removed: BTreeSet<(usize, usize)>,
    /// `VEVENT`s to append to the series' calendar, each with the zones it writes.
    added: Vec<(IcalCst<'static>, std::collections::BTreeMap<String, i64>)>,
    /// Zones written into existing `VEVENT`s: calendar, `TZID`, and the earliest time written.
    touched: BTreeSet<(usize, String, i64)>,
}

impl Edit<'_> {
    /// Writes `new` into the `VEVENT` at `located`, which reads as `located.read.event`.
    fn modify(
        &mut self,
        calendars: &mut [IcalCst<'static>],
        located: &Located,
        new: &NormalizedEvent,
    ) {
        let Some(old) = &located.read.event else {
            return;
        };
        let zones = Zones::of(&calendars[located.calendar]);
        let mut writer = Writer::new(&zones);
        if let IcalItem::Component(component) = &mut calendars[located.calendar].items[located.item]
        {
            write::apply(component, old, new, &mut writer);
        }
        self.finish(located.calendar, writer);
    }

    /// Writes an override's fields and range into its existing `VEVENT`.
    fn override_fields(
        &mut self,
        calendars: &mut [IcalCst<'static>],
        located: &Located,
        event: &ProviderEvent,
        range: InstanceRange,
    ) {
        let event = self.without_recurrence(event);
        self.modify(calendars, located, &event.fields);
        let old_range = located
            .read
            .instance
            .clone()
            .flatten()
            .map(|(_, range)| range);
        if old_range != Some(range)
            && let IcalItem::Component(component) =
                &mut calendars[located.calendar].items[located.item]
            && let Some(&at) = line::positions(component, "RECURRENCE-ID").first()
            && let IcalItem::Prop(old) = &component.items[at]
        {
            let value = line::raw(old);
            let rewritten = line::rewrite(old, &[("RANGE", range_param(range))], &value);
            component.items[at] = IcalItem::Prop(rewritten);
        }
    }

    /// The event with no recurrence, which an override cannot carry.
    fn without_recurrence(&mut self, event: &ProviderEvent) -> ProviderEvent {
        let mut event = event.clone();
        if event.fields.recurrence.take().is_some() {
            self.losses.push(Loss::new(
                Field::Recurrence,
                LossReason::Unrepresentable,
                "an override cannot carry its own recurrence",
            ));
        }
        event
    }

    /// Collects a writer's losses and zones.
    fn finish(&mut self, calendar: usize, writer: Writer<'_>) {
        self.losses.extend(writer.losses);
        self.touched.extend(
            writer
                .needed
                .into_iter()
                .map(|(tzid, anchor)| (calendar, tzid, anchor)),
        );
    }

    /// The start of a new `VEVENT`: its `UID`, its `DTSTAMP` unless `passthrough` has one, its
    /// `RECURRENCE-ID`, and its `DTSTART`.
    fn skeleton(
        &mut self,
        writer: &mut Writer<'_>,
        instance: Option<(&InstanceKey, InstanceRange)>,
        start: &calendar_sync_model::TimeValue,
        stamped: bool,
    ) -> IcalCst<'static> {
        let mut component = IcalCst::empty("VEVENT");
        component.push_line(line::build("UID", &[], &line::escape_text(self.uid)));
        if !stamped {
            component.push_line(line::build(
                "DTSTAMP",
                &[],
                &value::format_instant(self.stamp),
            ));
        }
        if let Some((key, range)) = instance {
            let (params, value) = writer.time(key.original_start(), &Field::Overrides);
            let mut params: Vec<(&str, String)> = params
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect();
            if let Some(range) = range_param(range) {
                params.push(("RANGE", range));
            }
            component.push_line(line::build("RECURRENCE-ID", &params, &value));
        }
        let (params, value) = writer.time(start, &Field::Times);
        let params: Vec<(&str, String)> = params
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, value)))
            .collect();
        component.push_line(line::build("DTSTART", &params, &value));
        component
    }

    /// A new `VEVENT` for `event`, in `calendar`.
    fn create(
        &mut self,
        calendar: &IcalCst<'static>,
        instance: Option<(&InstanceKey, InstanceRange)>,
        event: &ProviderEvent,
    ) -> (IcalCst<'static>, std::collections::BTreeMap<String, i64>) {
        let zones = Zones::of(calendar);
        let mut writer = Writer::new(&zones);
        // Passthrough is the provider's own serialization: iCalendar lines only from an
        // iCalendar provider.
        let own = matches!(event.ids.provider, Provider::CalDav | Provider::Ics);
        let stamped = own && event.passthrough.get("DTSTAMP").next().is_some();
        let mut component =
            self.skeleton(&mut writer, instance, event.fields.times.start(), stamped);
        if own {
            for field in &event.passthrough {
                if component.push_raw(&field.raw).is_err() {
                    writer.losses.push(Loss::new(
                        Field::Other(field.name.clone()),
                        LossReason::Malformed,
                        "a passthrough line could not be written back",
                    ));
                }
            }
        }
        let base = read::event(&component, &zones).event.unwrap_or_else(|| {
            NormalizedEvent::new(EventTimes::at(event.fields.times.start().clone()))
        });
        write::apply(&mut component, &base, &event.fields, &mut writer);
        self.losses.extend(writer.losses);
        (component, writer.needed)
    }

    /// A new `VEVENT` cancelling the instance `key`.
    fn cancelled(
        &mut self,
        calendar: &IcalCst<'static>,
        key: &InstanceKey,
    ) -> (IcalCst<'static>, std::collections::BTreeMap<String, i64>) {
        let zones = Zones::of(calendar);
        let mut writer = Writer::new(&zones);
        let mut component = self.skeleton(
            &mut writer,
            Some((key, InstanceRange::ThisInstance)),
            key.original_start(),
            false,
        );
        component.push_line(line::build("STATUS", &[], "CANCELLED"));
        self.losses.extend(writer.losses);
        (component, writer.needed)
    }
}

/// The unread `VEVENT`s among `unread` whose `RECURRENCE-ID` reads as `key`: overrides of that
/// instance with no readable `DTSTART`.
fn unread_overrides(
    calendars: &[IcalCst<'static>],
    unread: &[(usize, usize)],
    key: &InstanceKey,
) -> Vec<(usize, usize)> {
    unread
        .iter()
        .copied()
        .filter(|&(calendar, item)| match &calendars[calendar].items[item] {
            IcalItem::Component(component) => {
                read::event(component, &Zones::of(&calendars[calendar]))
                    .instance
                    .flatten()
                    .is_some_and(|(read, _)| read == *key)
            }
            _ => false,
        })
        .collect()
}

/// Marks an existing override cancelled.
fn cancel(calendars: &mut [IcalCst<'static>], located: &Located) {
    if let IcalItem::Component(component) = &mut calendars[located.calendar].items[located.item] {
        line::set_single(component, "STATUS", &[], Some("CANCELLED"));
    }
}

/// The `RANGE` parameter for an override's range.
fn range_param(range: InstanceRange) -> Option<String> {
    match range {
        InstanceRange::ThisInstance => None,
        InstanceRange::ThisAndFuture => Some("THISANDFUTURE".to_owned()),
    }
}

/// The iCalendar `UID` a series carries.
fn series_uid(series: &EventSeries) -> Option<&str> {
    let master = series
        .master
        .as_ref()
        .and_then(|master| master.ids.ical_uid.as_deref());
    master.or_else(|| {
        series.overrides.values().find_map(|value| match value {
            Override::Modified { event, .. } => event.ids.ical_uid.as_deref(),
            Override::Cancelled { ids, .. } => ids.as_ref().and_then(|ids| ids.ical_uid.as_deref()),
        })
    })
}

/// A group as a series.
fn series(source: &Source, group: Group) -> ReadSeries {
    let master = group
        .master
        .as_ref()
        .and_then(|located| source.event(&group.uid, located));
    let overrides = group
        .overrides
        .iter()
        .filter_map(|(key, located)| {
            let range = located
                .read
                .instance
                .clone()
                .flatten()
                .map_or(InstanceRange::ThisInstance, |(_, range)| range);
            let value = if located.read.cancelled {
                Override::Cancelled {
                    ids: None,
                    revision: None,
                }
            } else {
                Override::Modified {
                    event: Box::new(source.event(&group.uid, located)?),
                    range,
                }
            };
            Some((key.clone(), value))
        })
        .collect();
    ReadSeries {
        uid: group.uid,
        series: EventSeries { master, overrides },
        losses: dedup(group.losses),
    }
}

/// Losses in order, each once.
fn dedup(losses: Vec<Loss>) -> Vec<Loss> {
    let mut out: Vec<Loss> = Vec::with_capacity(losses.len());
    for loss in losses {
        if !out.contains(&loss) {
            out.push(loss);
        }
    }
    out
}
