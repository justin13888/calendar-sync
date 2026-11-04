//! A fake `CalDAV` calendar collection.
//!
//! It follows RFC 6578 (`references/rfc/rfc6578.txt`) for `sync-collection`: the initial report
//! lists existing members only, later reports list removed members with a `404` status, a
//! truncated report marks the collection `507 Insufficient Storage` and its sync token continues
//! from what it delivered, and an invalid token fails the `DAV:valid-sync-token` precondition.
//! Writes follow RFC 4791 and RFC 9110: the client chooses the href, and `PUT` and `DELETE` take
//! `If-Match` on the `ETag`, or `If-None-Match: *` to create only. Bodies are opaque text.

use crate::fault::{CallKind, Fault, FaultPlan, InjectedFault};
use crate::server::{Server, SyncFailure};

/// A precondition on a `PUT`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precondition {
    /// None: create or replace unconditionally.
    None,
    /// `If-Match`: replace only this version.
    IfMatch(String),
    /// `If-None-Match: *`: create only.
    IfNoneMatch,
}

/// A `sync-collection` report request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncCollection {
    /// The `DAV:sync-token` of an earlier report; `None` for an initial sync.
    pub sync_token: Option<String>,
    /// `DAV:limit`, the most members to report; `None` for no limit.
    pub limit: Option<usize>,
}

/// One member in a `sync-collection` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncResponse {
    /// The member was created or changed, and has this `ETag`.
    Changed {
        /// The member's href.
        href: String,
        /// The member's `ETag`.
        etag: String,
    },
    /// The member was removed (status `404`).
    Removed {
        /// The member's href.
        href: String,
    },
}

/// A `sync-collection` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReport {
    /// The members reported.
    pub responses: Vec<SyncResponse>,
    /// The `DAV:sync-token` to send next.
    pub sync_token: String,
    /// The report was truncated (`507` on the collection): send `sync_token` again for the rest.
    pub truncated: bool,
}

/// A member's `ETag` and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The member's href.
    pub href: String,
    /// The `ETag`, a quoted string unique to this version.
    pub etag: String,
    /// The calendar object's text.
    pub body: String,
}

/// A failed call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// `403 Forbidden` with `DAV:valid-sync-token`: the token is not valid. The client syncs again
    /// without one.
    InvalidSyncToken,
    /// `404 Not Found`.
    NotFound,
    /// `412 Precondition Failed`: `If-Match` or `If-None-Match` failed.
    PreconditionFailed,
    /// The response was lost. The call may have taken effect.
    LostResponse,
}

impl Error {
    /// The HTTP status the error stands for, or `None` for a lost response.
    #[must_use]
    pub fn status(self) -> Option<u16> {
        match self {
            Self::InvalidSyncToken => Some(403),
            Self::NotFound => Some(404),
            Self::PreconditionFailed => Some(412),
            Self::LostResponse => None,
        }
    }
}

/// A fake `CalDAV` calendar collection.
#[derive(Clone, Debug)]
pub struct FakeCalDav {
    server: Server<String>,
}

fn etag(seq: u64) -> String {
    format!("\"{seq}\"")
}

impl FakeCalDav {
    /// An empty collection that injects faults by `plan`.
    #[must_use]
    pub fn new(plan: FaultPlan) -> Self {
        Self {
            server: Server::new(plan),
        }
    }

    /// `REPORT` `DAV:sync-collection`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidSyncToken`] and [`Error::LostResponse`].
    pub fn sync_collection(&mut self, request: &SyncCollection) -> Result<SyncReport, Error> {
        let page = self
            .server
            .sync(request.sync_token.as_deref(), request.limit)
            .map_err(|failure| match failure {
                SyncFailure::TokenExpired => Error::InvalidSyncToken,
                SyncFailure::LostResponse => Error::LostResponse,
            })?;
        let truncated = page.next.is_some();
        Ok(SyncReport {
            responses: page
                .items
                .into_iter()
                .map(|(href, resource)| {
                    if resource.deleted {
                        SyncResponse::Removed { href }
                    } else {
                        SyncResponse::Changed {
                            href,
                            etag: etag(resource.seq),
                        }
                    }
                })
                .collect(),
            sync_token: page.next.or(page.sync).unwrap_or_default(),
            truncated,
        })
    }

    /// `GET` a member.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] and [`Error::LostResponse`].
    pub fn get(&mut self, href: &str) -> Result<Member, Error> {
        let fault = self.server.begin(CallKind::Read);
        let result = self
            .server
            .live(href)
            .map(|resource| Member {
                href: href.to_owned(),
                etag: etag(resource.seq),
                body: resource.body.clone(),
            })
            .ok_or(Error::NotFound);
        self.lose(fault, result)
    }

