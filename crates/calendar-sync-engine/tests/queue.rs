//! The write queue against the testkit's fake providers: an edit retried after a lost response
//! or a restart at any step applies once where the provider allows it, each provider's remaining
//! duplicate risk is asserted as it is, queued edits coalesce, and a create on a provider that
//! assigns ids carries the edits behind it to the id the provider gave.

use std::collections::VecDeque;

use calendar_sync_engine::{
    DerivedIds, Edit, EditId, Effect, Input, Notice, Operation, Outcome, Rejection, Request,
    RequestId, ResourceId, Role, SyncState,
};
use calendar_sync_model::{
    EventTimes, NormalizedEvent, Passthrough, Provider, ProviderEvent, ProviderIds, TimeValue,
};
use calendar_sync_testkit::caldav::FakeCalDav;
use calendar_sync_testkit::google::FakeGoogle;
use calendar_sync_testkit::graph::FakeGraph;
use calendar_sync_testkit::seam::{CalDavSeam, GoogleSeam, GraphSeam, MAPPER_VERSION};
use calendar_sync_testkit::{Fault, FaultPlan};
use jiff::Timestamp;
use serde_json::Value;

/// More requests than any edit here needs.
const MAX_REQUESTS: usize = 1_000;
/// The `CalDAV` collection the tests create in.
const COLLECTION: &str = "/cal/";

fn fields(title: &str) -> NormalizedEvent {
    let start = Timestamp::from_second(1_700_000_000).unwrap();
    let end = Timestamp::from_second(1_700_003_600).unwrap();
    let times = EventTimes::new(TimeValue::instant(start), Some(TimeValue::instant(end))).unwrap();
    let mut event = NormalizedEvent::new(times);
    event.title = Some(title.to_owned());
    event
}

/// A fake behind its seam.
trait Remote {
    const PROVIDER: Provider;

    fn new(plan: FaultPlan) -> Self;
    fn perform(&mut self, request: &Request) -> Outcome;
    /// The resource a create made by `edit` is queued under.
    fn create_resource(edit: &EditId) -> ResourceId;
    /// Another client creates an event, returning its id. Never faulted.
    fn remote_create(&mut self, fields: &NormalizedEvent) -> ResourceId;
    /// Another client deletes an event. Never faulted.
    fn remote_delete(&mut self, id: &ResourceId);
    /// The events that exist, in id order. Never faulted.
    fn events(&self) -> Vec<(String, NormalizedEvent)>;
    /// The number of calls served so far.
    fn calls(&self) -> u64;
}

struct Google {
    fake: FakeGoogle,
    seam: GoogleSeam,
    next: u32,
}

impl Remote for Google {
    const PROVIDER: Provider = Provider::Google;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeGoogle::new(plan),
            seam: GoogleSeam::new(None),
            next: 0,
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn create_resource(edit: &EditId) -> ResourceId {
        DerivedIds::of(edit).google_id()
    }

    fn remote_create(&mut self, fields: &NormalizedEvent) -> ResourceId {
        self.next += 1;
        let id = format!("remote{}", self.next);
        self.fake.remote_put(&id, GoogleSeam::body(fields));
        ResourceId::new(id)
    }

    fn remote_delete(&mut self, id: &ResourceId) {
        self.fake.remote_delete(id.as_str());
    }

    fn events(&self) -> Vec<(String, NormalizedEvent)> {
        self.fake
            .events()
            .into_iter()
            .map(|event| {
                let body = event.body.unwrap_or(Value::Null);
                (event.id, serde_json::from_value(body).unwrap())
            })
            .collect()
    }

    fn calls(&self) -> u64 {
        self.fake.calls()
    }
}

struct Graph {
    fake: FakeGraph,
    seam: GraphSeam,
}

impl Graph {
    /// A Graph that forgets each `transactionId` `calls` calls after it was used.
    fn forgetting(plan: FaultPlan, calls: u64) -> Self {
        Self {
            fake: FakeGraph::new(plan).with_transaction_lifetime(calls),
            seam: GraphSeam::new(None),
        }
    }
}

impl Remote for Graph {
    const PROVIDER: Provider = Provider::Graph;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeGraph::new(plan),
            seam: GraphSeam::new(None),
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn create_resource(edit: &EditId) -> ResourceId {
        DerivedIds::of(edit).idempotency_key()
    }

    fn remote_create(&mut self, fields: &NormalizedEvent) -> ResourceId {
        ResourceId::new(self.fake.remote_create(GraphSeam::body(fields)))
    }

