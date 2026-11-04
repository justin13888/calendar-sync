//! Provider-neutral calendar event model: series, overrides, stable instance ids, and time
//! variants (instant, zoned, floating, all-day).
//!
//! This crate is pure. It reads no clock, filesystem, environment or ambient randomness, and it
//! builds for `wasm32-unknown-unknown`. `jiff` is its only time library, and it holds no time
//! zone database: a zoned time keeps its zone's name, and a crate that needs offsets takes a
//! database as an input.
//!
//! # Shape
//!
//! - [`NormalizedEvent`]: the fields of one event, independent of any provider.
//! - [`ProviderEvent`]: one resource as a provider holds it. It carries the [`ProviderIds`], an
//!   opaque [`ProviderRevision`] compared only for equality, the [`MapperVersion`] that read it,
//!   its normalized fields, and a [`Passthrough`] of the fields the model has no place for.
//! - [`EventSeries`]: a master and its [`Override`]s, keyed by [`InstanceKey`], the provider's
//!   own identity for an instance (Google `originalStartTime`, Graph `originalStart`, `CalDAV`
//!   `RECURRENCE-ID`). Series and overrides are explicit, never inferred.
//! - [`TimeValue`]: an instant, a zoned civil time, a floating civil time, or a whole day.
//! - [`Recurrence`] and [`RecurrenceRule`]: the series' pattern, mirroring JSCalendar.
//! - [`Loss`]: what a mapping could not carry, by field and reason. Nothing is dropped silently.
//! - [`graph::recurrence_losses`]: which recurrences Microsoft Graph cannot hold.
//!
//! Every type serializes with `serde`. Field names are camelCase, as in JSCalendar.
//!
//! # Relation to JSCalendar
//!
//! The model stays close to JSCalendar (RFC 8984, vendored at `references/rfc/rfc8984.txt`, and
//! its revision `references/drafts/draft-ietf-calext-jscalendarbis-22.txt`), so a JMAP provider
//! can be added. Recurrence rules, statuses, alerts and the default-alerts flag follow it. Each
//! deliberate divergence, and why:
//!
//! | JSCalendar | This model | Why |
//! |---|---|---|
//! | `start` as a `LocalDateTime` plus `timeZone`, `showWithoutTime`, and a `duration` | [`EventTimes`]: a start and an exclusive end, each a [`TimeValue`] | Google, Graph and most iCalendar data carry an end, and Google and Graph let the start and end carry different zones. A floating time is its own variant, never a missing `timeZone` |
//! | `recurrenceOverrides`: patches keyed by `recurrenceId` inside the master | [`EventSeries::overrides`]: whole [`ProviderEvent`]s keyed by [`InstanceKey`] | Google and Graph hold each override as its own resource with its own id and revision, so an override is a record, not a patch |
//! | `recurrenceId` is a `LocalDateTime` in the master's zone | [`InstanceKey`] holds the provider's value in its own form | Graph writes `originalStart` in UTC, and a `RECURRENCE-ID` may be in UTC beside a zoned series |
//! | An excluded instance is an override with `excluded: true` | [`Recurrence::excluded_dates`], or [`Override::Cancelled`] where the provider keeps the cancelled instance as a resource | Mirrors where each provider stores the fact |
//! | No `RDATE` list; extra instances are overrides | [`Recurrence::dates`] | iCalendar `RDATE` maps to it directly |
//! | jscalendarbis keeps one `recurrenceRule` and obsoletes `excludedRecurrenceRules` | [`Recurrence::rules`] and [`Recurrence::exclusion_rules`] are lists | iCalendar data with several `RRULE`s or an `EXRULE` exists; it is carried, and a target that cannot hold it reports a [`Loss`] |
//! | `count` and `until` are separate optional properties | [`RuleEnd`], one of never, count or until | They are mutually exclusive, and the enum cannot hold both |
//! | `byMonth` holds strings such as `"5L"` | [`RuleMonth`] holds the number and the leap flag | Typed rather than parsed at each use |
//! | `participants` holds organizer and attendees in one map, with roles | [`NormalizedEvent::organizer`] and [`NormalizedEvent::attendees`] | Every provider separates them |
//! | `locations` and `virtualLocations` maps | [`NormalizedEvent::location`], one free-text string | Google, Graph and `LOCATION` hold one string |
//! | `description` with `descriptionContentType` | [`NormalizedEvent::description`] in plain text | A mapper converting HTML reports an approximation |
//! | `freeBusyStatus` `busy` or `free` | [`Transparency`] `Opaque` or `Transparent` | iCalendar's `TRANSP` names |
//! | `privacy` `public`, `private` or `secret` | [`Visibility`] adds `Default`, and names `secret` `Confidential` | Google and an absent `CLASS` defer to the calendar's default |
//! | `useDefaultAlerts`, reserved for JMAP in jscalendarbis | [`Reminders::use_default`] | Google has it |
//! | Vendor properties on the object | [`Passthrough`] on each [`ProviderEvent`] | Unknown fields belong to the provider record that held them |

mod event;
pub mod graph;
mod loss;
mod provider;
mod recurrence;
mod series;
mod time;

pub use event::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventStatus, NormalizedEvent,
    Organizer, ParticipantKind, Participation, RelativeTo, Reminders, Transparency, Visibility,
};
pub use loss::{Field, Loss, LossReason};
pub use provider::{
    MapperVersion, Passthrough, PassthroughField, Provider, ProviderEvent, ProviderIds,
    ProviderRevision,
};
pub use recurrence::{
    Frequency, NDay, Recurrence, RecurrenceRule, RuleEnd, RuleMonth, Skip, Weekday,
};
pub use series::{EventSeries, InstanceKey, InstanceRange, Override};
pub use time::{EventTimes, EventTimesError, TimeKind, TimeValue, TimeZoneId};

#[cfg(test)]
mod tests;
