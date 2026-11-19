//! Registers host authority between A's reservation and D's capability issuance.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use duduclaw_execution::command::{NativeCommandSnapshot, ProtectedLaunch};
use duduclaw_isolation::{bounded::Text, execution::*, ids::EmployeeId};
use super::{ProtectedMcpLaunch, authority::{BindStep, GatewayAuthority, PreparedAuthority}};
use super::refusal::{site, RefusalSite};

/// R8z-2: the fixed step at which [`AuthorizedLaunch::prepare`] gave up.
/// Every refusal of that preparation is exactly one of these; the returned
/// `BackendError` is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaunchStep {
    /// The one-shot prepared authority was already taken (second prepare).
    PreparedMissing,
    /// A launch state mutex was poisoned.
    StateLock,
    /// The launch was revoked before the binding ran.
    RevokedBefore,
    /// A check of the gateway's run binding (`GatewayAuthority::bind_run`).
    Bind(BindStep),
    /// The launch was revoked while or right after the binding ran.
    RevokedAfter,
    /// The blocking-pool wrapper itself failed (2 s slot wait, 10 s job
    /// limit, or the job panicked).
    VerifyBlocking,
    /// The MCP-boundary launch (`ProtectedMcpLaunch::prepare`) refused with
    /// this backend error class.
    InnerPrepare(BackendError),
}

impl LaunchStep {
    /// The fixed `step` log value.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::PreparedMissing => "prepared_missing",
            Self::StateLock => "state_lock",
            Self::RevokedBefore => "revoked_before",
            Self::Bind(step) => step.as_str(),
            Self::RevokedAfter => "revoked_after",
            Self::VerifyBlocking => "verify_blocking",
            Self::InnerPrepare(error) => match error {
                BackendError::Unavailable => "inner_prepare.unavailable",
                BackendError::Denied => "inner_prepare.denied",
                BackendError::WouldDeadlock => "inner_prepare.would_deadlock",
                BackendError::NotFound => "inner_prepare.not_found",
                BackendError::Timeout => "inner_prepare.timeout",
                BackendError::Io => "inner_prepare.io",
                BackendError::Protocol => "inner_prepare.protocol",
            },
        }
    }

    /// The per-turn refusal site: `launch.bind.<step>`.
    pub(super) const fn site(self) -> RefusalSite {
        match self {
            Self::PreparedMissing => site!("launch.bind.prepared_missing"),
            Self::StateLock => site!("launch.bind.state_lock"),
            Self::RevokedBefore => site!("launch.bind.revoked_before"),
            Self::Bind(step) => match step {
                BindStep::RecheckBackground => site!("launch.bind.recheck.background"),
                BindStep::RecheckExpired => site!("launch.bind.recheck.expired"),
                BindStep::RecheckAgent => site!("launch.bind.recheck.agent"),
                BindStep::RecheckIdentity => site!("launch.bind.recheck.identity"),
                BindStep::RecheckPolicyRevision => site!("launch.bind.recheck.policy_revision"),
                BindStep::RecheckRevisionChanged => site!("launch.bind.recheck.revision_changed"),
                BindStep::RecheckEpoch => site!("launch.bind.recheck.epoch"),
                BindStep::LiveSnapshot => site!("launch.bind.live_snapshot"),
                BindStep::BindingMismatch => site!("launch.bind.binding_mismatch"),
                BindStep::EpochAfterSnapshot => site!("launch.bind.epoch_after_snapshot"),
                BindStep::Associations => site!("launch.bind.associations"),
            },
            Self::RevokedAfter => site!("launch.bind.revoked_after"),
            Self::VerifyBlocking => site!("launch.bind.verify_blocking"),
            Self::InnerPrepare(error) => match error {
                BackendError::Unavailable => site!("launch.bind.inner_prepare.unavailable"),
                BackendError::Denied => site!("launch.bind.inner_prepare.denied"),
                BackendError::WouldDeadlock => site!("launch.bind.inner_prepare.would_deadlock"),
                BackendError::NotFound => site!("launch.bind.inner_prepare.not_found"),
                BackendError::Timeout => site!("launch.bind.inner_prepare.timeout"),
                BackendError::Io => site!("launch.bind.inner_prepare.io"),
                BackendError::Protocol => site!("launch.bind.inner_prepare.protocol"),
            },
        }
    }
}

