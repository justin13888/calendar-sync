//! RSVP planning against the testkit's fake providers: ownership is checked before any request
//! exists, each provider's plan sends the reply, and each provider's remaining duplicate-reply
//! risk is asserted as it is.

use calendar_sync_engine::rsvp::{self, GraphAction, ScheduleTag, Unsupported};
use calendar_sync_engine::{
    DuplicateRisk, Failure, Outcome, Refused, Reply, Request, ResourceId, Rsvp, RsvpError,
    RsvpPlan, RsvpTarget,
};
use calendar_sync_ical::{IcalDocument, Source};
use calendar_sync_model::{
    Alert, AlertAction, AlertTrigger, Attendee, AttendeeRole, EventTimes, Field, NormalizedEvent,
    Organizer, Participation, Passthrough, Provider, ProviderEvent, ProviderIds, ProviderRevision,
    RelativeTo, TimeValue,
};
use calendar_sync_testkit::caldav::{self, FakeCalDav, Precondition};
use calendar_sync_testkit::google::FakeGoogle;
use calendar_sync_testkit::graph::FakeGraph;
use calendar_sync_testkit::seam::{GoogleSeam, GraphSeam, MAPPER_VERSION};
use calendar_sync_testkit::{Fault, FaultPlan};
use jiff::{SignedDuration, Timestamp};

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";
const CAROL: &str = "carol@example.com";

/// A meeting Alice organizes, with Alice, Bob and Carol invited.
fn meeting() -> NormalizedEvent {
    let start = Timestamp::from_second(1_772_463_600).unwrap();
    let end = Timestamp::from_second(1_772_467_200).unwrap();
    let times = EventTimes::new(TimeValue::instant(start), Some(TimeValue::instant(end))).unwrap();
    let mut event = NormalizedEvent::new(times);
    event.title = Some("Review".to_owned());
    event.organizer = Some(Organizer {
        address: ALICE.to_owned(),
        name: Some("Alice".to_owned()),
    });
    let mut alice = Attendee::new(ALICE);
    alice.participation = Participation::Accepted;
    let mut carol = Attendee::new(CAROL);
    carol.role = AttendeeRole::Optional;
    event.attendees = vec![alice, Attendee::new(BOB), carol];
    event
}

fn participation(event: &NormalizedEvent, address: &str) -> Participation {
    event
        .attendees
        .iter()
        .find(|attendee| attendee.address.eq_ignore_ascii_case(address))
        .unwrap()
        .participation
}

fn read(outcome: Outcome) -> ProviderEvent {
    match outcome {
        Outcome::Read(Some(event)) => event,
        other => panic!("expected a read, got {other:?}"),
    }
}

// Ownership

#[test]
fn only_an_invited_attendee_other_than_the_organizer_may_reply() {
    let mut google = FakeGoogle::new(FaultPlan::none());
    google.remote_put("review", GoogleSeam::body(&meeting()));
    let seam = GoogleSeam::new(None);
    let resource = ResourceId::new("review");
    let current = read(seam.perform(&mut google, &Request::Read { resource }));
    let calls = google.calls();

    for target in [
        RsvpTarget::Google,
        RsvpTarget::Graph,
        RsvpTarget::CalDav {
            auto_schedule: true,
            schedule_tag: Some(ScheduleTag::new("\"s1\"")),
            instance: None,
        },
    ] {
        let organizer = Rsvp::new("MAILTO:Alice@Example.com", Reply::Decline);
        assert_eq!(
            rsvp::plan(&organizer, &current, &target),
            Err(RsvpError::Organizer)
        );
        let stranger = Rsvp::new("mallory@example.com", Reply::Accept);
        assert_eq!(
            rsvp::plan(&stranger, &current, &target),
            Err(RsvpError::NotInvited)
        );
        let mut unorganized = current.clone();
        unorganized.fields.organizer = None;
        assert_eq!(
            rsvp::plan(&Rsvp::new(BOB, Reply::Accept), &unorganized, &target),
            Err(RsvpError::NoOrganizer)
        );
    }
    // Every refusal is local: the provider saw no call.
    assert_eq!(google.calls(), calls);
}

