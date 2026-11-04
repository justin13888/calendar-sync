//! The sync state machine driven against the testkit's fake providers: random local and remote
//! operations, injected faults and restarts converge to the provider's state, a truncated or
//! failed listing never deletes, and the state resumes after any step.

use std::collections::{BTreeMap, VecDeque};

use calendar_sync_engine::{
    Edit, EditId, Effect, Input, ListFrom, Next, Notice, Operation, Outcome, Page, Request,
    RequestId, ResourceId, Role, SyncState,
};
use calendar_sync_model::{
    EventTimes, NormalizedEvent, Passthrough, Provider, ProviderEvent, ProviderIds, TimeValue,
};
use calendar_sync_testkit::caldav::FakeCalDav;
use calendar_sync_testkit::google::FakeGoogle;
use calendar_sync_testkit::graph::FakeGraph;
use calendar_sync_testkit::seam::{CalDavSeam, GoogleSeam, GraphSeam, MAPPER_VERSION};
use calendar_sync_testkit::{Fault, FaultPlan, InjectedFault};
use jiff::Timestamp;
use proptest::prelude::*;
use serde_json::Value;

/// More requests than any run needs at the rates these tests use.
const MAX_REQUESTS: usize = 20_000;
const PAGE: usize = 2;
/// How many ticks an edit lives.
const LIFETIME: i64 = 6;

/// What a provider holds, or what the engine mirrors: id to revision and fields.
type Snapshot = BTreeMap<String, (String, NormalizedEvent)>;

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
    /// Whether a create the engine repeats after a lost response is idempotent.
    const IDEMPOTENT_CREATE: bool;

    fn new(plan: FaultPlan) -> Self;
    fn perform(&mut self, request: &Request) -> Outcome;
    /// The resource id for slot `n`, if it has one.
    fn key(&self, n: u8) -> Option<ResourceId>;
    /// Another client creates or replaces slot `n`.
    fn remote_put(&mut self, n: u8, fields: &NormalizedEvent);
    /// Another client deletes slot `n`.
    fn remote_delete(&mut self, n: u8);
    fn expire_tokens(&mut self);
    fn snapshot(&self) -> Snapshot;
    fn faults(&self) -> Vec<InjectedFault>;
}

struct Google {
    fake: FakeGoogle,
    seam: GoogleSeam,
}

impl Remote for Google {
    const PROVIDER: Provider = Provider::Google;
    const IDEMPOTENT_CREATE: bool = true;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeGoogle::new(plan),
            seam: GoogleSeam::new(Some(PAGE)),
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn key(&self, n: u8) -> Option<ResourceId> {
        Some(ResourceId::new(format!("e{n}")))
    }

    fn remote_put(&mut self, n: u8, fields: &NormalizedEvent) {
        self.fake
            .remote_put(&format!("e{n}"), GoogleSeam::body(fields));
    }

    fn remote_delete(&mut self, n: u8) {
        self.fake.remote_delete(&format!("e{n}"));
    }

    fn expire_tokens(&mut self) {
        self.fake.expire_tokens();
    }

    fn snapshot(&self) -> Snapshot {
        self.fake
            .events()
            .into_iter()
            .map(|event| {
                let body = event.body.unwrap_or(Value::Null);
                (
                    event.id,
                    (event.etag, serde_json::from_value(body).unwrap()),
                )
            })
            .collect()
    }

    fn faults(&self) -> Vec<InjectedFault> {
        self.fake.faults().to_vec()
    }
}

struct Graph {
    fake: FakeGraph,
    seam: GraphSeam,
    /// Graph assigns ids, so slot `n` names whichever id was created for it last.
    slots: BTreeMap<u8, String>,
}

impl Remote for Graph {
    const PROVIDER: Provider = Provider::Graph;
    const IDEMPOTENT_CREATE: bool = false;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeGraph::new(plan),
            seam: GraphSeam::new(Some(PAGE)),
            slots: BTreeMap::new(),
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn key(&self, n: u8) -> Option<ResourceId> {
        self.slots.get(&n).cloned().map(ResourceId::new)
    }