    fn remote_delete(&mut self, id: &ResourceId) {
        self.fake.remote_delete(id.as_str());
    }

    fn events(&self) -> Vec<(String, NormalizedEvent)> {
        self.fake
            .events()
            .into_iter()
            .map(|event| {
                let body = event.body["event"].clone();
                (event.id, serde_json::from_value(body).unwrap())
            })
            .collect()
    }

    fn calls(&self) -> u64 {
        self.fake.calls()
    }
}

struct CalDav {
    fake: FakeCalDav,
    seam: CalDavSeam,
    next: u32,
}

impl Remote for CalDav {
    const PROVIDER: Provider = Provider::CalDav;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeCalDav::new(plan),
            seam: CalDavSeam::new(None),
            next: 0,
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn create_resource(edit: &EditId) -> ResourceId {
        DerivedIds::of(edit).caldav_href(COLLECTION)
    }

    fn remote_create(&mut self, fields: &NormalizedEvent) -> ResourceId {
        self.next += 1;
        let href = format!("{COLLECTION}remote{}.ics", self.next);
        self.fake.remote_put(&href, CalDavSeam::body(fields));
        ResourceId::new(href)
    }

    fn remote_delete(&mut self, id: &ResourceId) {
        self.fake.remote_delete(id.as_str());
    }

    fn events(&self) -> Vec<(String, NormalizedEvent)> {
        self.fake
            .members()
            .into_iter()
            .map(|member| (member.href, serde_json::from_str(&member.body).unwrap()))
            .collect()
    }

    fn calls(&self) -> u64 {
        self.fake.calls()
    }
}

/// One device: an engine state, the requests it has queued, and the notices it was given.
#[derive(Default)]
struct Device {
    state: SyncState,
    queue: VecDeque<(RequestId, Request)>,
    notices: Vec<Notice>,
}

impl Device {
    fn feed(&mut self, input: Input) {
        for effect in self.state.apply(input) {
            match effect {
                Effect::Request { id, request } => self.queue.push_back((id, request)),
                Effect::Notify(notice) => self.notices.push(notice),
            }
        }
    }

    /// Performs the next queued request, and delivers its outcome.
    fn step<R: Remote>(&mut self, remote: &mut R) -> bool {
        let Some((request, call)) = self.queue.pop_front() else {
            return false;
        };
        let outcome = remote.perform(&call);
        self.feed(Input::Outcome { request, outcome });
        true
    }

    /// Performs every request until none is left.
    fn settle<R: Remote>(&mut self, remote: &mut R) {
        for _ in 0..MAX_REQUESTS {
            if !self.step(remote) {
                assert!(self.state.is_quiet(), "the engine is still busy");
                return;
            }
        }
        panic!("the engine did not settle");
    }

    /// The application stops after sending the next request, so its response is lost along with
    /// everything queued; it reloads the state and resumes.
    fn restart_after_sending<R: Remote>(&mut self, remote: &mut R) {
        if let Some((_, request)) = self.queue.front() {
            let _ = remote.perform(&request.clone());
        }
        self.queue.clear();
        let json = serde_json::to_string(&self.state).unwrap();
        self.state = serde_json::from_str(&json).unwrap();
        self.feed(Input::Resume);
    }

    /// Syncs once, to completion.
    fn sync<R: Remote>(&mut self, remote: &mut R) {
        self.feed(Input::Sync);
        self.settle(remote);
    }

    /// How each edit ended: the first ending notice for it, and how many it got.
    fn endings(&self, edit: &EditId) -> Vec<&Notice> {
        self.notices
            .iter()
            .filter(|notice| match notice {
                Notice::Applied { edit: id, .. }
                | Notice::Rejected { edit: id, .. }
                | Notice::Expired { edit: id } => id == edit,
                _ => false,
            })
            .collect()
    }

    fn applied_once(&self, edit: &EditId) {
        let endings = self.endings(edit);
        assert!(
            matches!(&endings[..], [Notice::Applied { .. }]),
            "{edit:?} ended {endings:?}"
        );
    }
}

fn edit(id: &str, resource: ResourceId, operation: Operation) -> Edit {
    Edit {
        id: EditId::new(id),
        resource,
        operation,
        role: Role::Owner,
        expires_at: None,
    }
}

