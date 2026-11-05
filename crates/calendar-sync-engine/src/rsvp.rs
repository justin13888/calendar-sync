//! RSVP: an attendee's reply to an invitation, planned as the one write each provider takes.
//!
//! An attendee replies by changing its own participation and nothing else (iTIP, RFC 5546
//! section 3.2.3, vendored at `references/rfc/rfc5546.txt`). [`plan`] checks that locally, before
//! any request exists: the replying address must be an attendee of the event and not its
//! organizer, and an [`Rsvp`] derived from an edit ([`Rsvp::from_edit`]) must change nothing the
//! organizer owns. It then reads the provider's current record and returns the [`RsvpPlan`] for
//! the provider's own reply operation:
//!
//! | Provider | Plan | Precondition | [`DuplicateRisk`] |
//! |---|---|---|---|
//! | Google | [`RsvpPlan::Google`]: `PATCH` of the whole `attendees` array, with only the replier's entries changed | `If-Match` on the etag | [`DuplicateRisk::None`]; [`DuplicateRisk::SecondReply`] for a record with no etag |
//! | Microsoft Graph | [`RsvpPlan::Graph`]: `POST` `accept`, `decline` or `tentativelyAccept` | None: the actions are not documented as conditional or idempotent | [`DuplicateRisk::SecondReply`] |
//! | `CalDAV` | [`RsvpPlan::CalDav`]: `PUT` of the object with the replier's `PARTSTAT` changed, through `calendar_sync_ical::IcalDocument::reply` | `If-Schedule-Tag-Match` (RFC 6638 section 3.2.10) | [`DuplicateRisk::None`] |
//!
//! A `CalDAV` server that does not schedule (no `calendar-auto-schedule` in its `DAV` header, or
//! no `Schedule-Tag` on the object) sends no reply, so an RSVP there is
//! [`RsvpError::Unsupported`] rather than a silent local change.
//!
//! # Duplicate replies
//!
//! A reply whose response is lost may be sent again, by this device or another. Where the plan
//! carries a precondition the provider changes on every write, a repeat fails it, and planning
//! again from a fresh read finds the reply held: [`RsvpPlan::Done`]. Graph's reply actions carry
//! no precondition, so a repeat sent before the first one shows in a read sends the organizer a
//! second reply. That risk is reported, not hidden: [`RsvpPlan::duplicate_risk`].
//!
//! ```
//! use calendar_sync_engine::rsvp::{self, DuplicateRisk, Reply, Rsvp, RsvpPlan, RsvpTarget};
//! use calendar_sync_model::{
//!     Attendee, EventTimes, MapperVersion, NormalizedEvent, Organizer, Passthrough, Participation,
//!     Provider, ProviderEvent, ProviderIds, ProviderRevision, TimeValue,
//! };
//!
//! let mut fields = NormalizedEvent::new(EventTimes::at(TimeValue::instant(
//!     "2026-03-02T15:00:00Z".parse().unwrap(),
//! )));
//! fields.organizer = Some(Organizer { address: "alice@example.com".into(), name: None });
//! fields.attendees = vec![Attendee::new("bob@example.com")];
//! let current = ProviderEvent {
//!     ids: ProviderIds::new(Provider::Google, "review"),
//!     revision: Some(ProviderRevision::new("\"3\"")),
//!     mapper_version: MapperVersion::new(1),
//!     fields,
//!     passthrough: Passthrough::new(),
//! };
//!
//! let reply = Rsvp::new("bob@example.com", Reply::Accept);
//! let plan = rsvp::plan(&reply, &current, &RsvpTarget::Google).unwrap();
//! let RsvpPlan::Google { attendees, if_match, .. } = &plan else { panic!() };
//! assert_eq!(attendees[0].participation, Participation::Accepted);
//! assert_eq!(if_match.as_ref(), current.revision.as_ref());
//! assert_eq!(plan.duplicate_risk(), DuplicateRisk::None);
//! ```

use core::fmt;

use calendar_sync_model::{
    Attendee, Field, InstanceKey, MapperVersion, NormalizedEvent, Participation, Passthrough,
    Provider, ProviderEvent, ProviderIds, ProviderRevision,
};
use serde::{Deserialize, Serialize};

