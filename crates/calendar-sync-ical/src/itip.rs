//! Minimal iTIP (RFC 5546) for an attendee: replying by changing its own `PARTSTAT` in a
//! scheduling object, as a `CalDAV` server that schedules (RFC 6638) expects.
//!
//! On such a server the attendee does not send a `METHOD:REPLY` itself. It stores its copy of the
//! event with its own `PARTSTAT` changed, and the server sends the reply to the organizer
//! (RFC 6638 section 3.2.2.3). [`IcalDocument::reply`] makes that change and nothing else: it
//! rewrites the `PARTSTAT` parameter of the attendee's own `ATTENDEE` line, and every other byte of
//! the document stays as it was, the line's other parameters (`RSVP`, `SCHEDULE-STATUS`, `X-`
//! parameters) included.
//!
//! It refuses, before anything is written, what an attendee may not do:
//!
//! - reply as the organizer, or as someone who is not an attendee: only the attendee replying
//!   changes, and only its participation (RFC 5546 section 3.2.3);
//! - reply with a status that is not a reply: `NEEDS-ACTION` is the absence of one, and
//!   `DELEGATED` needs a delegate, which this minimal iTIP does not carry;
//! - reply where the server will not send it: an `ORGANIZER` whose `SCHEDULE-AGENT` is `CLIENT` or
//!   `NONE` tells the server to do no scheduling (RFC 6638 section 7.1).
//!
//! iMIP (sending the reply by email) is out of scope.

use core::fmt;

use calendar_sync_model::{InstanceKey, Participation};
use ical::tree::cst::{IcalCst, IcalItem};

use crate::IcalDocument;
use crate::line;
use crate::read;

/// Why an attendee's reply cannot be written into a document.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItipError {
    /// The participation is not a reply: only `ACCEPTED`, `DECLINED` and `TENTATIVE` are.
    NotAReply(Participation),
    /// The document holds no readable `VEVENT` with this `UID` and instance. A reply to one
    /// instance of a series needs that instance's override `VEVENT`.
    NoEvent,
    /// The `VEVENT` has no `ORGANIZER`, so it is not a scheduling object and nobody receives a
    /// reply.
    NoOrganizer,
    /// The address is the organizer's. The organizer does not reply to its own event.
    Organizer,
    /// The address is not an `ATTENDEE` of the `VEVENT`. An attendee replies only for itself.
    NotAttendee,
    /// The `ORGANIZER`'s `SCHEDULE-AGENT` is not `SERVER`, so the server sends no reply.
    NotServerScheduled(String),
}

impl fmt::Display for ItipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAReply(participation) => write!(f, "{participation:?} is not a reply"),
            Self::NoEvent => f.write_str("no such VEVENT"),
            Self::NoOrganizer => f.write_str("the VEVENT has no ORGANIZER"),
            Self::Organizer => f.write_str("the organizer does not reply to its own event"),
            Self::NotAttendee => f.write_str("the address is not an ATTENDEE of the VEVENT"),
            Self::NotServerScheduled(agent) => {
                write!(f, "the ORGANIZER's SCHEDULE-AGENT is {agent}, not SERVER")
            }
        }
    }
}

impl core::error::Error for ItipError {}

