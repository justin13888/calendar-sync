//! A client that follows each protocol's rules mirrors the fake exactly, whatever faults the
//! fake injects, and the same seed injects the same faults.

use std::collections::{BTreeMap, BTreeSet};

use calendar_sync_testkit::caldav::{self, FakeCalDav, Precondition, SyncCollection, SyncResponse};
use calendar_sync_testkit::google::{self, EventStatus, FakeGoogle, ListRequest};
use calendar_sync_testkit::graph::{self, DeltaItem, DeltaRequest, FakeGraph};
use calendar_sync_testkit::{Fault, FaultPlan, InjectedFault};
use proptest::prelude::*;
use serde_json::json;

/// More calls than any sync needs at the rates these tests use.
const MAX_CALLS: usize = 10_000;
const PAGE: usize = 2;

/// What the client mirrors: id to revision.
type Mirror = BTreeMap<String, String>;

#[derive(Clone, Debug)]
enum Op {
    /// Another client creates or replaces resource `n`.
    RemotePut(u8),
    /// Another client deletes resource `n`.
    RemoteDelete(u8),
    /// The client updates resource `n` conditionally, ignoring the outcome.
    ClientUpdate(u8),
    /// The client syncs.
    Sync,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..8u8).prop_map(Op::RemotePut),
        1 => (0..8u8).prop_map(Op::RemoteDelete),
        1 => (0..8u8).prop_map(Op::ClientUpdate),
        1 => Just(Op::Sync),
    ]
}

fn plan(seed: u64) -> FaultPlan {
    FaultPlan::seeded(seed).with_all(150)
}

// Google

#[derive(Default)]
struct GoogleClient {
    items: Mirror,
    sync_token: Option<String>,
}

impl GoogleClient {
    fn sync(&mut self, fake: &mut FakeGoogle) {
        let mut staged = self.start();
        let mut page_token = None;
        for _ in 0..MAX_CALLS {
            let request = ListRequest {
                sync_token: self.sync_token.clone(),
                page_token: page_token.clone(),
                max_results: Some(PAGE),
            };
            match fake.list(&request) {
                Ok(page) => {
                    for event in page.items {
                        match event.status {
                            EventStatus::Confirmed => staged.insert(event.id, event.etag),
                            EventStatus::Cancelled => staged.remove(&event.id),
                        };
                    }
                    if page.next_page_token.is_some() {
                        page_token = page.next_page_token;
                    } else {
                        self.items = staged;
                        self.sync_token = page.next_sync_token;
                        return;
                    }
                }
                Err(google::Error::Gone) => {
                    self.sync_token = None;
                    page_token = None;
                    staged = self.start();
                }
                Err(google::Error::LostResponse) => {}
                Err(error) => panic!("unexpected {error:?}"),
            }
        }
        panic!("sync did not finish");
    }

    /// A full pass starts from nothing; an incremental pass from the mirror.
    fn start(&self) -> Mirror {
        if self.sync_token.is_some() {
            self.items.clone()
        } else {
            Mirror::new()
        }
    }
}

fn run_google(seed: u64, ops: &[Op]) -> (Mirror, Mirror, Vec<InjectedFault>) {
    let mut fake = FakeGoogle::new(plan(seed));
    let mut client = GoogleClient::default();
    for op in ops {
        match op {
            Op::RemotePut(n) => fake.remote_put(&format!("e{n}"), json!({ "n": n })),
            Op::RemoteDelete(n) => fake.remote_delete(&format!("e{n}")),
            Op::ClientUpdate(n) => {
                let id = format!("e{n}");
                if let Some(etag) = client.items.get(&id) {
                    let _ = fake.update(&id, Some(etag), json!({ "n": n, "edited": true }));
                }
            }
            Op::Sync => client.sync(&mut fake),
        }
    }
    client.sync(&mut fake);
    let server = fake
        .events()
        .into_iter()
        .map(|event| (event.id, event.etag))
        .collect();
    (client.items, server, fake.faults().to_vec())
}

// Graph

#[derive(Default)]
struct GraphClient {
    items: Mirror,
    delta_link: Option<String>,
}

impl GraphClient {
    fn sync(&mut self, fake: &mut FakeGraph) {
        let mut staged = self.start();
        let mut link = self.delta_link.clone();
        for _ in 0..MAX_CALLS {
            let request = DeltaRequest {
                link: link.clone(),
                max_page_size: Some(PAGE),
            };
            match fake.delta(&request) {
                Ok(page) => {
                    for item in page.value {
                        match item {
                            DeltaItem::Changed(event) => staged.insert(event.id, event.change_key),
                            DeltaItem::Removed { id } => staged.remove(&id),
                        };
                    }
                    if page.next_link.is_some() {
                        link = page.next_link;
                    } else {
                        self.items = staged;
                        self.delta_link = page.delta_link;
                        return;
                    }
                }
                Err(graph::Error::SyncStateNotFound) => {
                    self.delta_link = None;
                    link = None;
                    staged = self.start();
                }
                Err(graph::Error::LostResponse) => {}
                Err(error) => panic!("unexpected {error:?}"),
            }
        }
        panic!("sync did not finish");
    }

    fn start(&self) -> Mirror {
        if self.delta_link.is_some() {
            self.items.clone()
        } else {
            Mirror::new()
        }
    }
}