use crate::merge::{self, Refused, Role};
use crate::seam::ResourceId;

/// The reply an attendee sends.
///
/// Only the three participation statuses that are replies: `NEEDS-ACTION` is the absence of one,
/// and a delegation needs a delegate, which this minimal iTIP does not carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reply {
    /// `ACCEPTED`.
    Accept,
    /// `DECLINED`.
    Decline,
    /// `TENTATIVE`.
    Tentative,
}

impl Reply {
    /// The participation the reply sets.
    #[must_use]
    pub const fn participation(self) -> Participation {
        match self {
            Self::Accept => Participation::Accepted,
            Self::Decline => Participation::Declined,
            Self::Tentative => Participation::Tentative,
        }
    }

    /// The reply that sets `participation`, where it is one.
    #[must_use]
    pub const fn of(participation: Participation) -> Option<Self> {
        match participation {
            Participation::Accepted => Some(Self::Accept),
            Participation::Declined => Some(Self::Decline),
            Participation::Tentative => Some(Self::Tentative),
            Participation::NeedsAction | Participation::Delegated => None,
        }
    }
}

/// One attendee's reply to one event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rsvp {
    /// The replier's calendar address, as in its attendee entry. Compared ASCII
    /// case-insensitively, without `mailto:`.
    pub address: String,
    /// The reply.
    pub reply: Reply,
}

impl Rsvp {
    /// `address` replies `reply`.
    #[must_use]
    pub fn new(address: impl Into<String>, reply: Reply) -> Self {
        Self {
            address: address.into(),
            reply,
        }
    }

    /// The reply an attendee's edit makes: `ours` is `base` with only the participation of
    /// `address`'s own attendee entries changed, to a reply.
    ///
    /// # Errors
    ///
    /// - [`RsvpError::Refused`], naming each unit the edit changes that the organizer owns: any
    ///   field but the reminders, another attendee's entry, or the rest of the replier's own.
    /// - [`RsvpError::NotOnlyAReply`] where the edit also changes the replier's reminders, which
    ///   it owns but which no reply carries: they are an update of their own.
    /// - [`RsvpError::NoReply`] where the edit leaves the participation as it was, or sets it to
    ///   something that is not a reply.
    pub fn from_edit(
        base: &NormalizedEvent,
        ours: &NormalizedEvent,
        address: &str,
    ) -> Result<Self, RsvpError> {
        let theirs = ProviderEvent {
            ids: ProviderIds::new(Provider::Ics, ""),
            revision: None,
            mapper_version: MapperVersion::default(),
            fields: base.clone(),
            passthrough: Passthrough::new(),
        };
        let role = Role::Attendee {
            address: bare(address).to_owned(),
        };
        let merged = merge::merge(base, ours, &theirs, &role);
        if !merged.refused.is_empty() {
            return Err(RsvpError::Refused(merged.refused));
        }
        if base.reminders != ours.reminders {
            return Err(RsvpError::NotOnlyAReply(vec![Field::Reminders]));
        }
        let mut replies = ours
            .attendees
            .iter()
            .filter(|attendee| is(attendee, address))
            .map(|attendee| attendee.participation);
        let participation = replies.next().ok_or(RsvpError::NotInvited)?;
        let unchanged = base
            .attendees
            .iter()
            .filter(|attendee| is(attendee, address))
            .all(|attendee| attendee.participation == participation);
        match Reply::of(participation) {
            Some(reply) if !unchanged && replies.all(|other| other == participation) => {
                Ok(Self::new(address, reply))
            }
            _ => Err(RsvpError::NoReply),
        }
    }
}

/// The provider a reply is planned for, and what [`plan`] needs to know about it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RsvpTarget {
    /// Google Calendar.
    Google,
    /// Microsoft Graph.
    Graph,
    /// A `CalDAV` server (RFC 4791), scheduling as RFC 6638 describes.
    CalDav {
        /// Whether the server schedules: its `DAV` header lists `calendar-auto-schedule`.
        auto_schedule: bool,
        /// The object's `Schedule-Tag`, as the server last returned it; `None` where the object
        /// is not a scheduling object.
        schedule_tag: Option<ScheduleTag>,
        /// The instance replied to, where the reply is to one instance of a series: the
        /// `RECURRENCE-ID` of its override in the object. `None` replies to the master.
        instance: Option<InstanceKey>,
    },
}