#[test]
fn an_edit_is_a_reply_only_when_it_changes_nothing_but_the_own_participation() {
    let base = meeting();

    let mut accepted = base.clone();
    accepted.attendees[1].participation = Participation::Accepted;
    assert_eq!(
        Rsvp::from_edit(&base, &accepted, "Bob@Example.com"),
        Ok(Rsvp::new("Bob@Example.com", Reply::Accept))
    );

    // Organizer-owned fields are refused, each named.
    let mut retitled = accepted.clone();
    retitled.title = Some("My review".to_owned());
    retitled.location = Some("Room 2".to_owned());
    assert_eq!(
        Rsvp::from_edit(&base, &retitled, BOB),
        Err(RsvpError::Refused(vec![
            Refused {
                field: Field::Title,
                attendee: None,
            },
            Refused {
                field: Field::Location,
                attendee: None,
            },
        ]))
    );

    // Another attendee's reply is theirs.
    let mut for_carol = accepted.clone();
    for_carol.attendees[2].participation = Participation::Declined;
    assert_eq!(
        Rsvp::from_edit(&base, &for_carol, BOB),
        Err(RsvpError::Refused(vec![Refused {
            field: Field::Attendees,
            attendee: Some(CAROL.to_owned()),
        }]))
    );

    // The rest of the replier's own entry is the organizer's.
    let mut promoted = accepted.clone();
    promoted.attendees[1].role = AttendeeRole::Chair;
    assert_eq!(
        Rsvp::from_edit(&base, &promoted, BOB),
        Err(RsvpError::Refused(vec![Refused {
            field: Field::Attendees,
            attendee: Some(BOB.to_owned()),
        }]))
    );

    // Reminders are the replier's own, but no reply carries them.
    let mut reminded = accepted.clone();
    reminded.reminders.alerts.push(Alert {
        trigger: AlertTrigger::Offset {
            offset: SignedDuration::from_mins(-10),
            relative_to: RelativeTo::Start,
        },
        action: AlertAction::Display,
    });
    assert_eq!(
        Rsvp::from_edit(&base, &reminded, BOB),
        Err(RsvpError::NotOnlyAReply(vec![Field::Reminders]))
    );

    // No change, or a change to something that is not a reply, makes no reply.
    assert_eq!(Rsvp::from_edit(&base, &base, BOB), Err(RsvpError::NoReply));
    let mut delegated = base.clone();
    delegated.attendees[1].participation = Participation::Delegated;
    assert_eq!(
        Rsvp::from_edit(&base, &delegated, BOB),
        Err(RsvpError::NoReply)
    );
    let mut undecided = accepted.clone();
    undecided.attendees[1].participation = Participation::NeedsAction;
    assert_eq!(
        Rsvp::from_edit(&accepted, &undecided, BOB),
        Err(RsvpError::NoReply)
    );

    // Removing oneself is the organizer's call.
    let mut left = base.clone();
    left.attendees.remove(1);
    assert!(matches!(
        Rsvp::from_edit(&base, &left, BOB),
        Err(RsvpError::Refused(_))
    ));
}

// Google

/// Performs a Google plan as `events.patch` of the attendee array: the fake stores the whole
/// event, so the patch is the current record with its attendees replaced.
fn send_google(google: &mut FakeGoogle, current: &ProviderEvent, plan: &RsvpPlan) -> Outcome {
    let RsvpPlan::Google {
        resource,
        if_match,
        attendees,
        passthrough,
    } = plan
    else {
        panic!("expected a Google plan, got {plan:?}");
    };
    let mut event = current.clone();
    event.fields.attendees.clone_from(attendees);
    event.passthrough.clone_from(passthrough);
    GoogleSeam::new(None).perform(
        google,
        &Request::Update {
            resource: resource.clone(),
            if_match: if_match.clone(),
            event,
        },
    )
}

