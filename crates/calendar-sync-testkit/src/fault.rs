//! Which faults a fake injects, and when.

use std::collections::BTreeMap;

use crate::rng::SplitMix64;

/// A failure a fake provider can inject.
///
/// Each fake turns a fault into its own protocol's form. A fault that cannot take effect on a
/// call, such as a short page when only one item remains, is not injected and not logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fault {
    /// Every sync token the fake has issued stops being valid, and the call fails: Google
    /// `410 Gone`, Graph `410 Gone` (`syncStateNotFound`), `CalDAV` `DAV:valid-sync-token`. The
    /// client must start a full sync again. Applies to a sync call that carries a token.
    TokenExpired,
    /// Another client changes the target first, so a conditional write fails with
    /// `412 Precondition Failed`. Applies to a write that carries a precondition on a resource
    /// that exists, or an `If-None-Match: *` create.
    PreconditionFailed,
    /// The page delivers again changes the client has already received, as Graph warns it may.
    /// Applies to a sync call when something was delivered before it.
    Replay,
    /// The page holds fewer items than it could, and the rest follow on later pages (`CalDAV`:
    /// a truncated `507` report). Applies to a sync call with more than one item left.
    PartialPage,
    /// The call takes effect on the server, but its response never reaches the client, which sees
    /// a transport error. Applies to every call.
    LostResponse,
}

impl Fault {
    /// Every fault, in the order a call draws them.
    pub const ALL: [Self; 5] = [
        Self::TokenExpired,
        Self::PreconditionFailed,
        Self::Replay,
        Self::PartialPage,
        Self::LostResponse,
    ];

    /// Whether this fault can apply to a call of `kind` at all.
    #[must_use]
    pub fn applies_to(self, kind: CallKind) -> bool {
        match self {
            Self::TokenExpired | Self::Replay | Self::PartialPage => kind == CallKind::Sync,
            Self::PreconditionFailed => kind == CallKind::Write,
            Self::LostResponse => true,
        }
    }
}

/// The kind of call a fake serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallKind {
    /// A page of a change listing: Google `events.list`, Graph `delta`, `CalDAV`
    /// `sync-collection`.
    Sync,
    /// A read of one resource.
    Read,
    /// A create, update or delete.
    Write,
}

/// How often each fault fires, and on which calls, from one seed.
///
/// Calls are numbered from zero in the order the fake receives them. A fault scripted for a call
/// fires on it if it applies to that call's kind; otherwise each fault fires at its rate, in the
/// order of [`Fault::ALL`], drawn from a generator seeded with `seed`. The same plan and the same
/// sequence of calls always inject the same faults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaultPlan {
    seed: u64,
    rates: BTreeMap<Fault, u16>,
    scripted: BTreeMap<u64, Fault>,
}

impl FaultPlan {
    /// A plan that injects nothing.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A plan drawing from `seed`, with every rate at zero.
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }

    /// Fire `fault` on `per_mille` of the calls it applies to. Values above 1000 count as 1000.
    #[must_use]
    pub fn with_rate(mut self, fault: Fault, per_mille: u16) -> Self {
        self.rates.insert(fault, per_mille.min(1000));
        self
    }

    /// Fire every fault on `per_mille` of the calls it applies to.
    #[must_use]
    pub fn with_all(mut self, per_mille: u16) -> Self {
        for fault in Fault::ALL {
            self = self.with_rate(fault, per_mille);
        }
        self
    }

    /// Fire `fault` on call number `call`, counted from zero, if it applies to that call.
    #[must_use]
    pub fn at_call(mut self, call: u64, fault: Fault) -> Self {
        self.scripted.insert(call, fault);
        self
    }

    /// The seed the plan draws from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }
}

/// A fault a fake injected, for assertions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InjectedFault {
    /// The call's number, counted from zero.
    pub call: u64,
    /// The kind of call.
    pub kind: CallKind,
    /// The fault.
    pub fault: Fault,
}