    /// `PUT` a member, returning its new `ETag`. A deleted href may be written again.
    ///
    /// # Errors
    ///
    /// [`Error::PreconditionFailed`] and [`Error::LostResponse`].
    pub fn put(
        &mut self,
        href: &str,
        precondition: &Precondition,
        body: String,
    ) -> Result<String, Error> {
        let fault = self.server.begin(CallKind::Write);
        if fault == Some(Fault::PreconditionFailed) {
            // Another client writes the member first.
            let raced = match precondition {
                Precondition::None => false,
                Precondition::IfMatch(_) => self.server.touch(href).is_some(),
                Precondition::IfNoneMatch => {
                    self.server.live(href).is_none() && {
                        self.server.upsert(href, body.clone());
                        true
                    }
                }
            };
            if raced {
                self.server.record(Fault::PreconditionFailed);
            }
        }
        let current = self.server.live(href).map(|resource| etag(resource.seq));
        let allowed = match (precondition, &current) {
            (Precondition::None, _) | (Precondition::IfNoneMatch, None) => true,
            (Precondition::IfMatch(tag), Some(current)) => tag == current,
            (Precondition::IfMatch(_), None) | (Precondition::IfNoneMatch, Some(_)) => false,
        };
        let result = if allowed {
            Ok(etag(self.server.upsert(href, body)))
        } else {
            Err(Error::PreconditionFailed)
        };
        self.lose(fault, result)
    }

    /// `DELETE` a member, conditional on `if_match` when given.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::PreconditionFailed`], and [`Error::LostResponse`].
    pub fn delete(&mut self, href: &str, if_match: Option<&str>) -> Result<(), Error> {
        let fault = self.server.begin(CallKind::Write);
        if fault == Some(Fault::PreconditionFailed)
            && if_match.is_some()
            && self.server.touch(href).is_some()
        {
            self.server.record(Fault::PreconditionFailed);
        }
        let result = match self.server.live(href) {
            None => Err(Error::NotFound),
            Some(resource) if if_match.is_some_and(|tag| tag != etag(resource.seq)) => {
                Err(Error::PreconditionFailed)
            }
            Some(_) => {
                self.server.remove(href);
                Ok(())
            }
        };
        self.lose(fault, result)
    }

    /// Another client creates or replaces a member. Never faulted.
    pub fn remote_put(&mut self, href: &str, body: String) {
        self.server.upsert(href, body);
    }

    /// Another client deletes a member, if it exists. Never faulted.
    pub fn remote_delete(&mut self, href: &str) {
        self.server.remove(href);
    }

    /// Invalidate every sync token issued so far.
    pub fn expire_tokens(&mut self) {
        self.server.expire_tokens();
    }

    /// The members that exist now, in href order, for assertions. Never faulted.
    #[must_use]
    pub fn members(&self) -> Vec<Member> {
        self.server
            .live_resources()
            .map(|(href, resource)| Member {
                href: href.clone(),
                etag: etag(resource.seq),
                body: resource.body.clone(),
            })
            .collect()
    }

    /// The faults injected so far.
    #[must_use]
    pub fn faults(&self) -> &[InjectedFault] {
        self.server.faults()
    }