    fn remote_put(&mut self, n: u8, fields: &NormalizedEvent) {
        let body = GraphSeam::body(fields);
        match self.slots.get(&n) {
            Some(id) if self.fake.events().iter().any(|event| &event.id == id) => {
                self.fake.remote_update(id, body);
            }
            _ => {
                self.slots.insert(n, self.fake.remote_create(body));
            }
        }
    }

    fn remote_delete(&mut self, n: u8) {
        if let Some(id) = self.slots.get(&n) {
            self.fake.remote_delete(id);
        }
    }

    fn expire_tokens(&mut self) {
        self.fake.expire_tokens();
    }

    fn snapshot(&self) -> Snapshot {
        self.fake
            .events()
            .into_iter()
            .map(|event| {
                let body = event.body["event"].clone();
                (
                    event.id,
                    (event.change_key, serde_json::from_value(body).unwrap()),
                )
            })
            .collect()
    }

    fn faults(&self) -> Vec<InjectedFault> {
        self.fake.faults().to_vec()
    }
}

struct CalDav {
    fake: FakeCalDav,
    seam: CalDavSeam,
}

impl Remote for CalDav {
    const PROVIDER: Provider = Provider::CalDav;
    const IDEMPOTENT_CREATE: bool = true;

    fn new(plan: FaultPlan) -> Self {
        Self {
            fake: FakeCalDav::new(plan),
            seam: CalDavSeam::new(Some(PAGE)),
        }
    }

    fn perform(&mut self, request: &Request) -> Outcome {
        self.seam.perform(&mut self.fake, request)
    }

    fn key(&self, n: u8) -> Option<ResourceId> {
        Some(ResourceId::new(format!("/cal/{n}.ics")))
    }

    fn remote_put(&mut self, n: u8, fields: &NormalizedEvent) {
        self.fake
            .remote_put(&format!("/cal/{n}.ics"), CalDavSeam::body(fields));
    }

    fn remote_delete(&mut self, n: u8) {
        self.fake.remote_delete(&format!("/cal/{n}.ics"));
    }

    fn expire_tokens(&mut self) {
        self.fake.expire_tokens();
    }

    fn snapshot(&self) -> Snapshot {
        self.fake
            .members()
            .into_iter()
            .map(|member| {
                (
                    member.href,
                    (member.etag, serde_json::from_str(&member.body).unwrap()),
                )
            })
            .collect()
    }

    fn faults(&self) -> Vec<InjectedFault> {
        self.fake.faults().to_vec()
    }
}

/// The application: it feeds inputs to the engine and performs its requests, in order.
struct Harness<R> {
    remote: R,
    state: SyncState,
    queue: VecDeque<(RequestId, Request)>,
    notices: Vec<Notice>,
    edits: u64,
    clock: i64,
    performed: usize,
}

impl<R: Remote> Harness<R> {
    fn new(plan: FaultPlan) -> Self {
        Self {
            remote: R::new(plan),
            state: SyncState::new(),
            queue: VecDeque::new(),
            notices: Vec::new(),
            edits: 0,
            clock: 0,
            performed: 0,
        }
    }