/// R8z-2: the one line for a refused launch preparation (fixed `step` only),
/// and the step as this turn's refusal site. `prepare` is polled inline by
/// `duduclaw_execution::command::protected` on the turn's own task, so the
/// turn trace is in scope here (outside a turn, `note_site` is a no-op). The
/// blocking-pool job never logs or records: it returns its step to here.
fn log_refused(step: LaunchStep) {
    tracing::warn!(step = step.as_str(), "isolated launch preparation refused");
}

fn refused(step: LaunchStep, error: BackendError) -> BackendError {
    log_refused(step);
    super::runtime::note_site(step.site());
    error
}

pub(super) struct AuthorizedLaunch {
    inner: Arc<ProtectedMcpLaunch>,
    authority: Arc<GatewayAuthority>,
    prepared: Mutex<Option<PreparedAuthority>>,
    run: Mutex<Option<RunHandle>>,
    revoked: Arc<AtomicBool>,
}
impl AuthorizedLaunch {
    pub(super) fn new(inner: Arc<ProtectedMcpLaunch>, authority: Arc<GatewayAuthority>,
        prepared: PreparedAuthority) -> Self {
        Self { inner, authority, prepared: Mutex::new(Some(prepared)), run: Mutex::new(None), revoked: Arc::new(AtomicBool::new(false)) }
    }
}
impl ProtectedLaunch for AuthorizedLaunch {
    fn employee(&self) -> EmployeeId { self.inner.employee() }
    fn task(&self) -> Option<Text<128>> { self.inner.task() }
    fn wait_origin(&self) -> WaitOrigin { self.inner.wait_origin() }
    fn caller_employee(&self) -> Option<EmployeeId> { self.inner.caller_employee() }
    fn prepare<'a>(&'a self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind,
        native: &'a NativeCommandSnapshot, pty: bool) -> BackendFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            let prepared = self.prepared.lock()
                .map_err(|_| refused(LaunchStep::StateLock, BackendError::Unavailable))?
                .take().ok_or_else(|| refused(LaunchStep::PreparedMissing, BackendError::Denied))?;
            // Record ownership before registering: cancellation or unwinding must
            // still erase the run even if preparation never returns a plan.
            *self.run.lock().map_err(|_| refused(LaunchStep::StateLock, BackendError::Unavailable))? = Some(run);
            let authority = self.authority.clone();
            let revoked = self.revoked.clone();
            // The job's own refusal is its step (`Ok(Err(step))`); an `Err` is
            // only ever the blocking-pool wrapper's.
            let bound = duduclaw_mcp_boundary::authority::verify_blocking(move || {
                Ok((|| {
                    if revoked.load(Ordering::Acquire) { return Err(LaunchStep::RevokedBefore); }
                    authority.bind_run(run, prepared).map_err(|refusal| LaunchStep::Bind(refusal.step))?;
                    let lease = BindingLease { authority, run, armed: true };
                    if revoked.load(Ordering::Acquire) { return Err(LaunchStep::RevokedAfter); }
                    Ok(lease)
                })())
            }).await;
            let mut lease = match bound {
                Ok(Ok(lease)) => lease,
                Ok(Err(step)) => return Err(refused(step, BackendError::Denied)),
                Err(_) => return Err(refused(LaunchStep::VerifyBlocking, BackendError::Denied)),
            };
            if self.revoked.load(Ordering::Acquire) { return Err(refused(LaunchStep::RevokedAfter, BackendError::Denied)); }
            match self.inner.prepare(run, class, runtime, native, pty).await {
                Ok(policy) => {
                    lease.armed = false;
                    Ok(policy)
                }
                Err(error) => Err(refused(LaunchStep::InnerPrepare(error), error)),
            }
        })
    }
    fn activate(&self, run: RunHandle, mcp: Option<std::os::fd::OwnedFd>) -> BackendFuture<'_, ()> {
        self.inner.activate(run, mcp)
    }
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.revoked.store(true, Ordering::Release);
            self.authority.revoke_run(run);
            self.inner.revoke(run).await
        })
    }
}
impl Drop for AuthorizedLaunch {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::Release);
        if let Some(run) = self.run.get_mut().unwrap_or_else(|error| error.into_inner()).take() {
            self.authority.revoke_run(run);
        }
    }
}

