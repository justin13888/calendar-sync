//! Time values: instant, zoned, floating and all-day, and the start and end of an event.

use core::fmt;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use serde::{Deserialize, Serialize};

/// A time zone identifier, as the provider or the mapper wrote it.
///
/// This is an IANA name such as `Europe/Berlin` wherever the provider gave one or the mapper
/// converted to one (Graph's Windows names are converted by the Graph mapper). A `TZID` defined
/// only by an iCalendar `VTIMEZONE` is kept as written. The model never resolves it: a crate that
/// needs offsets takes a time zone database as an input.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeZoneId(String);

impl TimeZoneId {
    /// Wraps a time zone identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TimeZoneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A point in time, in one of the four forms calendars use.
///
/// The derived ordering is structural, for use as a map key. It is not chronological: comparing a
/// zoned value with an instant needs a time zone database, which this crate does not hold.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TimeValue {
    /// An absolute instant, written in UTC (iCalendar `...Z`, Graph `originalStart`).
    Instant {
        /// The instant.
        timestamp: Timestamp,
    },
    /// A civil date and time in a named time zone. The zone is kept, never resolved to an offset
    /// here, so a later change to the zone's rules is honoured.
    Zoned {
        /// The wall-clock date and time.
        date_time: DateTime,
        /// The zone the wall-clock time is in.
        time_zone: TimeZoneId,
    },
    /// A civil date and time in no zone: it happens at that wall-clock time wherever the viewer
    /// is. It is never coerced to a zone.
    Floating {
        /// The wall-clock date and time.
        date_time: DateTime,
    },
    /// A whole day, with no time of day.
    AllDay {
        /// The day.
        date: Date,
    },
}

impl TimeValue {
    /// An instant.
    #[must_use]
    pub fn instant(timestamp: Timestamp) -> Self {
        Self::Instant { timestamp }
    }

    /// A civil date and time in a named zone.
    #[must_use]
    pub fn zoned(date_time: DateTime, time_zone: TimeZoneId) -> Self {
        Self::Zoned {
            date_time,
            time_zone,
        }
    }

    /// A floating civil date and time.
    #[must_use]
    pub fn floating(date_time: DateTime) -> Self {
        Self::Floating { date_time }
    }

    /// A whole day.
    #[must_use]
    pub fn all_day(date: Date) -> Self {
        Self::AllDay { date }
    }

    /// Which of the four forms this value takes.
    #[must_use]
    pub fn kind(&self) -> TimeKind {
        match self {
            Self::Instant { .. } => TimeKind::Instant,
            Self::Zoned { .. } => TimeKind::Zoned,
            Self::Floating { .. } => TimeKind::Floating,
            Self::AllDay { .. } => TimeKind::AllDay,
        }
    }
}

/// The form of a [`TimeValue`], without its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeKind {
    /// [`TimeValue::Instant`].
    Instant,
    /// [`TimeValue::Zoned`].
    Zoned,
    /// [`TimeValue::Floating`].
    Floating,
    /// [`TimeValue::AllDay`].
    AllDay,
}

impl TimeKind {
    /// Whether an event may start in `self` and end in `other`.
    ///
    /// All-day pairs with all-day, and floating with floating. An instant and a zoned value pair
    /// with each other, since Google and Graph let the start and end carry different zones.
    #[must_use]
    pub fn pairs_with(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::AllDay, Self::AllDay)
                | (Self::Floating, Self::Floating)
                | (Self::Instant | Self::Zoned, Self::Instant | Self::Zoned)
        )
    }
}

/// When an event starts and ends.
///
/// The end is exclusive. For an all-day event it is the day after the last day, as in iCalendar,
/// Google and Graph. An absent end means the event takes no time (iCalendar section 3.6.1: a
/// `DTSTART` with no `DTEND` or `DURATION`).
///
/// [`EventTimes::new`] checks that the start and end pair (see [`TimeKind::pairs_with`]) and that
/// the end is not before the start wherever that can be decided without a time zone database:
/// two all-day, two floating, two instant, or two zoned values in the same zone. Deserializing
/// runs the same checks.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "EventTimesRepr", into = "EventTimesRepr")]
pub struct EventTimes {
    start: TimeValue,
    end: Option<TimeValue>,
}

impl EventTimes {
    /// The start and end of an event.
    ///
    /// # Errors
    ///
    /// [`EventTimesError::Unpaired`] if the end's form cannot pair with the start's, and
    /// [`EventTimesError::EndBeforeStart`] if the end is decidably before the start.
    pub fn new(start: TimeValue, end: Option<TimeValue>) -> Result<Self, EventTimesError> {
        if let Some(end) = &end {
            if !start.kind().pairs_with(end.kind()) {
                return Err(EventTimesError::Unpaired {
                    start: start.kind(),
                    end: end.kind(),
                });
            }
            if end_before_start(&start, end) {
                return Err(EventTimesError::EndBeforeStart);
            }
        }
        Ok(Self { start, end })
    }

    /// An event with no duration.
    #[must_use]
    pub fn at(start: TimeValue) -> Self {
        Self { start, end: None }
    }

    /// When the event starts.
    #[must_use]
    pub fn start(&self) -> &TimeValue {
        &self.start
    }

    /// When the event ends, exclusive. `None` means it takes no time.
    #[must_use]
    pub fn end(&self) -> Option<&TimeValue> {
        self.end.as_ref()
    }

    /// Whether the event is all-day.
    #[must_use]
    pub fn is_all_day(&self) -> bool {
        self.start.kind() == TimeKind::AllDay
    }
}

/// Whether `end` is before `start`, where that is decidable without a time zone database.
fn end_before_start(start: &TimeValue, end: &TimeValue) -> bool {
    match (start, end) {
        (TimeValue::AllDay { date: s }, TimeValue::AllDay { date: e }) => e < s,
        (TimeValue::Floating { date_time: s }, TimeValue::Floating { date_time: e }) => e < s,
        (TimeValue::Instant { timestamp: s }, TimeValue::Instant { timestamp: e }) => e < s,
        (
            TimeValue::Zoned {
                date_time: s,
                time_zone: sz,
            },
            TimeValue::Zoned {
                date_time: e,
                time_zone: ez,
            },
        ) => sz == ez && e < s,
        _ => false,
    }
}

/// Why an [`EventTimes`] was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventTimesError {
    /// The start and end are in forms that cannot pair, such as an all-day start and a timed end.
    Unpaired {
        /// The start's form.
        start: TimeKind,
        /// The end's form.
        end: TimeKind,
    },
    /// The end is before the start.
    EndBeforeStart,
}

impl fmt::Display for EventTimesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unpaired { start, end } => {
                write!(f, "an event cannot start {start:?} and end {end:?}")
            }
            Self::EndBeforeStart => f.write_str("the event ends before it starts"),
        }
    }
}

impl core::error::Error for EventTimesError {}

/// The serialized shape of [`EventTimes`], checked on the way in.
#[derive(Serialize, Deserialize)]
struct EventTimesRepr {
    start: TimeValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<TimeValue>,
}

impl TryFrom<EventTimesRepr> for EventTimes {
    type Error = EventTimesError;

    fn try_from(repr: EventTimesRepr) -> Result<Self, Self::Error> {
        Self::new(repr.start, repr.end)
    }
}

impl From<EventTimes> for EventTimesRepr {
    fn from(times: EventTimes) -> Self {
        Self {
            start: times.start,
            end: times.end,
        }
    }
}