    fn feed(&mut self, input: Input) {
        for effect in self.state.apply(input) {
            match effect {
                Effect::Request { id, request } => self.queue.push_back((id, request)),
                Effect::Notify(notice) => self.notices.push(notice),
            }
        }
        // The state serializes, and reads back the same, after every step.
        let json = serde_json::to_string(&self.state).unwrap();
        let back: SyncState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, self.state);
    }

    /// Performs up to `limit` requests, or every request until none is left.
    fn run(&mut self, limit: Option<usize>) {
        let mut done = 0;
        while limit.is_none_or(|limit| done < limit) {
            let Some((id, request)) = self.queue.pop_front() else {
                return;
            };
            let outcome = self.remote.perform(&request);
            self.feed(Input::Outcome {
                request: id,
                outcome,
            });
            done += 1;
            self.performed += 1;
            assert!(self.performed < MAX_REQUESTS, "the engine did not settle");
        }
    }

    /// The application stops, losing every request not yet performed, and, with `sent`, the
    /// response to the one it was performing. It reloads the state and resumes.
    fn restart(&mut self, sent: bool) {
        if sent && let Some((_, request)) = self.queue.front() {
            let request = request.clone();
            let _ = self.remote.perform(&request);
        }
        self.queue.clear();
        let json = serde_json::to_string(&self.state).unwrap();
        self.state = serde_json::from_str(&json).unwrap();
        self.feed(Input::Resume);
    }

    fn edit(&mut self, resource: ResourceId, operation: Operation) {
        self.edits += 1;
        let edit = Edit {
            id: EditId::new(format!("edit{}", self.edits)),
            resource,
            operation,
            role: Role::Owner,
            expires_at: Some(Timestamp::from_second(self.clock + LIFETIME).unwrap()),
        };
        self.feed(Input::Edit(edit));
    }

    fn apply(&mut self, op: &Op) {
        match *op {
            Op::RemotePut(n) => {
                let title = format!("remote {}", self.performed);
                self.remote.remote_put(n, &fields(&title));
            }
            Op::RemoteDelete(n) => self.remote.remote_delete(n),
            Op::LocalCreate(n) => {
                let Some(resource) = self.remote.key(n) else {
                    return;
                };
                if !R::IDEMPOTENT_CREATE || self.state.records().contains_key(&resource) {
                    return;
                }
                let event = ProviderEvent {
                    ids: ProviderIds::new(R::PROVIDER, resource.as_str()),
                    revision: None,
                    mapper_version: MAPPER_VERSION,
                    fields: fields(&format!("created {}", self.edits)),
                    passthrough: Passthrough::new(),
                };
                self.edit(resource, Operation::Create(event));
            }
            Op::LocalUpdate(n) => {
                let Some(resource) = self.remote.key(n) else {
                    return;
                };
                let Some(record) = self.state.records().get(&resource) else {
                    return;
                };
                let base = record.fields.clone();
                let mut ours = base.clone();
                ours.title = Some(format!("local {}", self.edits));
                ours.location = Some("here".to_owned());
                self.edit(resource, Operation::Update { base, ours });
            }
            Op::LocalDelete(n) => {
                let Some(resource) = self.remote.key(n) else {
                    return;
                };
                if self.state.records().contains_key(&resource) {
                    self.edit(resource, Operation::Delete);
                }
            }
            Op::Sync => self.feed(Input::Sync),
            Op::Run(count) => self.run(Some(usize::from(count))),
            Op::Restart { sent } => self.restart(sent),
            Op::ExpireTokens => self.remote.expire_tokens(),
            Op::Tick => {
                self.clock += 1;
                let now = Timestamp::from_second(self.clock).unwrap();
                self.feed(Input::Tick { now });
            }
        }
    }

    /// Runs everything queued, then syncs once more, and returns what the engine mirrors.
    fn settle(&mut self) -> Snapshot {
        self.run(None);
        self.feed(Input::Sync);
        self.run(None);
        assert!(self.state.is_quiet(), "the engine is still busy");
        self.state
            .records()
            .iter()
            .map(|(resource, record)| {
                let revision = record.revision.as_ref().unwrap().as_str().to_owned();
                (
                    resource.as_str().to_owned(),
                    (revision, record.fields.clone()),
                )
            })
            .collect()
    }

    /// Each edit ended in exactly one notice.
    fn ended_edits(&self) -> u64 {
        let ended = self
            .notices
            .iter()
            .filter(|notice| {
                matches!(
                    notice,
                    Notice::Applied { .. } | Notice::Rejected { .. } | Notice::Expired { .. }
                )
            })
            .count();
        u64::try_from(ended).unwrap()
    }
}