impl IcalDocument {
    /// Writes `address`'s reply into the `VEVENT` with `uid`, the master where `instance` is
    /// `None`, else the override of `instance`: the `PARTSTAT` of its `ATTENDEE` line becomes
    /// `participation`.
    ///
    /// Returns whether the document changed. Where the line already holds `participation`, nothing
    /// is rewritten. Addresses compare ASCII case-insensitively, without `mailto:`.
    ///
    /// A reply to the master changes the master only: an override keeps its own `PARTSTAT`, as
    /// RFC 5546 keeps each instance's reply apart.
    ///
    /// # Errors
    ///
    /// An [`ItipError`] for each refusal the module lists, before anything is written.
    pub fn reply(
        &mut self,
        uid: &str,
        instance: Option<&InstanceKey>,
        address: &str,
        participation: Participation,
    ) -> Result<bool, ItipError> {
        let partstat = match participation {
            Participation::Accepted => "ACCEPTED",
            Participation::Declined => "DECLINED",
            Participation::Tentative => "TENTATIVE",
            Participation::NeedsAction | Participation::Delegated => {
                return Err(ItipError::NotAReply(participation));
            }
        };
        let (groups, _) = read::groups(&self.calendars);
        let group = groups
            .into_iter()
            .find(|group| group.uid == uid)
            .ok_or(ItipError::NoEvent)?;
        let mut group = group;
        let located = match instance {
            None => group.master,
            Some(key) => group.overrides.remove(key),
        }
        .ok_or(ItipError::NoEvent)?;
        let IcalItem::Component(component) =
            &mut self.calendars[located.calendar].items[located.item]
        else {
            return Err(ItipError::NoEvent);
        };
        check_organizer(component, address)?;

        let wanted = read::address(address);
        let mine: Vec<usize> = line::positions(component, "ATTENDEE")
            .into_iter()
            .filter(|&at| match &component.items[at] {
                IcalItem::Prop(attendee) => {
                    read::address(&line::text(attendee)).eq_ignore_ascii_case(&wanted)
                }
                _ => false,
            })
            .collect();
        if mine.is_empty() {
            return Err(ItipError::NotAttendee);
        }
        let mut changed = false;
        for at in mine {
            if let IcalItem::Prop(old) = &component.items[at] {
                let current = line::param(old, "PARTSTAT");
                if current.is_some_and(|current| current.eq_ignore_ascii_case(partstat)) {
                    continue;
                }
                let value = line::raw(old);
                let new = line::rewrite(old, &[("PARTSTAT", Some(partstat.to_owned()))], &value);
                component.items[at] = IcalItem::Prop(new);
                changed = true;
            }
        }
        Ok(changed)
    }
}