fn create<R: Remote>(id: &str, title: &str) -> Edit {
    let edit_id = EditId::new(id);
    let resource = R::create_resource(&edit_id);
    let event = ProviderEvent {
        ids: ProviderIds::new(R::PROVIDER, resource.as_str()),
        revision: None,
        mapper_version: MAPPER_VERSION,
        fields: fields(title),
        passthrough: Passthrough::new(),
    };
    edit(id, resource, Operation::Create(event))
}

fn update(id: &str, resource: &ResourceId, from: &str, to: &str) -> Edit {
    let operation = Operation::Update {
        base: fields(from),
        ours: fields(to),
    };
    edit(id, resource.clone(), operation)
}

fn titles<R: Remote>(remote: &R) -> Vec<String> {
    remote
        .events()
        .into_iter()
        .map(|(_, fields)| fields.title.unwrap_or_default())
        .collect()
}

// A retry after any step applies once.

/// What a scenario starts from and pushes.
#[derive(Clone, Copy, Debug)]
enum Push {
    Create,
    Update,
    Delete,
}

/// How the scenario breaks at call `at`.
#[derive(Clone, Copy, Debug)]
enum Break {
    /// The call's response is lost.
    LoseResponse,
    /// The application stops after sending the call, losing its response, and resumes.
    Restart,
}

/// Seeds the provider, syncs, and pushes one edit; returns the device and the edit's id.
fn push<R: Remote>(remote: &mut R, push: Push, restart_at: Option<u64>) -> (Device, EditId) {
    let mut device = Device::default();
    let existing = match push {
        Push::Create => None,
        Push::Update | Push::Delete => Some(remote.remote_create(&fields("before"))),
    };
    device.sync(remote);
    let start = remote.calls();
    let pushed = match (push, existing) {
        (Push::Create, _) => create::<R>("edit1", "created"),
        (Push::Update, Some(id)) => update("edit1", &id, "before", "after"),
        (Push::Delete, Some(id)) => edit("edit1", id, Operation::Delete),
        _ => unreachable!(),
    };
    let id = pushed.id.clone();
    device.feed(Input::Edit(pushed));
    for _ in 0..MAX_REQUESTS {
        if restart_at == Some(remote.calls() - start) && !device.queue.is_empty() {
            device.restart_after_sending(remote);
        } else if !device.step(remote) {
            break;
        }
    }
    device.sync(remote);
    (device, id)
}

fn expected(push: Push) -> Vec<String> {
    match push {
        Push::Create => vec!["created".to_owned()],
        Push::Update => vec!["after".to_owned()],
        Push::Delete => Vec::new(),
    }
}

/// Breaks the push at every call it makes, one at a time, and checks the edit applies once.
fn applies_once<R: Remote>(make: &impl Fn(FaultPlan) -> R, push_kind: Push, how: Break) {
    // The calls the push makes when nothing fails.
    let mut clean = make(FaultPlan::none());
    push(&mut clean, push_kind, None);
    let calls = clean.calls();
    assert_eq!(titles(&clean), expected(push_kind));

    for at in 0..calls {
        let (mut remote, restart_at) = match how {
            Break::LoseResponse => (
                make(FaultPlan::none().at_call(at, Fault::LostResponse)),
                None,
            ),
            Break::Restart => (make(FaultPlan::none()), Some(at)),
        };
        let (device, id) = push(&mut remote, push_kind, restart_at);
        let context = format!("{:?} {push_kind:?} {how:?} at call {at}", R::PROVIDER);
        assert_eq!(titles(&remote), expected(push_kind), "{context}");
        device.applied_once(&id);
        let mirrored: Vec<String> = device
            .state
            .records()
            .values()
            .map(|record| record.fields.title.clone().unwrap_or_default())
            .collect();
        assert_eq!(mirrored, expected(push_kind), "{context}");
    }
}

fn applies_once_everywhere<R: Remote>(make: impl Fn(FaultPlan) -> R) {
    for push_kind in [Push::Create, Push::Update, Push::Delete] {
        for how in [Break::LoseResponse, Break::Restart] {
            applies_once(&make, push_kind, how);
        }
    }
}

#[test]
fn google_applies_each_edit_once_whatever_step_is_retried() {
    applies_once_everywhere(Google::new);
}

#[test]
fn graph_applies_each_edit_once_whatever_step_is_retried() {
    applies_once_everywhere(Graph::new);
}

#[test]
fn graph_applies_each_edit_once_when_the_transaction_id_is_already_forgotten() {
    // The lookup before each create, not the `transactionId`, keeps a retry from duplicating.
    applies_once_everywhere(|plan| Graph::forgetting(plan, 0));
}