#[derive(Clone, Debug)]
enum Op {
    RemotePut(u8),
    RemoteDelete(u8),
    LocalCreate(u8),
    LocalUpdate(u8),
    LocalDelete(u8),
    Sync,
    /// Perform this many queued requests.
    Run(u8),
    /// The application restarts, losing what was queued.
    Restart {
        sent: bool,
    },
    ExpireTokens,
    Tick,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..6u8).prop_map(Op::RemotePut),
        1 => (0..6u8).prop_map(Op::RemoteDelete),
        2 => (0..6u8).prop_map(Op::LocalCreate),
        3 => (0..6u8).prop_map(Op::LocalUpdate),
        1 => (0..6u8).prop_map(Op::LocalDelete),
        2 => Just(Op::Sync),
        4 => (1..8u8).prop_map(Op::Run),
        1 => any::<bool>().prop_map(|sent| Op::Restart { sent }),
        1 => Just(Op::ExpireTokens),
        1 => Just(Op::Tick),
    ]
}

fn plan(seed: u64) -> FaultPlan {
    FaultPlan::seeded(seed).with_all(120)
}

struct Run {
    mirror: Snapshot,
    server: Snapshot,
    faults: Vec<InjectedFault>,
    state: String,
}

fn run<R: Remote>(seed: u64, ops: &[Op]) -> Run {
    let mut harness = Harness::<R>::new(plan(seed));
    for op in ops {
        harness.apply(op);
    }
    let mirror = harness.settle();
    assert_eq!(harness.ended_edits(), harness.edits, "an edit was lost");
    Run {
        mirror,
        server: harness.remote.snapshot(),
        faults: harness.remote.faults(),
        state: serde_json::to_string(&harness.state).unwrap(),
    }
}

fn converges<R: Remote>(seed: u64, ops: &[Op]) -> Result<(), TestCaseError> {
    let first = run::<R>(seed, ops);
    prop_assert_eq!(&first.mirror, &first.server);
    let second = run::<R>(seed, ops);
    prop_assert_eq!(&first.state, &second.state);
    prop_assert_eq!(&first.faults, &second.faults);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn google_converges(seed: u64, ops in prop::collection::vec(op(), 0..80)) {
        converges::<Google>(seed, &ops)?;
    }

    #[test]
    fn graph_converges(seed: u64, ops in prop::collection::vec(op(), 0..80)) {
        converges::<Graph>(seed, &ops)?;
    }

    #[test]
    fn caldav_converges(seed: u64, ops in prop::collection::vec(op(), 0..80)) {
        converges::<CalDav>(seed, &ops)?;
    }
}

/// A long fixed scenario, so each fake injects every fault it can while the engine runs.
fn long_scenario() -> Vec<Op> {
    (0..600u16)
        .map(|i| {
            // Slots spread by a multiplicative hash, so every operation reaches every slot.
            let n =
                u8::try_from((u64::from(i).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) % 6).unwrap();
            match i % 12 {
                0 | 1 => Op::RemotePut(n),
                2 => Op::LocalCreate(n),
                3 | 4 => Op::LocalUpdate(n),
                5 => Op::Sync,
                6 | 7 => Op::Run(5),
                8 => Op::RemoteDelete(n),
                9 => Op::LocalDelete(n.wrapping_add(1) % 6),
                10 if i % 60 == 10 => Op::Restart {
                    sent: i % 120 == 10,
                },
                10 if i % 48 == 10 => Op::ExpireTokens,
                _ => Op::Tick,
            }
        })
        .collect()
}

#[test]
fn every_fault_is_injected_and_survived() {
    let ops = long_scenario();
    for (name, run) in [
        ("google", run::<Google>(7, &ops)),
        ("graph", run::<Graph>(7, &ops)),
        ("caldav", run::<CalDav>(7, &ops)),
    ] {
        assert_eq!(run.mirror, run.server, "{name} did not converge");
        assert!(!run.mirror.is_empty(), "{name} ended empty");
        let kinds: Vec<Fault> = Fault::ALL
            .into_iter()
            .filter(|fault| run.faults.iter().any(|injected| injected.fault == *fault))
            .collect();
        assert_eq!(kinds, Fault::ALL, "{name} did not inject every fault");
    }
}

// Targeted cases, driven by hand.