#[test]
fn google_patches_only_the_own_entry_under_if_match() {
    let mut google = FakeGoogle::new(FaultPlan::none());
    google.remote_put("review", GoogleSeam::body(&meeting()));
    let seam = GoogleSeam::new(None);
    let resource = ResourceId::new("review");
    let current = read(seam.perform(
        &mut google,
        &Request::Read {
            resource: resource.clone(),
        },
    ));

    let plan = rsvp::plan(
        &Rsvp::new(BOB, Reply::Tentative),
        &current,
        &RsvpTarget::Google,
    )
    .unwrap();
    let RsvpPlan::Google {
        if_match,
        attendees,
        ..
    } = &plan
    else {
        panic!("expected a Google plan, got {plan:?}");
    };
    assert_eq!(if_match, &current.revision);
    // The whole array, each entry as the provider holds it, only Bob's reply changed.
    let mut expected = current.fields.attendees.clone();
    expected[1].participation = Participation::Tentative;
    assert_eq!(attendees, &expected);

    assert!(matches!(
        send_google(&mut google, &current, &plan),
        Outcome::Written(_)
    ));
    let now = read(seam.perform(&mut google, &Request::Read { resource }));
    let mut wanted = meeting();
    wanted.attendees[1].participation = Participation::Tentative;
    assert_eq!(now.fields, wanted);
    assert_eq!(
        rsvp::plan(&Rsvp::new(BOB, Reply::Tentative), &now, &RsvpTarget::Google),
        Ok(RsvpPlan::Done)
    );
}

#[test]
fn google_a_repeated_reply_fails_its_precondition_and_replans_to_done() {
    // Call 0 reads; call 1 sends the patch, which takes effect but loses its response.
    let mut google = FakeGoogle::new(FaultPlan::none().at_call(1, Fault::LostResponse));
    google.remote_put("review", GoogleSeam::body(&meeting()));
    let seam = GoogleSeam::new(None);
    let resource = ResourceId::new("review");
    let current = read(seam.perform(
        &mut google,
        &Request::Read {
            resource: resource.clone(),
        },
    ));
    let reply = Rsvp::new(BOB, Reply::Accept);
    let plan = rsvp::plan(&reply, &current, &RsvpTarget::Google).unwrap();
    assert_eq!(plan.duplicate_risk(), DuplicateRisk::None);

    assert_eq!(
        send_google(&mut google, &current, &plan),
        Outcome::Failed(Failure::Transient)
    );
    // The same plan sent again, by this device or by another that read before the reply landed,
    // is refused: the reply changed the etag.
    assert_eq!(
        send_google(&mut google, &current, &plan),
        Outcome::Failed(Failure::PreconditionFailed)
    );
    let now = read(seam.perform(&mut google, &Request::Read { resource }));
    assert_eq!(participation(&now.fields, BOB), Participation::Accepted);
    assert_eq!(
        rsvp::plan(&reply, &now, &RsvpTarget::Google),
        Ok(RsvpPlan::Done)
    );
}

#[test]
fn google_without_an_etag_writes_unconditionally_and_says_so() {
    let current = ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "review"),
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields: meeting(),
        passthrough: Passthrough::new(),
    };
    let plan = rsvp::plan(
        &Rsvp::new(BOB, Reply::Accept),
        &current,
        &RsvpTarget::Google,
    )
    .unwrap();
    assert!(matches!(plan, RsvpPlan::Google { if_match: None, .. }));
    assert_eq!(plan.duplicate_risk(), DuplicateRisk::SecondReply);
}

#[test]
fn google_carries_every_attendee_field_the_model_does_not_hold() {
    let mut kept = Passthrough::new();
    kept.push("colorId", "\"7\"");
    kept.push("attendees[carol@example.com].comment", "\"Running late\"");
    kept.push("organizer.self", "false");
    kept.push("attendees[alice@example.com].additionalGuests", "2");
    let current = ProviderEvent {
        ids: ProviderIds::new(Provider::Google, "review"),
        revision: Some(ProviderRevision::new("\"3\"")),
        mapper_version: MAPPER_VERSION,
        fields: meeting(),
        passthrough: kept,
    };
    let plan = rsvp::plan(
        &Rsvp::new(BOB, Reply::Accept),
        &current,
        &RsvpTarget::Google,
    )
    .unwrap();
    let RsvpPlan::Google { passthrough, .. } = &plan else {
        panic!("expected a Google plan, got {plan:?}");
    };
    // Other attendees' fields go with the whole array, so the patch erases none of them; fields
    // outside the array are not part of the patch.
    let names: Vec<(&str, &str)> = passthrough
        .iter()
        .map(|field| (field.name.as_str(), field.raw.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("attendees[carol@example.com].comment", "\"Running late\""),
            ("attendees[alice@example.com].additionalGuests", "2"),
        ]
    );
}

// Graph