    /// The number of calls served so far.
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.server.calls()
    }

    fn lose<T>(&mut self, fault: Option<Fault>, result: Result<T, Error>) -> Result<T, Error> {
        if fault == Some(Fault::LostResponse) {
            self.server.record(Fault::LostResponse);
            return Err(Error::LostResponse);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, FakeCalDav, Precondition, SyncCollection, SyncResponse};
    use crate::fault::{Fault, FaultPlan};

    #[test]
    fn truncated_report_continues_from_its_token() {
        let mut dav = FakeCalDav::new(FaultPlan::none());
        for href in ["/cal/a.ics", "/cal/b.ics", "/cal/c.ics"] {
            dav.remote_put(href, String::new());
        }
        let first = dav
            .sync_collection(&SyncCollection {
                sync_token: None,
                limit: Some(2),
            })
            .unwrap();
        assert!(first.truncated);
        assert_eq!(first.responses.len(), 2);
        let rest = dav
            .sync_collection(&SyncCollection {
                sync_token: Some(first.sync_token),
                limit: Some(2),
            })
            .unwrap();
        assert!(!rest.truncated);
        assert_eq!(rest.responses.len(), 1);
    }

    #[test]
    fn removed_members_are_reported_after_the_initial_sync() {
        let mut dav = FakeCalDav::new(FaultPlan::none());
        dav.remote_put("/cal/a.ics", String::new());
        dav.remote_put("/cal/b.ics", String::new());
        dav.remote_delete("/cal/b.ics");
        let initial = dav.sync_collection(&SyncCollection::default()).unwrap();
        assert_eq!(initial.responses.len(), 1);
        dav.remote_delete("/cal/a.ics");
        let next = dav
            .sync_collection(&SyncCollection {
                sync_token: Some(initial.sync_token),
                limit: None,
            })
            .unwrap();
        assert_eq!(
            next.responses,
            [SyncResponse::Removed {
                href: "/cal/a.ics".to_owned()
            }]
        );
    }

    #[test]
    fn invalid_token_fails_the_precondition() {
        let mut dav = FakeCalDav::new(FaultPlan::none());
        let error = dav
            .sync_collection(&SyncCollection {
                sync_token: Some("http://example.com/sync/10".to_owned()),
                limit: None,
            })
            .unwrap_err();
        assert_eq!(error, Error::InvalidSyncToken);
        assert_eq!(error.status(), Some(403));
    }

    #[test]
    fn injected_token_expiry_fails_the_precondition() {
        let plan = FaultPlan::none().at_call(1, Fault::TokenExpired);
        let mut dav = FakeCalDav::new(plan);
        let request = SyncCollection {
            sync_token: Some(
                dav.sync_collection(&SyncCollection::default())
                    .unwrap()
                    .sync_token,
            ),
            limit: None,
        };
        assert_eq!(
            dav.sync_collection(&request).unwrap_err(),
            Error::InvalidSyncToken
        );
        assert_eq!(
            dav.sync_collection(&request).unwrap_err(),
            Error::InvalidSyncToken
        );
        assert_eq!(dav.faults()[0].fault, Fault::TokenExpired);
        assert!(dav.sync_collection(&SyncCollection::default()).is_ok());
    }

    #[test]
    fn replay_reports_a_member_already_reported() {
        let plan = FaultPlan::none().at_call(1, Fault::Replay);
        let mut dav = FakeCalDav::new(plan);
        dav.remote_put("/cal/a.ics", String::new());
        let initial = dav.sync_collection(&SyncCollection::default()).unwrap();
        let next = dav
            .sync_collection(&SyncCollection {
                sync_token: Some(initial.sync_token),
                limit: None,
            })
            .unwrap();
        assert_eq!(next.responses, initial.responses);
        assert_eq!(dav.faults()[0].fault, Fault::Replay);
    }

    #[test]
    fn partial_page_truncates_the_report() {
        let plan = FaultPlan::none().at_call(0, Fault::PartialPage);
        let mut dav = FakeCalDav::new(plan);
        let hrefs = ["/cal/a.ics", "/cal/b.ics", "/cal/c.ics", "/cal/d.ics"];
        for href in hrefs {
            dav.remote_put(href, String::new());
        }
        let mut report = dav.sync_collection(&SyncCollection::default()).unwrap();
        assert!(report.truncated && report.responses.len() < hrefs.len());
        assert_eq!(dav.faults()[0].fault, Fault::PartialPage);
        let mut seen = std::mem::take(&mut report.responses);
        while report.truncated {
            report = dav
                .sync_collection(&SyncCollection {
                    sync_token: Some(report.sync_token),
                    limit: None,
                })
                .unwrap();
            seen.append(&mut report.responses);
        }
        let seen: Vec<String> = seen
            .into_iter()
            .map(|response| match response {
                SyncResponse::Changed { href, .. } | SyncResponse::Removed { href } => href,
            })
            .collect();
        assert_eq!(seen, hrefs);
    }

    #[test]
    fn lost_response_still_applies_the_put() {
        let plan = FaultPlan::none().at_call(0, Fault::LostResponse);
        let mut dav = FakeCalDav::new(plan);
        assert_eq!(
            dav.put("/cal/a.ics", &Precondition::IfNoneMatch, "x".to_owned())
                .unwrap_err(),
            Error::LostResponse
        );
        assert_eq!(dav.get("/cal/a.ics").unwrap().body, "x");
        assert_eq!(
            dav.put("/cal/a.ics", &Precondition::IfNoneMatch, "x".to_owned())
                .unwrap_err(),
            Error::PreconditionFailed
        );
        assert_eq!(dav.faults()[0].fault, Fault::LostResponse);
    }

    #[test]
    fn create_only_put_loses_a_race() {
        let plan = FaultPlan::none().at_call(0, Fault::PreconditionFailed);
        let mut dav = FakeCalDav::new(plan);
        assert_eq!(
            dav.put("/cal/a.ics", &Precondition::IfNoneMatch, "x".to_owned())
                .unwrap_err(),
            Error::PreconditionFailed
        );
        let member = dav.get("/cal/a.ics").unwrap();
        let etag = dav
            .put(
                "/cal/a.ics",
                &Precondition::IfMatch(member.etag.clone()),
                "y".to_owned(),
            )
            .unwrap();
        assert_ne!(etag, member.etag);
        assert_eq!(
            dav.delete("/cal/a.ics", Some(&member.etag)).unwrap_err(),
            Error::PreconditionFailed
        );
        dav.delete("/cal/a.ics", Some(&etag)).unwrap();
        assert_eq!(dav.get("/cal/a.ics").unwrap_err(), Error::NotFound);
    }

    #[test]
    fn stale_retry_resurrects_a_deleted_member() {
        let mut dav = FakeCalDav::new(FaultPlan::none());
        dav.put("/cal/a.ics", &Precondition::IfNoneMatch, "x".to_owned())
            .unwrap();
        dav.remote_delete("/cal/a.ics");
        assert!(
            dav.put("/cal/a.ics", &Precondition::IfNoneMatch, "x".to_owned())
                .is_ok()
        );
        assert_eq!(dav.members().len(), 1);
    }
}