/// A `CalDAV` `Schedule-Tag` (RFC 6638 section 3.2.10): opaque, compared for equality only.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduleTag(String);

impl ScheduleTag {
    /// Wraps a schedule tag exactly as the server sent it, quotes included.
    #[must_use]
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    /// The tag as the server sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The write that sends a reply, planned from the provider's current record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RsvpPlan {
    /// The provider already holds the reply. Nothing is sent.
    Done,
    /// Google: `events.patch` of the resource with `attendees` as the whole array, under
    /// `If-Match`. Google replaces an array whole on a patch, so every entry is sent, each as the
    /// provider holds it, and only the replier's participation differs.
    Google {
        /// The event, or the instance, replied to.
        resource: ResourceId,
        /// The revision the plan was made from. `None` where the record has none, which writes
        /// unconditionally.
        if_match: Option<ProviderRevision>,
        /// The whole attendee array to send.
        attendees: Vec<Attendee>,
    },
    /// Microsoft Graph: `POST /me/events/{id}/{action}`, with no precondition.
    Graph {
        /// The event, or the instance, replied to.
        resource: ResourceId,
        /// The action.
        action: GraphAction,
    },
    /// `CalDAV`: `PUT` of the object at `resource` under `If-Schedule-Tag-Match`, its body the
    /// object as last read with `calendar_sync_ical::IcalDocument::reply(uid, instance, address,
    /// participation)` applied. The engine does not depend on the iCalendar crate: the seam that
    /// holds the object's bytes applies the reply to them.
    CalDav {
        /// The object's href.
        resource: ResourceId,
        /// The schedule tag the plan was made from.
        if_schedule_tag_match: ScheduleTag,
        /// The event's `UID`.
        uid: String,
        /// The instance replied to; `None` for the master.
        instance: Option<InstanceKey>,
        /// The replier's address.
        address: String,
        /// The participation to write.
        participation: Participation,
    },
}

impl RsvpPlan {
    /// What sending the plan again, after a lost response or from another device, can do.
    #[must_use]
    pub fn duplicate_risk(&self) -> DuplicateRisk {
        match self {
            Self::Done
            | Self::CalDav { .. }
            | Self::Google {
                if_match: Some(_), ..
            } => DuplicateRisk::None,
            Self::Google { if_match: None, .. } | Self::Graph { .. } => DuplicateRisk::SecondReply,
        }
    }
}

/// A Microsoft Graph reply action on an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphAction {
    /// `accept`.
    Accept,
    /// `decline`.
    Decline,
    /// `tentativelyAccept`.
    TentativelyAccept,
}

impl GraphAction {
    /// The action's path segment.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::TentativelyAccept => "tentativelyAccept",
        }
    }
}

/// What sending a reply's plan again can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateRisk {
    /// Nothing: a repeat fails its precondition, or changes nothing the organizer is told of.
    None,
    /// A repeat sent before the first reply shows in a read sends the organizer a second reply.
    SecondReply,
}

/// Why a reply cannot be planned.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RsvpError {
    /// The event has no organizer, so it is no invitation and nobody receives a reply.
    NoOrganizer,
    /// The replier is the organizer, who does not reply to its own event.
    Organizer,
    /// The replier is not an attendee of the event. An attendee replies only for itself.
    NotInvited,
    /// The edit changes units the organizer owns. Nothing is planned.
    Refused(Vec<Refused>),
    /// The edit changes fields the replier owns that a reply does not carry.
    NotOnlyAReply(Vec<Field>),
    /// The edit makes no reply: the participation is unchanged, or is not a reply.
    NoReply,
    /// The provider cannot send the reply.
    Unsupported(Unsupported),
}

/// Why a provider cannot send a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unsupported {
    /// The `CalDAV` server does not schedule (`calendar-auto-schedule`), so it sends no reply.
    NoAutoSchedule,
    /// The `CalDAV` object has no `Schedule-Tag`, so it is not a scheduling object.
    NoScheduleTag,
    /// The `CalDAV` object's event carries no `UID` to name it by.
    NoUid,
}