#[test]
fn caldav_applies_each_edit_once_whatever_step_is_retried() {
    applies_once_everywhere(CalDav::new);
}

// Each provider's remaining duplicate risk, asserted as it is.

/// Another device pushes the same create after the first applied it.
fn second_device_pushes_again<R: Remote>(remote: &mut R) -> Device {
    let mut device = Device::default();
    device.feed(Input::Edit(create::<R>("edit1", "created")));
    device.settle(remote);
    device
}

#[test]
fn a_create_repeated_by_another_device_makes_one_event_on_every_provider() {
    fn check<R: Remote>(mut remote: R) {
        let mut first = Device::default();
        first.feed(Input::Edit(create::<R>("edit1", "created")));
        first.settle(&mut remote);
        first.applied_once(&EditId::new("edit1"));
        let second = second_device_pushes_again(&mut remote);
        second.applied_once(&EditId::new("edit1"));
        assert_eq!(titles(&remote), ["created"], "{:?}", R::PROVIDER);
    }
    check(Google::new(FaultPlan::none()));
    check(Graph::new(FaultPlan::none()));
    check(CalDav::new(FaultPlan::none()));
}

#[test]
fn a_create_repeated_after_the_event_was_deleted() {
    fn after_delete<R: Remote>(mut remote: R) -> (Device, Vec<String>) {
        let mut first = Device::default();
        first.feed(Input::Edit(create::<R>("edit1", "created")));
        first.settle(&mut remote);
        let (id, _) = remote.events().pop().unwrap();
        remote.remote_delete(&ResourceId::new(id));
        let second = second_device_pushes_again(&mut remote);
        let titles = titles(&remote);
        (second, titles)
    }

    // Google keeps a deleted event's id, so the repeat is refused and nothing comes back.
    let (device, titles) = after_delete(Google::new(FaultPlan::none()));
    assert!(titles.is_empty());
    let endings = device.endings(&EditId::new("edit1"));
    assert!(matches!(
        &endings[..],
        [Notice::Rejected {
            reason: Rejection::Exists,
            ..
        }]
    ));

    // A deleted `CalDAV` href is free again, so `If-None-Match: *` lets the repeat bring the
    // event back.
    let (device, titles) = after_delete(CalDav::new(FaultPlan::none()));
    assert_eq!(titles, ["created"]);
    device.applied_once(&EditId::new("edit1"));

    // Graph finds nothing under the key once the event is gone, and the fake does not return a
    // deleted event for a remembered `transactionId`: the repeat makes a new event.
    let (device, titles) = after_delete(Graph::new(FaultPlan::none()));
    assert_eq!(titles, ["created"]);
    device.applied_once(&EditId::new("edit1"));
}

/// Two devices push the same create at once: both look it up before either creates it.
fn concurrent_graph_creates(mut remote: Graph) -> Vec<String> {
    let mut a = Device::default();
    let mut b = Device::default();
    a.feed(Input::Edit(create::<Graph>("edit1", "created")));
    b.feed(Input::Edit(create::<Graph>("edit1", "created")));
    // Both lookups find nothing, so both send a create.
    assert!(matches!(a.queue.front(), Some((_, Request::Lookup { .. }))));
    a.step(&mut remote);
    b.step(&mut remote);
    assert!(matches!(a.queue.front(), Some((_, Request::Create { .. }))));
    assert!(matches!(b.queue.front(), Some((_, Request::Create { .. }))));
    a.settle(&mut remote);
    b.settle(&mut remote);
    a.applied_once(&EditId::new("edit1"));
    b.applied_once(&EditId::new("edit1"));
    titles(&remote)
}

#[test]
fn graph_dedupes_concurrent_creates_only_while_it_remembers_the_transaction_id() {
    // Remembered: the second create returns the first one's event.
    assert_eq!(
        concurrent_graph_creates(Graph::new(FaultPlan::none())),
        ["created"]
    );
    // Forgotten by the time the second create arrives: a duplicate, the residual risk.
    assert_eq!(
        concurrent_graph_creates(Graph::forgetting(FaultPlan::none(), 0)),
        ["created", "created"]
    );
}

#[test]
fn concurrent_creates_with_a_client_chosen_id_make_one_event() {
    fn check<R: Remote>(mut remote: R) {
        let mut a = Device::default();
        let mut b = Device::default();
        a.feed(Input::Edit(create::<R>("edit1", "created")));
        b.feed(Input::Edit(create::<R>("edit1", "created")));
        a.step(&mut remote);
        b.step(&mut remote);
        a.settle(&mut remote);
        b.settle(&mut remote);
        a.applied_once(&EditId::new("edit1"));
        b.applied_once(&EditId::new("edit1"));
        assert_eq!(titles(&remote), ["created"], "{:?}", R::PROVIDER);
    }
    check(Google::new(FaultPlan::none()));
    check(CalDav::new(FaultPlan::none()));
}