/// The running state of a [`FaultPlan`]: the call counter, the generator and the log.
#[derive(Clone, Debug)]
pub(crate) struct Injector {
    plan: FaultPlan,
    rng: SplitMix64,
    calls: u64,
    current: Option<(u64, CallKind)>,
    log: Vec<InjectedFault>,
}

impl Injector {
    pub(crate) fn new(plan: FaultPlan) -> Self {
        Self {
            rng: SplitMix64::new(plan.seed),
            plan,
            calls: 0,
            current: None,
            log: Vec::new(),
        }
    }

    /// Start a call of `kind`, and draw the fault it may suffer. The caller reports through
    /// [`Self::record`] whether the fault took effect.
    pub(crate) fn begin(&mut self, kind: CallKind) -> Option<Fault> {
        let call = self.calls;
        self.calls += 1;
        self.current = Some((call, kind));
        // Draw for every applicable fault on every call, so one fault's rate does not shift the
        // draws of the others.
        let mut drawn = None;
        for fault in Fault::ALL
            .into_iter()
            .filter(|fault| fault.applies_to(kind))
        {
            let rate = self.plan.rates.get(&fault).copied().unwrap_or(0);
            let roll = self.rng.below(1000);
            if drawn.is_none() && roll < u64::from(rate) {
                drawn = Some(fault);
            }
        }
        match self.plan.scripted.get(&call) {
            Some(&fault) if fault.applies_to(kind) => Some(fault),
            _ => drawn,
        }
    }

    /// Log that `fault` took effect on the current call.
    pub(crate) fn record(&mut self, fault: Fault) {
        if let Some((call, kind)) = self.current {
            self.log.push(InjectedFault { call, kind, fault });
        }
    }

    pub(crate) fn rng(&mut self) -> &mut SplitMix64 {
        &mut self.rng
    }

    pub(crate) fn log(&self) -> &[InjectedFault] {
        &self.log
    }

    pub(crate) fn calls(&self) -> u64 {
        self.calls
    }
}

#[cfg(test)]
mod tests {
    use super::{CallKind, Fault, FaultPlan, Injector};

    #[test]
    fn same_seed_draws_the_same_faults() {
        let plan = FaultPlan::seeded(42).with_all(300);
        let kinds = [CallKind::Sync, CallKind::Write, CallKind::Read];
        let draw = |plan: &FaultPlan| {
            let mut injector = Injector::new(plan.clone());
            (0..300)
                .map(|i| injector.begin(kinds[i % kinds.len()]))
                .collect::<Vec<_>>()
        };
        let first = draw(&plan);
        assert_eq!(first, draw(&plan));
        assert!(first.iter().any(Option::is_some));
        assert_ne!(first, draw(&FaultPlan::seeded(43).with_all(300)));
    }

    #[test]
    fn drawn_faults_apply_to_the_call() {
        let mut injector = Injector::new(FaultPlan::seeded(1).with_all(1000));
        for kind in [CallKind::Sync, CallKind::Read, CallKind::Write] {
            for _ in 0..50 {
                let fault = injector.begin(kind).expect("every rate is 1000");
                assert!(fault.applies_to(kind));
            }
        }
    }

    #[test]
    fn scripted_fault_fires_only_where_it_applies() {
        let plan = FaultPlan::none()
            .at_call(0, Fault::PreconditionFailed)
            .at_call(1, Fault::PreconditionFailed);
        let mut injector = Injector::new(plan);
        assert_eq!(injector.begin(CallKind::Sync), None);
        assert_eq!(
            injector.begin(CallKind::Write),
            Some(Fault::PreconditionFailed)
        );
        assert_eq!(injector.begin(CallKind::Write), None);
    }

    #[test]
    fn zero_rates_inject_nothing() {
        let mut injector = Injector::new(FaultPlan::seeded(9));
        assert!((0..100).all(|_| injector.begin(CallKind::Sync).is_none()));
    }
}