// The blocking job owns this lease until the async caller accepts it. A dropped
// join result or a cancelled prepare therefore cannot leave a late binding.
struct BindingLease { authority: Arc<GatewayAuthority>, run: RunHandle, armed: bool }
impl Drop for BindingLease {
    fn drop(&mut self) { if self.armed { self.authority.revoke_run(self.run); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::refusal::site;

    const ERRORS: [BackendError; 7] = [BackendError::Unavailable, BackendError::Denied, BackendError::WouldDeadlock,
        BackendError::NotFound, BackendError::Timeout, BackendError::Io, BackendError::Protocol];

    fn every_step() -> Vec<LaunchStep> {
        let mut steps = vec![LaunchStep::PreparedMissing, LaunchStep::StateLock, LaunchStep::RevokedBefore,
            LaunchStep::RevokedAfter, LaunchStep::VerifyBlocking];
        steps.extend(BindStep::ALL.map(LaunchStep::Bind));
        steps.extend(ERRORS.map(LaunchStep::InnerPrepare));
        steps
    }

    /// R8z-2: every step has a distinct fixed label, and its turn site is
    /// exactly `launch.bind.<label>` (a listed `refusal::SITES` entry).
    #[test]
    fn r8z2_launch_steps_are_fixed_and_map_to_launch_bind_sites() {
        let steps = every_step();
        let labels: std::collections::BTreeSet<_> = steps.iter().map(|step| step.as_str()).collect();
        assert_eq!(labels.len(), steps.len(), "duplicate step label");
        for step in &steps {
            let label = step.as_str();
            assert!(!label.is_empty() && label.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{label}");
            assert_eq!(step.site().as_str(), format!("launch.bind.{label}"), "{step:?}");
            assert!(super::super::refusal::SITES.contains(&step.site().as_str()), "{label}");
        }
        assert_eq!(LaunchStep::Bind(BindStep::RecheckPolicyRevision).as_str(), "recheck.policy_revision");
        assert_eq!(LaunchStep::InnerPrepare(BackendError::Denied).as_str(), "inner_prepare.denied");
        assert_eq!(LaunchStep::VerifyBlocking.site().as_str(), "launch.bind.verify_blocking");
    }

    #[derive(Clone)]
    struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    /// R8z-2: one line per refusal, with the fixed message and the fixed
    /// `step` field and nothing else (no run id, agent, path or error text).
    #[test]
    fn r8z2_the_refused_line_has_the_fixed_step_field_only() {
        for step in every_step() {
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_target(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, || log_refused(step));
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("WARN") && logs.contains("isolated launch preparation refused"), "{logs}");
            assert!(logs.trim_end().ends_with(&format!("refused step=\"{}\"", step.as_str())), "{logs}");
            // `step` is the only field.
            assert_eq!(logs.matches('=').count(), 1, "{logs}");
        }
    }

    /// R8z-2: the step reaches the turn's trace (first recorded wins, so the
    /// outer `spawn.backend.denied` mapping no longer hides it).
    #[tokio::test]
    async fn r8z2_a_refused_preparation_records_its_launch_bind_site() {
        let site = super::super::runtime::tests::site_of(async {
            assert_eq!(refused(LaunchStep::Bind(BindStep::LiveSnapshot), BackendError::Denied), BackendError::Denied);
            super::super::runtime::note_site(site!("spawn.backend.denied"));
        }).await;
        assert_eq!(site, Some("launch.bind.live_snapshot"));
    }
}
