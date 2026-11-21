//! Free/busy: occurrences merged into the disjoint intervals in which time is taken.
//!
//! Each occurrence blocks time as its record says ([`BusyKind::of`]): a cancelled or
//! transparent event blocks nothing, a tentative one blocks time tentatively (`FBTYPE`
//! `BUSY-TENTATIVE`, RFC 5545 section 3.2.9), and any other blocks it outright (`BUSY`). An
//! occurrence that takes no time blocks nothing, as RFC 4791 section 7.10 has a free/busy query
//! treat it.
//!
//! [`merge`] turns any number of overlapping [`BusyInterval`]s into the disjoint, ordered list a
//! free/busy reply carries: where intervals of both kinds overlap, the time is busy, and
//! touching intervals of one kind join.
//!
//! ```
//! use calendar_sync_engine::freebusy::{self, BusyInterval, BusyKind};
//! use calendar_sync_recurrence::Window;
//!
//! let at = |hour: i8| jiff::civil::date(2026, 3, 2).at(hour, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp();
//! let merged = freebusy::merge(
//!     [
//!         BusyInterval::new(at(9), at(11), BusyKind::Tentative),
//!         BusyInterval::new(at(10), at(12), BusyKind::Busy),
//!         BusyInterval::new(at(12), at(13), BusyKind::Busy),
//!     ],
//!     &Window::new(at(0), at(23)).unwrap(),
//! );
//! assert_eq!(
//!     merged,
//!     [
//!         BusyInterval::new(at(9), at(10), BusyKind::Tentative),
//!         BusyInterval::new(at(10), at(13), BusyKind::Busy),
//!     ]
//! );
//! ```

use calendar_sync_model::{EventStatus, NormalizedEvent, Transparency};
use calendar_sync_recurrence::Window;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// How an interval's time is taken. The order is precedence: where both kinds overlap, the time
/// is [`BusyKind::Busy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BusyKind {
    /// Taken by an event that may happen (`FBTYPE=BUSY-TENTATIVE`).
    Tentative,
    /// Taken (`FBTYPE=BUSY`).
    Busy,
}

impl BusyKind {
    /// How an event with `fields` takes its time, or `None` if it takes none: it is cancelled,
    /// or transparent.
    #[must_use]
    pub fn of(fields: &NormalizedEvent) -> Option<Self> {
        match (fields.transparency, fields.status) {
            (Transparency::Transparent, _) | (_, EventStatus::Cancelled) => None,
            (Transparency::Opaque, EventStatus::Tentative) => Some(Self::Tentative),
            (Transparency::Opaque, EventStatus::Confirmed) => Some(Self::Busy),
        }
    }
}

/// A span of taken time: from `start`, inclusive, to `end`, exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusyInterval {
    /// Where the interval starts, inclusive.
    pub start: Timestamp,
    /// Where the interval ends, exclusive. An interval whose end is not after its start takes
    /// no time, and [`merge`] drops it.
    pub end: Timestamp,
    /// How the time is taken.
    pub kind: BusyKind,
}

impl BusyInterval {
    /// The interval from `start` to `end`, taken as `kind`.
    #[must_use]
    pub fn new(start: Timestamp, end: Timestamp, kind: BusyKind) -> Self {
        Self { start, end, kind }
    }
}

/// `intervals` clipped to `range` and merged: ordered by start, disjoint, with no two touching
/// intervals of one kind. Where intervals of both kinds overlap, the time is
/// [`BusyKind::Busy`]. An interval that takes no time, or none inside `range`, is dropped.
#[must_use]
pub fn merge(
    intervals: impl IntoIterator<Item = BusyInterval>,
    range: &Window,
) -> Vec<BusyInterval> {
    // Each interval opens and closes a count for its kind; the kind at any time is the highest
    // with a positive count.
    let mut edges: Vec<(Timestamp, BusyKind, i64)> = Vec::new();
    for interval in intervals {
        let start = interval.start.max(range.start());
        let end = interval.end.min(range.end());
        if end > start {
            edges.push((start, interval.kind, 1));
            edges.push((end, interval.kind, -1));
        }
    }
    edges.sort_unstable();

    let mut merged: Vec<BusyInterval> = Vec::new();
    let mut tentative = 0_i64;
    let mut busy = 0_i64;
    let mut index = 0;
    while index < edges.len() {
        let at = edges[index].0;
        let before = kind(tentative, busy);
        while let Some(&(time, edge_kind, delta)) = edges.get(index)
            && time == at
        {
            match edge_kind {
                BusyKind::Tentative => tentative += delta,
                BusyKind::Busy => busy += delta,
            }
            index += 1;
        }
        let after = kind(tentative, busy);
        if before == after {
            continue;
        }
        if let Some(open) = before
            && let Some(last) = merged.last_mut()
        {
            debug_assert_eq!(last.kind, open);
            last.end = at;
        }
        if let Some(kind) = after {
            merged.push(BusyInterval::new(at, at, kind));
        }
    }
    merged
}

/// The kind of time taken where `tentative` and `busy` intervals are open.
fn kind(tentative: i64, busy: i64) -> Option<BusyKind> {
    if busy > 0 {
        Some(BusyKind::Busy)
    } else if tentative > 0 {
        Some(BusyKind::Tentative)
    } else {
        None
    }
}