#[test]
fn graph_replies_through_its_action_and_a_repeat_can_send_a_second_reply() {
    let mut graph = FakeGraph::new(FaultPlan::none());
    let id = graph.remote_create(GraphSeam::body(&meeting()));
    let seam = GraphSeam::new(None);
    let resource = ResourceId::new(id.clone());
    let before = read(seam.perform(
        &mut graph,
        &Request::Read {
            resource: resource.clone(),
        },
    ));

    for (reply, action) in [
        (Reply::Accept, GraphAction::Accept),
        (Reply::Decline, GraphAction::Decline),
        (Reply::Tentative, GraphAction::TentativelyAccept),
    ] {
        let plan = rsvp::plan(&Rsvp::new(BOB, reply), &before, &RsvpTarget::Graph).unwrap();
        assert_eq!(
            plan,
            RsvpPlan::Graph {
                resource: resource.clone(),
                action
            }
        );
        // No precondition travels with a Graph reply.
        assert_eq!(plan.duplicate_risk(), DuplicateRisk::SecondReply);
    }
    assert_eq!(GraphAction::TentativelyAccept.path(), "tentativelyAccept");

    // The accept takes effect, and its response is lost. The fake has no reply action, so the
    // effect is applied as the server applies it: Bob's own response in the event.
    let reply = Rsvp::new(BOB, Reply::Accept);
    let first = rsvp::plan(&reply, &before, &RsvpTarget::Graph).unwrap();
    let mut replied = meeting();
    replied.attendees[1].participation = Participation::Accepted;
    graph.remote_update(&id, GraphSeam::body(&replied));

    // A device that planned from the read before the reply landed holds the same plan, and
    // nothing refuses it: sending it is a second reply to the organizer.
    let repeat = rsvp::plan(&reply, &before, &RsvpTarget::Graph).unwrap();
    assert_eq!(repeat, first);
    assert_eq!(repeat.duplicate_risk(), DuplicateRisk::SecondReply);

    // Planned from a read after the reply landed, nothing is sent.
    let after = read(seam.perform(&mut graph, &Request::Read { resource }));
    assert_eq!(
        rsvp::plan(&reply, &after, &RsvpTarget::Graph),
        Ok(RsvpPlan::Done)
    );
}

// CalDAV

const HREF: &str = "/cal/bob/review.ics";