fn event(id: &str, revision: &str, title: &str) -> ProviderEvent {
    ProviderEvent {
        ids: ProviderIds::new(Provider::Google, id),
        revision: Some(calendar_sync_model::ProviderRevision::new(revision)),
        mapper_version: MAPPER_VERSION,
        fields: fields(title),
        passthrough: Passthrough::new(),
    }
}

fn requests(effects: &[Effect]) -> Vec<(RequestId, Request)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Request { id, request } => Some((*id, request.clone())),
            Effect::Notify(_) => None,
        })
        .collect()
}

fn upsert(event: &ProviderEvent) -> calendar_sync_engine::Change {
    calendar_sync_engine::Change::Upsert {
        resource: ResourceId::of(event),
        revision: event.revision.clone().unwrap(),
        event: Some(event.clone()),
    }
}

fn listed(changes: Vec<calendar_sync_engine::Change>, next: Next) -> Outcome {
    Outcome::Listed(Page { changes, next })
}

fn cursor(text: &str) -> calendar_sync_engine::Cursor {
    calendar_sync_engine::Cursor::new(text)
}

/// A state holding `a` and `b` after one complete full pass, with cursor `c1`.
fn synced() -> SyncState {
    let mut state = SyncState::new();
    let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
    state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![upsert(&event("a", "1", "a")), upsert(&event("b", "1", "b"))],
            Next::Done(cursor("c1")),
        ),
    });
    assert_eq!(state.records().len(), 2);
    state
}

fn ids(state: &SyncState) -> Vec<&str> {
    state.records().keys().map(ResourceId::as_str).collect()
}

#[test]
fn a_full_listing_that_fails_midway_deletes_nothing() {
    let mut state = synced();
    // The cursor expires, so a full listing starts.
    let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: Outcome::Failed(calendar_sync_engine::Failure::CursorExpired),
    });
    let (list, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::List {
            from: ListFrom::Start
        }
    );
    // Its first page lists only `a`, and the listing then fails for good.
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![upsert(&event("a", "1", "a"))],
            Next::More(cursor("p2")),
        ),
    });
    let (list, _) = requests(&effects)[0].clone();
    assert_eq!(ids(&state), ["a", "b"]);
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: Outcome::Failed(calendar_sync_engine::Failure::Rejected("boom".to_owned())),
    });
    assert!(matches!(
        &effects[..],
        [Effect::Notify(Notice::ListingFailed { .. })]
    ));
    assert_eq!(ids(&state), ["a", "b"]);
    assert!(!state.is_syncing());
}

#[test]
fn a_truncated_full_listing_deletes_only_once_it_completes() {
    let mut state = synced();
    let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: Outcome::Failed(calendar_sync_engine::Failure::CursorExpired),
    });
    let (list, _) = requests(&effects)[0].clone();
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![upsert(&event("a", "1", "a"))],
            Next::More(cursor("p2")),
        ),
    });
    // The application stops here, and resumes from the serialized state.
    let json = serde_json::to_string(&state).unwrap();
    let mut state: SyncState = serde_json::from_str(&json).unwrap();
    let late = requests(&effects)[0].0;
    let effects = state.apply(Input::Resume);
    let (list, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::List {
            from: ListFrom::Continue(cursor("p2"))
        }
    );
    // An outcome for the request lost in the restart is ignored.
    assert!(
        state
            .apply(Input::Outcome {
                request: late,
                outcome: listed(vec![], Next::Done(cursor("x"))),
            })
            .is_empty()
    );
    assert_eq!(ids(&state), ["a", "b"]);
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: listed(vec![], Next::Done(cursor("c2"))),
    });
    assert_eq!(effects, [Effect::Notify(Notice::Synced)]);
    assert_eq!(ids(&state), ["a"]);
    assert_eq!(state.cursor(), Some(&cursor("c2")));
}

