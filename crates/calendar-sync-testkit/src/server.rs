//! The state every fake shares: a collection of resources, a change sequence, the sync passes
//! over it, and the fault injector.
//!
//! Every change takes the next sequence number, and a resource's revision is the number of its
//! last change, so a revision is unique to one version. A sync pass lists, in id order, the
//! resources whose last change falls between the token it started from and the sequence number
//! when it started, so a change made during a pass is reported by the next one. Paging is by the
//! last id delivered, which stays correct while the collection changes.

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::fault::{CallKind, Fault, FaultPlan, InjectedFault, Injector};

/// One resource as the fake holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resource<B> {
    pub(crate) body: B,
    /// The sequence number of the resource's last change.
    pub(crate) seq: u64,
    pub(crate) deleted: bool,
}

/// Where a sync pass stands. Encoded into the opaque tokens the fakes hand out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    /// Tokens from an earlier epoch have expired.
    epoch: u64,
    /// A full pass omits deleted resources.
    full: bool,
    /// Changes after this sequence number are reported.
    since: u64,
    /// The sequence number when the pass started, once it has.
    upto: Option<u64>,
    /// The last id delivered in this pass.
    after: Option<String>,
}

impl Cursor {
    pub(crate) fn encode(&self) -> String {
        let upto = self
            .upto
            .map_or_else(|| "-".to_owned(), |upto| upto.to_string());
        let after = self.after.as_deref().map_or_else(|| "-".to_owned(), hex);
        let kind = if self.full { 'f' } else { 'i' };
        format!("{}.{kind}.{}.{upto}.{after}", self.epoch, self.since)
    }

    pub(crate) fn decode(token: &str) -> Option<Self> {
        let mut parts = token.split('.');
        let epoch = parts.next()?.parse().ok()?;
        let full = match parts.next()? {
            "f" => true,
            "i" => false,
            _ => return None,
        };
        let since = parts.next()?.parse().ok()?;
        let upto = match parts.next()? {
            "-" => None,
            upto => Some(upto.parse().ok()?),
        };
        let after = match parts.next()? {
            "-" => None,
            after => Some(unhex(after)?),
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            epoch,
            full,
            since,
            upto,
            after,
        })
    }
}

fn hex(text: &str) -> String {
    use std::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// One page of a sync pass.
#[derive(Clone, Debug)]
pub(crate) struct Page<B> {
    /// The resources on the page, in id order, followed by any replayed ones.
    pub(crate) items: Vec<(String, Resource<B>)>,
    /// More of this pass follows from this token.
    pub(crate) next: Option<String>,
    /// The pass is complete; the next pass starts from this token.
    pub(crate) sync: Option<String>,
}

/// Why a sync call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncFailure {
    /// The token is not one this fake issued in its current epoch.
    TokenExpired,
    /// The response was lost.
    LostResponse,
}

/// The collection and the injector one fake owns.
#[derive(Clone, Debug)]
pub(crate) struct Server<B> {
    resources: BTreeMap<String, Resource<B>>,
    seq: u64,
    epoch: u64,
    injector: Injector,
}