/// Bob's copy of Alice's meeting, with parameters the model does not map.
const INVITE: &str = concat!(
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Example Corp//Calendar//EN\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:review@example.com\r\n",
    "DTSTAMP:20260101T000000Z\r\n",
    "SEQUENCE:1\r\n",
    "DTSTART:20260302T150000Z\r\n",
    "DTEND:20260302T160000Z\r\n",
    "SUMMARY:Review\r\n",
    "ORGANIZER;CN=Alice:mailto:alice@example.com\r\n",
    "ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n",
    "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com\r\n",
    "ATTENDEE;ROLE=OPT-PARTICIPANT;X-SEAT=12:mailto:carol@example.com\r\n",
    "END:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

fn caldav_read(member: &caldav::Member) -> ProviderEvent {
    let read = IcalDocument::parse(member.body.as_bytes()).read(&Source::caldav(
        HREF,
        Some(ProviderRevision::new(member.etag.clone())),
    ));
    read.series[0].series.master.clone().unwrap()
}

fn target(schedule_tag: &str) -> RsvpTarget {
    RsvpTarget::CalDav {
        auto_schedule: true,
        schedule_tag: Some(ScheduleTag::new(schedule_tag)),
        instance: None,
    }
}

/// Performs a `CalDAV` plan: the reply applied to the object's bytes, sent as a `PUT`.
///
/// The fake models no `Schedule-Tag`, so the `PUT` is conditional on the `ETag` it was planned
/// from. On a scheduling server the attendee's own `PUT` changes both (RFC 6638 section 3.2.10),
/// so a repeat fails either precondition alike.
fn send_caldav(
    fake: &mut FakeCalDav,
    member: &caldav::Member,
    plan: &RsvpPlan,
) -> Result<String, caldav::Error> {
    let RsvpPlan::CalDav {
        resource,
        uid,
        instance,
        address,
        participation,
        ..
    } = plan
    else {
        panic!("expected a CalDAV plan, got {plan:?}");
    };
    let mut document = IcalDocument::parse(member.body.as_bytes());
    assert_eq!(
        document.reply(uid, instance.as_ref(), address, *participation),
        Ok(true)
    );
    let body = String::from_utf8(document.to_bytes()).unwrap();
    fake.put(
        resource.as_str(),
        &Precondition::IfMatch(member.etag.clone()),
        body,
    )
}

#[test]
fn caldav_puts_only_the_own_partstat_under_the_schedule_tag() {
    let mut fake = FakeCalDav::new(FaultPlan::none());
    fake.remote_put(HREF, INVITE.to_owned());
    let member = fake.get(HREF).unwrap();
    let current = caldav_read(&member);

    let plan = rsvp::plan(&Rsvp::new(BOB, Reply::Decline), &current, &target("\"s1\"")).unwrap();
    assert_eq!(
        plan,
        RsvpPlan::CalDav {
            resource: ResourceId::new(HREF),
            if_schedule_tag_match: ScheduleTag::new("\"s1\""),
            uid: "review@example.com".to_owned(),
            instance: None,
            address: BOB.to_owned(),
            participation: Participation::Declined,
        }
    );
    assert_eq!(plan.duplicate_risk(), DuplicateRisk::None);

    send_caldav(&mut fake, &member, &plan).unwrap();
    let stored = fake.get(HREF).unwrap();
    assert_eq!(
        stored.body,
        INVITE.replace(
            "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@",
            "ATTENDEE;PARTSTAT=DECLINED;RSVP=TRUE:mailto:bob@",
        )
    );
    assert_eq!(
        rsvp::plan(
            &Rsvp::new(BOB, Reply::Decline),
            &caldav_read(&stored),
            &target("\"s2\"")
        ),
        Ok(RsvpPlan::Done)
    );
}

#[test]
fn caldav_a_repeated_reply_fails_its_precondition_and_replans_to_done() {
    // Call 0 reads; call 1 sends the PUT, which takes effect but loses its response.
    let mut fake = FakeCalDav::new(FaultPlan::none().at_call(1, Fault::LostResponse));
    fake.remote_put(HREF, INVITE.to_owned());
    let member = fake.get(HREF).unwrap();
    let reply = Rsvp::new(BOB, Reply::Accept);
    let plan = rsvp::plan(&reply, &caldav_read(&member), &target("\"s1\"")).unwrap();

    assert_eq!(
        send_caldav(&mut fake, &member, &plan),
        Err(caldav::Error::LostResponse)
    );
    assert_eq!(
        send_caldav(&mut fake, &member, &plan),
        Err(caldav::Error::PreconditionFailed)
    );
    let now = caldav_read(&fake.get(HREF).unwrap());
    assert_eq!(participation(&now.fields, BOB), Participation::Accepted);
    assert_eq!(
        rsvp::plan(&reply, &now, &target("\"s2\"")),
        Ok(RsvpPlan::Done)
    );
}

#[test]
fn caldav_without_scheduling_is_unsupported() {
    let mut fake = FakeCalDav::new(FaultPlan::none());
    fake.remote_put(HREF, INVITE.to_owned());
    let current = caldav_read(&fake.get(HREF).unwrap());
    let reply = Rsvp::new(BOB, Reply::Accept);

    let no_auto_schedule = RsvpTarget::CalDav {
        auto_schedule: false,
        schedule_tag: Some(ScheduleTag::new("\"s1\"")),
        instance: None,
    };
    assert_eq!(
        rsvp::plan(&reply, &current, &no_auto_schedule),
        Err(RsvpError::Unsupported(Unsupported::NoAutoSchedule))
    );
    let no_tag = RsvpTarget::CalDav {
        auto_schedule: true,
        schedule_tag: None,
        instance: None,
    };
    assert_eq!(
        rsvp::plan(&reply, &current, &no_tag),
        Err(RsvpError::Unsupported(Unsupported::NoScheduleTag))
    );
    let mut no_uid = current.clone();
    no_uid.ids.ical_uid = None;
    assert_eq!(
        rsvp::plan(&reply, &no_uid, &target("\"s1\"")),
        Err(RsvpError::Unsupported(Unsupported::NoUid))
    );
    // Unsupported even where the reply is already held: the server would not have sent it.
    let mut held = current;
    held.fields.attendees[1].participation = Participation::Accepted;
    assert_eq!(
        rsvp::plan(&reply, &held, &no_auto_schedule),
        Err(RsvpError::Unsupported(Unsupported::NoAutoSchedule))
    );
}