#[test]
fn a_record_from_a_newer_mapper_is_never_overwritten_by_an_older_one() {
    let mut state = synced();
    let with_mapper = |revision: &str, title: &str, mapper: u32| {
        let mut record = event("a", revision, title);
        record.mapper_version = calendar_sync_model::MapperVersion::new(mapper);
        record
    };
    let pass = |state: &mut SyncState, record: ProviderEvent| {
        let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
        let changes = vec![calendar_sync_engine::Change::Upsert {
            resource: ResourceId::new("a"),
            revision: record.revision.clone().unwrap(),
            event: Some(record),
        }];
        state.apply(Input::Outcome {
            request: list,
            outcome: listed(changes, Next::Done(cursor("c"))),
        });
        state.records()[&ResourceId::new("a")].clone()
    };
    // A listing at the revision held writes nothing, even from a newer mapper.
    assert_eq!(
        pass(&mut state, with_mapper("1", "same", 2))
            .fields
            .title
            .as_deref(),
        Some("a")
    );
    // A new revision read by a newer mapper replaces the record.
    let newer = pass(&mut state, with_mapper("2", "newer", 2));
    assert_eq!(newer.fields.title.as_deref(), Some("newer"));
    // A new revision read by an older mapper does not.
    assert_eq!(pass(&mut state, with_mapper("3", "older", 1)), newer);
}

#[test]
fn a_replayed_change_changes_nothing() {
    let mut state = synced();
    let before = state.records().clone();
    let (list, request) = requests(&state.apply(Input::Sync))[0].clone();
    assert_eq!(
        request,
        Request::List {
            from: ListFrom::Since(cursor("c1"))
        }
    );
    // A replay of `a` at the revision held, carrying a body that differs.
    state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![upsert(&event("a", "1", "other"))],
            Next::Done(cursor("c2")),
        ),
    });
    assert_eq!(state.records(), &before);
}

#[test]
fn a_change_listed_without_its_body_is_read_before_the_pass_completes() {
    let mut state = synced();
    let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![calendar_sync_engine::Change::Upsert {
                resource: ResourceId::new("a"),
                revision: calendar_sync_model::ProviderRevision::new("2"),
                event: None,
            }],
            Next::Done(cursor("c2")),
        ),
    });
    let (read, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::Read {
            resource: ResourceId::new("a")
        }
    );
    assert!(state.is_syncing());
    let effects = state.apply(Input::Outcome {
        request: read,
        outcome: Outcome::Read(Some(event("a", "2", "renamed"))),
    });
    assert_eq!(effects, [Effect::Notify(Notice::Synced)]);
    let title = state.records()[&ResourceId::new("a")].fields.title.clone();
    assert_eq!(title.as_deref(), Some("renamed"));
}

#[test]
fn an_edit_survives_a_full_resync() {
    let mut state = synced();
    let base = fields("a");
    let mut ours = base.clone();
    ours.title = Some("mine".to_owned());
    let effects = state.apply(Input::Edit(Edit {
        id: EditId::new("e1"),
        resource: ResourceId::new("a"),
        operation: Operation::Update { base, ours },
        role: Role::Owner,
        expires_at: None,
    }));
    let (read, _) = requests(&effects)[0].clone();
    // A full resync runs while the edit's read is in flight.
    let (list, _) = requests(&state.apply(Input::Sync))[0].clone();
    let effects = state.apply(Input::Outcome {
        request: list,
        outcome: Outcome::Failed(calendar_sync_engine::Failure::CursorExpired),
    });
    let (list, _) = requests(&effects)[0].clone();
    state.apply(Input::Outcome {
        request: list,
        outcome: listed(
            vec![upsert(&event("a", "1", "a"))],
            Next::Done(cursor("c2")),
        ),
    });
    assert_eq!(state.pending_edits().count(), 1);
    // The read plans the write from the provider's record.
    let effects = state.apply(Input::Outcome {
        request: read,
        outcome: Outcome::Read(Some(event("a", "1", "a"))),
    });
    let (write, request) = requests(&effects)[0].clone();
    let Request::Update {
        if_match, event: e, ..
    } = request
    else {
        panic!("expected an update, got {request:?}");
    };
    assert_eq!(if_match.unwrap().as_str(), "1");
    assert_eq!(e.fields.title.as_deref(), Some("mine"));
    let effects = state.apply(Input::Outcome {
        request: write,
        outcome: Outcome::Written(event("a", "2", "mine")),
    });
    assert!(
        matches!(&effects[..], [Effect::Notify(Notice::Applied { edit, .. })] if edit.as_str() == "e1")
    );
    assert!(state.is_quiet());
}