// Remapping.

#[test]
fn edits_behind_a_graph_create_follow_it_to_the_id_graph_assigns() {
    let mut remote = Graph::new(FaultPlan::none());
    let mut device = Device::default();
    let created = create::<Graph>("edit1", "created");
    let key = created.resource.clone();
    device.feed(Input::Edit(created));
    // Queued behind the create, which has started, so it is not folded into it.
    device.feed(Input::Edit(update("edit2", &key, "created", "renamed")));
    assert_eq!(device.state.pending_edits().count(), 2);
    device.settle(&mut remote);

    let (id, _) = remote.events().pop().unwrap();
    let id = ResourceId::new(id);
    assert_ne!(id, key);
    assert!(device.notices.contains(&Notice::Remapped {
        from: key.clone(),
        to: id.clone(),
    }));
    device.applied_once(&EditId::new("edit1"));
    device.applied_once(&EditId::new("edit2"));
    assert_eq!(titles(&remote), ["renamed"]);

    // An edit made later against the key reaches the event too.
    device.feed(Input::Edit(update("edit3", &key, "renamed", "again")));
    assert!(matches!(
        device.queue.front(),
        Some((_, Request::Read { resource })) if *resource == id
    ));
    device.settle(&mut remote);
    device.applied_once(&EditId::new("edit3"));
    assert_eq!(titles(&remote), ["again"]);
    assert_eq!(device.state.records().keys().collect::<Vec<_>>(), [&id]);
}

#[test]
fn a_create_with_a_client_chosen_id_is_not_remapped() {
    fn check<R: Remote>(mut remote: R) {
        let mut device = Device::default();
        device.feed(Input::Edit(create::<R>("edit1", "created")));
        device.settle(&mut remote);
        assert!(
            !device
                .notices
                .iter()
                .any(|notice| matches!(notice, Notice::Remapped { .. }))
        );
        let resource = R::create_resource(&EditId::new("edit1"));
        assert!(device.state.records().contains_key(&resource));
    }
    check(Google::new(FaultPlan::none()));
    check(CalDav::new(FaultPlan::none()));
}

// Coalescing.

/// A device holding one synced event, with an update to it in flight so later edits wait.
fn busy<R: Remote>(remote: &mut R) -> (Device, ResourceId) {
    let id = remote.remote_create(&fields("a"));
    let mut device = Device::default();
    device.sync(remote);
    device.feed(Input::Edit(update("head", &id, "a", "b")));
    assert_eq!(device.queue.len(), 1);
    (device, id)
}

fn writes(device: &Device) -> usize {
    device
        .queue
        .iter()
        .filter(|(_, request)| {
            matches!(
                request,
                Request::Create { .. } | Request::Update { .. } | Request::Delete { .. }
            )
        })
        .count()
}

#[test]
fn a_run_of_updates_waiting_behind_one_in_flight_is_pushed_as_one() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    device.feed(Input::Edit(update("e1", &id, "b", "c")));
    device.feed(Input::Edit(update("e2", &id, "c", "d")));
    device.feed(Input::Edit(update("e3", &id, "d", "e")));
    assert_eq!(device.state.pending_edits().count(), 2);
    let before = remote.calls();
    device.settle(&mut remote);
    // The head reads and writes; the folded edit reads and writes once.
    assert_eq!(remote.calls() - before, 4);
    for edit in ["head", "e1", "e2", "e3"] {
        device.applied_once(&EditId::new(edit));
    }
    assert_eq!(titles(&remote), ["e"]);
}

#[test]
fn an_update_whose_base_is_not_the_waiting_result_stays_separate() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    device.feed(Input::Edit(update("e1", &id, "b", "c")));
    device.feed(Input::Edit(update("e2", &id, "x", "y")));
    assert_eq!(device.state.pending_edits().count(), 3);
    device.settle(&mut remote);
    for edit in ["head", "e1", "e2"] {
        device.applied_once(&EditId::new(edit));
    }
}