impl fmt::Display for RsvpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOrganizer => f.write_str("the event has no organizer"),
            Self::Organizer => f.write_str("the organizer does not reply to its own event"),
            Self::NotInvited => f.write_str("the replier is not an attendee"),
            Self::Refused(refused) => {
                write!(
                    f,
                    "the edit changes {} unit(s) the organizer owns",
                    refused.len()
                )
            }
            Self::NotOnlyAReply(fields) => {
                write!(
                    f,
                    "the edit also changes {fields:?}, which a reply does not carry"
                )
            }
            Self::NoReply => f.write_str("the edit makes no reply"),
            Self::Unsupported(Unsupported::NoAutoSchedule) => {
                f.write_str("the CalDAV server does not schedule")
            }
            Self::Unsupported(Unsupported::NoScheduleTag) => {
                f.write_str("the CalDAV object is not a scheduling object")
            }
            Self::Unsupported(Unsupported::NoUid) => f.write_str("the CalDAV event has no UID"),
        }
    }
}

impl core::error::Error for RsvpError {}

/// Plans `rsvp` against `current`, the provider's record of the event as read just before.
///
/// Ownership is checked first, then whether the provider can send the reply, then whether it
/// already holds it. The function is pure: the same inputs plan the same write on every device.
///
/// # Errors
///
/// [`RsvpError::NoOrganizer`], [`RsvpError::Organizer`] and [`RsvpError::NotInvited`] for a
/// replier who may not reply, and [`RsvpError::Unsupported`] for a `CalDAV` server that would not
/// send it.
pub fn plan(
    rsvp: &Rsvp,
    current: &ProviderEvent,
    target: &RsvpTarget,
) -> Result<RsvpPlan, RsvpError> {
    let fields = &current.fields;
    let organizer = fields.organizer.as_ref().ok_or(RsvpError::NoOrganizer)?;
    if bare(&organizer.address).eq_ignore_ascii_case(bare(&rsvp.address)) {
        return Err(RsvpError::Organizer);
    }
    let participation = rsvp.reply.participation();
    let mut own = fields
        .attendees
        .iter()
        .filter(|attendee| is(attendee, &rsvp.address))
        .peekable();
    if own.peek().is_none() {
        return Err(RsvpError::NotInvited);
    }
    let held = own.all(|attendee| attendee.participation == participation);

    let resource = ResourceId::of(current);
    let plan = match target {
        RsvpTarget::Google => RsvpPlan::Google {
            resource,
            if_match: current.revision.clone(),
            attendees: fields
                .attendees
                .iter()
                .map(|attendee| {
                    let mut attendee = attendee.clone();
                    if is(&attendee, &rsvp.address) {
                        attendee.participation = participation;
                    }
                    attendee
                })
                .collect(),
        },
        RsvpTarget::Graph => RsvpPlan::Graph {
            resource,
            action: match rsvp.reply {
                Reply::Accept => GraphAction::Accept,
                Reply::Decline => GraphAction::Decline,
                Reply::Tentative => GraphAction::TentativelyAccept,
            },
        },
        RsvpTarget::CalDav {
            auto_schedule,
            schedule_tag,
            instance,
        } => {
            if !auto_schedule {
                return Err(RsvpError::Unsupported(Unsupported::NoAutoSchedule));
            }
            let tag = schedule_tag
                .clone()
                .ok_or(RsvpError::Unsupported(Unsupported::NoScheduleTag))?;
            let uid = current
                .ids
                .ical_uid
                .clone()
                .ok_or(RsvpError::Unsupported(Unsupported::NoUid))?;
            RsvpPlan::CalDav {
                resource,
                if_schedule_tag_match: tag,
                uid,
                instance: instance.clone(),
                address: rsvp.address.clone(),
                participation,
            }
        }
    };
    Ok(if held { RsvpPlan::Done } else { plan })
}

/// Whether `attendee` is the replier at `address`.
fn is(attendee: &Attendee, address: &str) -> bool {
    bare(&attendee.address).eq_ignore_ascii_case(bare(address))
}

/// An address without its `mailto:` scheme.
fn bare(address: &str) -> &str {
    let address = address.trim();
    match address.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => &address[7..],
        _ => address,
    }
}