#[test]
fn an_edit_expires_by_the_injected_clock() {
    let mut state = synced();
    let at = Timestamp::from_second(100).unwrap();
    let edit = |id: &str| Edit {
        id: EditId::new(id),
        resource: ResourceId::new("b"),
        operation: Operation::Delete,
        role: Role::Owner,
        expires_at: Some(at),
    };
    let effects = state.apply(Input::Edit(edit("e1")));
    let (read, _) = requests(&effects)[0].clone();
    let effects = state.apply(Input::Tick { now: at });
    assert_eq!(
        effects,
        [Effect::Notify(Notice::Expired {
            edit: EditId::new("e1")
        })]
    );
    // The expired edit's read is no longer awaited.
    assert!(
        state
            .apply(Input::Outcome {
                request: read,
                outcome: Outcome::Read(None),
            })
            .is_empty()
    );
    // An edit already past its time expires on arrival.
    let effects = state.apply(Input::Edit(edit("e2")));
    assert!(matches!(
        &effects[..],
        [Effect::Notify(Notice::Expired { .. })]
    ));
    assert!(state.is_quiet());
    assert_eq!(ids(&state), ["a", "b"]);
}

#[test]
fn an_attendee_cannot_delete() {
    let mut state = synced();
    let effects = state.apply(Input::Edit(Edit {
        id: EditId::new("e1"),
        resource: ResourceId::new("a"),
        operation: Operation::Delete,
        role: Role::Attendee {
            address: "me@example.com".to_owned(),
        },
        expires_at: None,
    }));
    assert!(matches!(
        &effects[..],
        [Effect::Notify(Notice::Rejected {
            reason: calendar_sync_engine::Rejection::NotOwner,
            ..
        })]
    ));
}

#[test]
fn a_create_over_a_deleted_google_id_is_rejected_not_repeated() {
    // Google keeps a cancelled event's id, so inserting it again answers 409 for good.
    let mut harness = Harness::<Google>::new(FaultPlan::none());
    harness.apply(&Op::RemotePut(0));
    harness.apply(&Op::RemoteDelete(0));
    harness.apply(&Op::LocalCreate(0));
    harness.run(None);
    assert!(harness.state.is_quiet());
    assert!(matches!(
        &harness.notices[..],
        [Notice::Rejected {
            reason: calendar_sync_engine::Rejection::Exists,
            ..
        }]
    ));
}

#[test]
fn a_create_whose_response_was_lost_is_applied_once() {
    let plan = FaultPlan::none().at_call(0, Fault::LostResponse);
    let mut harness = Harness::<CalDav>::new(plan);
    harness.apply(&Op::LocalCreate(0));
    harness.run(None);
    assert!(matches!(&harness.notices[..], [Notice::Applied { .. }]));
    assert_eq!(harness.remote.snapshot().len(), 1);
    assert_eq!(harness.settle(), harness.remote.snapshot());
}

#[test]
fn a_write_before_a_full_listing_does_not_keep_a_deleted_event() {
    let mut harness = Harness::<Google>::new(FaultPlan::none());
    harness.apply(&Op::RemotePut(4));
    harness.apply(&Op::Sync);
    harness.run(None);
    harness.apply(&Op::ExpireTokens);
    harness.apply(&Op::LocalUpdate(4));
    harness.apply(&Op::Sync);
    // The edit's read; the expired listing, which starts a full one; the edit's write, which
    // lands while the full listing is still to come.
    harness.run(Some(3));
    assert!(harness.state.is_syncing());
    assert!(matches!(
        &harness.notices[..],
        [Notice::Synced, Notice::Applied { .. }]
    ));
    // Another client deletes the event before the full listing's snapshot.
    harness.apply(&Op::RemoteDelete(4));
    harness.run(None);
    assert!(harness.remote.snapshot().is_empty());
    assert!(harness.state.records().is_empty());
}