#[test]
fn a_delete_absorbs_the_update_waiting_before_it() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    device.feed(Input::Edit(update("e1", &id, "b", "c")));
    device.feed(Input::Edit(edit("e2", id.clone(), Operation::Delete)));
    let pending: Vec<&Operation> = device
        .state
        .pending_edits()
        .map(|edit| &edit.operation)
        .collect();
    assert_eq!(pending[1], &Operation::Delete);
    device.settle(&mut remote);
    for edit in ["head", "e1", "e2"] {
        device.applied_once(&EditId::new(edit));
    }
    assert!(titles(&remote).is_empty());
}

#[test]
fn an_attendee_update_is_not_folded_into_its_delete() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    let attendee = Role::Attendee {
        address: "me@example.com".to_owned(),
    };
    let mut first = update("e1", &id, "b", "c");
    first.role = attendee.clone();
    let mut second = edit("e2", id, Operation::Delete);
    second.role = attendee;
    device.feed(Input::Edit(first));
    device.feed(Input::Edit(second));
    assert_eq!(device.state.pending_edits().count(), 3);
    device.settle(&mut remote);
    let endings = device.endings(&EditId::new("e2"));
    assert!(matches!(
        &endings[..],
        [Notice::Rejected {
            reason: Rejection::NotOwner,
            ..
        }]
    ));
}

#[test]
fn a_create_and_an_update_waiting_together_create_the_updated_event() {
    let mut remote = Google::new(FaultPlan::none());
    let mut device = Device::default();
    let created = create::<Google>("c1", "a");
    let id = created.resource.clone();
    // A delete of the same id goes first, so the create waits.
    device.feed(Input::Edit(edit("d0", id.clone(), Operation::Delete)));
    device.feed(Input::Edit(created));
    device.feed(Input::Edit(update("u1", &id, "a", "b")));
    assert_eq!(device.state.pending_edits().count(), 2);
    device.settle(&mut remote);
    for edit in ["d0", "c1", "u1"] {
        device.applied_once(&EditId::new(edit));
    }
    assert_eq!(titles(&remote), ["b"]);
}

#[test]
fn a_create_and_a_delete_waiting_together_send_nothing() {
    let mut remote = Google::new(FaultPlan::none());
    let mut device = Device::default();
    let created = create::<Google>("c1", "a");
    let id = created.resource.clone();
    device.feed(Input::Edit(edit("d0", id.clone(), Operation::Delete)));
    device.feed(Input::Edit(created));
    device.feed(Input::Edit(edit("d1", id, Operation::Delete)));
    // Both end at once, and only the first delete is left.
    device.applied_once(&EditId::new("c1"));
    device.applied_once(&EditId::new("d1"));
    assert_eq!(device.state.pending_edits().count(), 1);
    device.settle(&mut remote);
    device.applied_once(&EditId::new("d0"));
    assert!(titles(&remote).is_empty());
    assert_eq!(writes(&device), 0);
}

#[test]
fn a_folded_edit_expires_with_the_newer_edit() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    let at = |second| Some(Timestamp::from_second(second).unwrap());
    let mut first = update("e1", &id, "b", "c");
    first.expires_at = at(10);
    let mut second = update("e2", &id, "c", "d");
    second.expires_at = at(20);
    device.feed(Input::Edit(first));
    device.feed(Input::Edit(second));
    let expiry: Vec<_> = device
        .state
        .pending_edits()
        .map(|edit| edit.expires_at)
        .collect();
    assert_eq!(expiry, [None, at(20)]);
    device.feed(Input::Tick {
        now: Timestamp::from_second(15).unwrap(),
    });
    assert_eq!(device.state.pending_edits().count(), 2);
    device.feed(Input::Tick {
        now: Timestamp::from_second(20).unwrap(),
    });
    for edit in ["e1", "e2"] {
        let endings = device.endings(&EditId::new(edit));
        assert!(matches!(&endings[..], [Notice::Expired { .. }]), "{edit}");
    }
    device.settle(&mut remote);
    device.applied_once(&EditId::new("head"));
    assert_eq!(titles(&remote), ["b"]);
}

#[test]
fn a_folded_queue_survives_a_restart() {
    let mut remote = Google::new(FaultPlan::none());
    let (mut device, id) = busy(&mut remote);
    device.feed(Input::Edit(update("e1", &id, "b", "c")));
    device.feed(Input::Edit(update("e2", &id, "c", "d")));
    device.restart_after_sending(&mut remote);
    device.settle(&mut remote);
    for edit in ["head", "e1", "e2"] {
        device.applied_once(&EditId::new(edit));
    }
    assert_eq!(titles(&remote), ["d"]);
}