/// Refuses a reply the organizer would make, or one the server would not send.
fn check_organizer(component: &IcalCst<'_>, address: &str) -> Result<(), ItipError> {
    let organizer = line::first(component, "ORGANIZER").ok_or(ItipError::NoOrganizer)?;
    if read::address(&line::text(organizer)).eq_ignore_ascii_case(&read::address(address)) {
        return Err(ItipError::Organizer);
    }
    match line::param(organizer, "SCHEDULE-AGENT") {
        Some(agent) if !agent.eq_ignore_ascii_case("SERVER") => {
            Err(ItipError::NotServerScheduled(agent))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use calendar_sync_model::TimeValue;

    use super::*;

    /// An attendee's copy: a weekly series with one moved instance, a folded line, and parameters
    /// the model does not map.
    const INVITE: &str = concat!(
        "BEGIN:VCALENDAR\r\n",
        "VERSION:2.0\r\n",
        "PRODID:-//Example Corp//Calendar//EN\r\n",
        "BEGIN:VEVENT\r\n",
        "UID:review@example.com\r\n",
        "DTSTAMP:20260101T000000Z\r\n",
        "SEQUENCE:2\r\n",
        "DTSTART:20260105T150000Z\r\n",
        "DTEND:20260105T160000Z\r\n",
        "RRULE:FREQ=WEEKLY\r\n",
        "SUMMARY:Review\r\n",
        "ORGANIZER;CN=Alice:mailto:alice@example.com\r\n",
        "ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n",
        "ATTENDEE;CN=Bob;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;SCHEDULE-STATUS=1.2;X-NOTE=keep:\r\n",
        " mailto:Bob@Example.com\r\n",
        "ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:carol@example.com\r\n",
        "END:VEVENT\r\n",
        "BEGIN:VEVENT\r\n",
        "UID:review@example.com\r\n",
        "DTSTAMP:20260101T000000Z\r\n",
        "RECURRENCE-ID:20260112T150000Z\r\n",
        "DTSTART:20260112T170000Z\r\n",
        "DTEND:20260112T180000Z\r\n",
        "ORGANIZER;CN=Alice:mailto:alice@example.com\r\n",
        "ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n",
        "END:VEVENT\r\n",
        "END:VCALENDAR\r\n",
    );

    const UID: &str = "review@example.com";

    fn text(document: &IcalDocument) -> String {
        String::from_utf8(document.to_bytes()).unwrap()
    }

    fn moved() -> InstanceKey {
        InstanceKey::new(TimeValue::instant("2026-01-12T15:00:00Z".parse().unwrap()))
    }

    #[test]
    fn a_reply_rewrites_only_the_attendees_partstat() {
        let mut document = IcalDocument::parse(INVITE.as_bytes());
        let changed = document
            .reply(UID, None, "bob@example.com", Participation::Accepted)
            .unwrap();
        assert!(changed);
        // The rewritten line is a new line, so its folds are the writer's; compare it unfolded.
        let unfold = |text: &str| text.replace("\r\n ", "");
        let expected = unfold(INVITE).replace(
            "ATTENDEE;CN=Bob;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;",
            "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED;RSVP=TRUE;",
        );
        assert_ne!(expected, unfold(INVITE), "the replacement must apply");
        assert_eq!(unfold(&text(&document)), expected);
    }

    #[test]
    fn a_reply_already_held_changes_no_byte() {
        let mut document = IcalDocument::parse(INVITE.as_bytes());
        document
            .reply(UID, None, "bob@example.com", Participation::Tentative)
            .unwrap();
        let once = text(&document);
        let again = document
            .reply(UID, None, "BOB@example.com", Participation::Tentative)
            .unwrap();
        assert!(!again);
        assert_eq!(text(&document), once);
    }

    #[test]
    fn a_reply_to_an_instance_changes_only_its_override() {
        let mut document = IcalDocument::parse(INVITE.as_bytes());
        let changed = document
            .reply(
                UID,
                Some(&moved()),
                "mailto:bob@example.com",
                Participation::Declined,
            )
            .unwrap();
        assert!(changed);
        assert_eq!(
            text(&document),
            INVITE.replace(
                "ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com",
                "ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.com",
            )
        );
    }

    #[test]
    fn what_an_attendee_may_not_do_is_refused_before_any_write() {
        let unmoved = InstanceKey::new(TimeValue::instant("2026-01-19T15:00:00Z".parse().unwrap()));
        let refusals = [
            (
                None,
                "bob@example.com",
                Participation::NeedsAction,
                ItipError::NotAReply(Participation::NeedsAction),
            ),
            (
                None,
                "bob@example.com",
                Participation::Delegated,
                ItipError::NotAReply(Participation::Delegated),
            ),
            (
                None,
                "alice@example.com",
                Participation::Declined,
                ItipError::Organizer,
            ),
            (
                None,
                "mallory@example.com",
                Participation::Accepted,
                ItipError::NotAttendee,
            ),
            // Carol is invited to the series, but the moved instance does not list her.
            (
                Some(moved()),
                "carol@example.com",
                Participation::Accepted,
                ItipError::NotAttendee,
            ),
            // No override holds this instance.
            (
                Some(unmoved),
                "bob@example.com",
                Participation::Accepted,
                ItipError::NoEvent,
            ),
        ];
        for (instance, address, participation, error) in refusals {
            let mut document = IcalDocument::parse(INVITE.as_bytes());
            assert_eq!(
                document.reply(UID, instance.as_ref(), address, participation),
                Err(error)
            );
            assert_eq!(text(&document), INVITE);
        }
        let mut document = IcalDocument::parse(INVITE.as_bytes());
        assert_eq!(
            document.reply(
                "other@example.com",
                None,
                "bob@example.com",
                Participation::Accepted
            ),
            Err(ItipError::NoEvent)
        );
    }

    #[test]
    fn no_organizer_or_a_client_scheduled_one_is_refused() {
        let unorganized = INVITE.replace("ORGANIZER;CN=Alice:mailto:alice@example.com\r\n", "");
        let mut document = IcalDocument::parse(unorganized.as_bytes());
        assert_eq!(
            document.reply(UID, None, "bob@example.com", Participation::Accepted),
            Err(ItipError::NoOrganizer)
        );

        let client = INVITE.replace(
            "ORGANIZER;CN=Alice:",
            "ORGANIZER;CN=Alice;SCHEDULE-AGENT=CLIENT:",
        );
        let mut document = IcalDocument::parse(client.as_bytes());
        assert_eq!(
            document.reply(UID, None, "bob@example.com", Participation::Accepted),
            Err(ItipError::NotServerScheduled("CLIENT".to_owned()))
        );
        assert_eq!(text(&document), client);

        let server = INVITE.replace(
            "ORGANIZER;CN=Alice:",
            "ORGANIZER;CN=Alice;SCHEDULE-AGENT=server:",
        );
        let mut document = IcalDocument::parse(server.as_bytes());
        assert_eq!(
            document.reply(UID, None, "bob@example.com", Participation::Accepted),
            Ok(true)
        );
    }
}