fn run_graph(seed: u64, ops: &[Op]) -> (Mirror, Mirror, Vec<InjectedFault>) {
    let mut fake = FakeGraph::new(plan(seed));
    let mut client = GraphClient::default();
    // Graph assigns ids, so slot `n` names whichever id was created for it last.
    let mut slots: BTreeMap<u8, String> = BTreeMap::new();
    for op in ops {
        match op {
            Op::RemotePut(n) => match slots.get(n) {
                Some(id) if fake.events().iter().any(|event| &event.id == id) => {
                    fake.remote_update(id, json!({ "n": n }));
                }
                _ => {
                    slots.insert(*n, fake.remote_create(json!({ "n": n })));
                }
            },
            Op::RemoteDelete(n) => {
                if let Some(id) = slots.get(n) {
                    fake.remote_delete(id);
                }
            }
            Op::ClientUpdate(n) => {
                if let Some(id) = slots.get(n)
                    && let Some(key) = client.items.get(id)
                {
                    let _ = fake.patch(id, Some(key), json!({ "edited": true }));
                }
            }
            Op::Sync => client.sync(&mut fake),
        }
    }
    client.sync(&mut fake);
    let server = fake
        .events()
        .into_iter()
        .map(|event| (event.id, event.change_key))
        .collect();
    (client.items, server, fake.faults().to_vec())
}

// CalDAV

#[derive(Default)]
struct DavClient {
    items: Mirror,
    sync_token: Option<String>,
}

impl DavClient {
    fn sync(&mut self, fake: &mut FakeCalDav) {
        // An initial sync starts from nothing; a truncated report's token continues the same pass.
        let mut staged = if self.sync_token.is_none() {
            Mirror::new()
        } else {
            self.items.clone()
        };
        let mut token = self.sync_token.clone();
        for _ in 0..MAX_CALLS {
            let request = SyncCollection {
                sync_token: token.clone(),
                limit: Some(PAGE),
            };
            match fake.sync_collection(&request) {
                Ok(report) => {
                    for response in report.responses {
                        match response {
                            SyncResponse::Changed { href, etag } => staged.insert(href, etag),
                            SyncResponse::Removed { href } => staged.remove(&href),
                        };
                    }
                    token = Some(report.sync_token);
                    if !report.truncated {
                        self.items = staged;
                        self.sync_token = token;
                        return;
                    }
                }
                Err(caldav::Error::InvalidSyncToken) => {
                    staged = Mirror::new();
                    token = None;
                }
                Err(caldav::Error::LostResponse) => {}
                Err(error) => panic!("unexpected {error:?}"),
            }
        }
        panic!("sync did not finish");
    }
}

fn run_caldav(seed: u64, ops: &[Op]) -> (Mirror, Mirror, Vec<InjectedFault>) {
    let mut fake = FakeCalDav::new(plan(seed));
    let mut client = DavClient::default();
    for op in ops {
        match op {
            Op::RemotePut(n) => fake.remote_put(&format!("/cal/{n}.ics"), format!("n={n}")),
            Op::RemoteDelete(n) => fake.remote_delete(&format!("/cal/{n}.ics")),
            Op::ClientUpdate(n) => {
                let href = format!("/cal/{n}.ics");
                if let Some(etag) = client.items.get(&href) {
                    let _ = fake.put(
                        &href,
                        &Precondition::IfMatch(etag.clone()),
                        format!("n={n} edited"),
                    );
                }
            }
            Op::Sync => client.sync(&mut fake),
        }
    }
    client.sync(&mut fake);
    let server = fake
        .members()
        .into_iter()
        .map(|member| (member.href, member.etag))
        .collect();
    (client.items, server, fake.faults().to_vec())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn google_client_converges(seed: u64, ops in prop::collection::vec(op(), 0..60)) {
        let (mirror, server, faults) = run_google(seed, &ops);
        prop_assert_eq!(&mirror, &server);
        prop_assert_eq!(run_google(seed, &ops), (mirror, server, faults));
    }

    #[test]
    fn graph_client_converges(seed: u64, ops in prop::collection::vec(op(), 0..60)) {
        let (mirror, server, faults) = run_graph(seed, &ops);
        prop_assert_eq!(&mirror, &server);
        prop_assert_eq!(run_graph(seed, &ops), (mirror, server, faults));
    }

    #[test]
    fn caldav_client_converges(seed: u64, ops in prop::collection::vec(op(), 0..60)) {
        let (mirror, server, faults) = run_caldav(seed, &ops);
        prop_assert_eq!(&mirror, &server);
        prop_assert_eq!(run_caldav(seed, &ops), (mirror, server, faults));
    }
}

/// A long fixed scenario, so each fake shows every fault it can inject.
fn long_scenario() -> Vec<Op> {
    (0..400u16)
        .map(|i| {
            let n = u8::try_from(i % 8).unwrap_or(0);
            match i % 6 {
                0..=2 => Op::RemotePut(n),
                3 => Op::ClientUpdate(n),
                4 => Op::Sync,
                _ => Op::RemoteDelete(n),
            }
        })
        .collect()
}

fn kinds(faults: &[InjectedFault]) -> BTreeSet<Fault> {
    faults.iter().map(|fault| fault.fault).collect()
}

#[test]
fn every_fault_is_injected_and_survived() {
    let all: BTreeSet<Fault> = Fault::ALL.into_iter().collect();
    let ops = long_scenario();
    for (name, (mirror, server, faults)) in [
        ("google", run_google(7, &ops)),
        ("graph", run_graph(7, &ops)),
        ("caldav", run_caldav(7, &ops)),
    ] {
        assert_eq!(mirror, server, "{name} did not converge");
        assert_eq!(kinds(&faults), all, "{name} did not inject every fault");
    }
}

#[test]
fn a_different_seed_injects_different_faults() {
    let ops = long_scenario();
    assert_ne!(run_google(1, &ops).2, run_google(2, &ops).2);
    assert_ne!(run_graph(1, &ops).2, run_graph(2, &ops).2);
    assert_ne!(run_caldav(1, &ops).2, run_caldav(2, &ops).2);
}