impl<B: Clone> Server<B> {
    pub(crate) fn new(plan: FaultPlan) -> Self {
        Self {
            resources: BTreeMap::new(),
            seq: 0,
            epoch: 0,
            injector: Injector::new(plan),
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Resource<B>> {
        self.resources.get(id)
    }

    pub(crate) fn live(&self, id: &str) -> Option<&Resource<B>> {
        self.resources.get(id).filter(|resource| !resource.deleted)
    }

    pub(crate) fn live_resources(&self) -> impl Iterator<Item = (&String, &Resource<B>)> {
        self.resources
            .iter()
            .filter(|(_, resource)| !resource.deleted)
    }

    /// Create or replace a resource, returning its new revision.
    pub(crate) fn upsert(&mut self, id: &str, body: B) -> u64 {
        self.seq += 1;
        let seq = self.seq;
        self.resources.insert(
            id.to_owned(),
            Resource {
                body,
                seq,
                deleted: false,
            },
        );
        seq
    }

    /// Give a live resource a new revision without changing its body, as a concurrent writer
    /// touching a field outside the body would.
    pub(crate) fn touch(&mut self, id: &str) -> Option<u64> {
        let resource = self.resources.get_mut(id).filter(|r| !r.deleted)?;
        self.seq += 1;
        resource.seq = self.seq;
        Some(self.seq)
    }

    /// Delete a live resource, keeping a tombstone so incremental syncs report it.
    pub(crate) fn remove(&mut self, id: &str) -> Option<u64> {
        let resource = self.resources.get_mut(id).filter(|r| !r.deleted)?;
        self.seq += 1;
        resource.seq = self.seq;
        resource.deleted = true;
        Some(self.seq)
    }

    /// Invalidate every token issued so far.
    pub(crate) fn expire_tokens(&mut self) {
        self.epoch += 1;
    }

    pub(crate) fn begin(&mut self, kind: CallKind) -> Option<Fault> {
        self.injector.begin(kind)
    }

    pub(crate) fn record(&mut self, fault: Fault) {
        self.injector.record(fault);
    }

    pub(crate) fn faults(&self) -> &[InjectedFault] {
        self.injector.log()
    }

    pub(crate) fn calls(&self) -> u64 {
        self.injector.calls()
    }

    /// Serve one page of a sync pass, starting a full pass when `token` is `None`.
    pub(crate) fn sync(
        &mut self,
        token: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Page<B>, SyncFailure> {
        let fault = self.begin(CallKind::Sync);
        let cursor = match token {
            None => Cursor {
                epoch: self.epoch,
                full: true,
                since: 0,
                upto: None,
                after: None,
            },
            Some(token) => match Cursor::decode(token) {
                Some(cursor) if cursor.epoch == self.epoch => cursor,
                _ => return Err(SyncFailure::TokenExpired),
            },
        };
        if token.is_some() && fault == Some(Fault::TokenExpired) {
            self.expire_tokens();
            self.record(Fault::TokenExpired);
            return Err(SyncFailure::TokenExpired);
        }

        let upto = cursor.upto.unwrap_or(self.seq);
        let in_pass = |resource: &Resource<B>| {
            resource.seq > cursor.since
                && resource.seq <= upto
                && !(cursor.full && resource.deleted)
        };
        let start = cursor
            .after
            .as_ref()
            .map_or(Bound::Unbounded, |after| Bound::Excluded(after.clone()));
        let matching: Vec<&String> = self
            .resources
            .range::<String, _>((start, Bound::Unbounded))
            .filter(|(_, resource)| in_pass(resource))
            .map(|(id, _)| id)
            .collect();

        let mut take = matching.len().min(limit.unwrap_or(usize::MAX).max(1));
        let mut partial = false;
        if fault == Some(Fault::PartialPage) && take >= 2 {
            take = 1 + self.injector.rng().index(take - 1);
            partial = true;
        }
        let mut items: Vec<(String, Resource<B>)> = matching[..take]
            .iter()
            .map(|id| ((*id).clone(), self.resources[*id].clone()))
            .collect();

        let mut replayed = false;
        if fault == Some(Fault::Replay) {
            // Changes delivered before this page: by an earlier pass, or by an earlier page of
            // this one.
            let delivered = |id: &String, resource: &Resource<B>| {
                let earlier_pass = resource.seq <= cursor.since;
                let earlier_page = cursor.after.as_ref().is_some_and(|after| id <= after)
                    && resource.seq > cursor.since
                    && resource.seq <= upto;
                (earlier_pass || earlier_page) && !(cursor.full && resource.deleted)
            };
            let candidates: Vec<(String, Resource<B>)> = self
                .resources
                .iter()
                .filter(|(id, resource)| delivered(id, resource))
                .map(|(id, resource)| (id.clone(), resource.clone()))
                .collect();
            if !candidates.is_empty() {
                let count = 1 + self.injector.rng().index(candidates.len().min(3));
                let mut picked: Vec<usize> = Vec::with_capacity(count);
                while picked.len() < count {
                    let index = self.injector.rng().index(candidates.len());
                    if !picked.contains(&index) {
                        picked.push(index);
                    }
                }
                picked.sort_unstable();
                items.extend(picked.into_iter().map(|index| candidates[index].clone()));
                replayed = true;
            }
        }

        let (next, sync) = if take < matching.len() {
            let next = Cursor {
                epoch: self.epoch,
                full: cursor.full,
                since: cursor.since,
                upto: Some(upto),
                after: Some(matching[take - 1].clone()),
            };
            (Some(next.encode()), None)
        } else {
            let sync = Cursor {
                epoch: self.epoch,
                full: false,
                since: upto,
                upto: None,
                after: None,
            };
            (None, Some(sync.encode()))
        };

        if partial {
            self.record(Fault::PartialPage);
        }
        if replayed {
            self.record(Fault::Replay);
        }
        if fault == Some(Fault::LostResponse) {
            self.record(Fault::LostResponse);
            return Err(SyncFailure::LostResponse);
        }
        Ok(Page { items, next, sync })
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Server, SyncFailure};
    use crate::fault::{Fault, FaultPlan};

    fn ids<B>(page: &super::Page<B>) -> Vec<&str> {
        page.items.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn cursor_round_trips() {
        let cursor = Cursor {
            epoch: 3,
            full: false,
            since: 9,
            upto: Some(12),
            after: Some("a.b/c d".to_owned()),
        };
        assert_eq!(Cursor::decode(&cursor.encode()), Some(cursor));
        assert_eq!(Cursor::decode("garbage"), None);
        assert_eq!(Cursor::decode("1.i.2.-.zz"), None);
    }

    #[test]
    fn pages_cover_a_collection_that_changes_mid_pass() {
        let mut server = Server::new(FaultPlan::none());
        for id in ["a", "b", "c", "d"] {
            server.upsert(id, ());
        }
        let first = server.sync(None, Some(2)).unwrap();
        assert_eq!(ids(&first), ["a", "b"]);
        // A change during the pass: "c" moves past the pass, "e" arrives after it started.
        server.upsert("c", ());
        server.upsert("e", ());
        let second = server.sync(first.next.as_deref(), Some(2)).unwrap();
        assert_eq!(ids(&second), ["d"]);
        let token = second.sync.unwrap();
        let third = server.sync(Some(&token), None).unwrap();
        assert_eq!(ids(&third), ["c", "e"]);
    }

    #[test]
    fn full_pass_omits_tombstones_and_incremental_reports_them() {
        let mut server = Server::new(FaultPlan::none());
        server.upsert("a", ());
        server.upsert("b", ());
        server.remove("b");
        let full = server.sync(None, None).unwrap();
        assert_eq!(ids(&full), ["a"]);
        server.remove("a");
        let incremental = server.sync(full.sync.as_deref(), None).unwrap();
        assert_eq!(ids(&incremental), ["a"]);
        assert!(incremental.items[0].1.deleted);
    }

    #[test]
    fn token_expiry_invalidates_every_token() {
        let plan = FaultPlan::none().at_call(1, Fault::TokenExpired);
        let mut server = Server::<()>::new(plan);
        let token = server.sync(None, None).unwrap().sync.unwrap();
        assert_eq!(
            server.sync(Some(&token), None).unwrap_err(),
            SyncFailure::TokenExpired
        );
        assert_eq!(
            server.sync(Some(&token), None).unwrap_err(),
            SyncFailure::TokenExpired
        );
        assert!(server.sync(None, None).is_ok());
    }

    #[test]
    fn partial_page_always_makes_progress() {
        let mut server = Server::new(FaultPlan::seeded(5).with_rate(Fault::PartialPage, 1000));
        for id in ["a", "b", "c", "d", "e"] {
            server.upsert(id, ());
        }
        let mut seen = Vec::new();
        let mut token = None;
        loop {
            let page = server.sync(token.as_deref(), None).unwrap();
            assert!(!page.items.is_empty());
            seen.extend(ids(&page).into_iter().map(str::to_owned));
            match page.next {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, ["a", "b", "c", "d", "e"]);
    }
}
