//! Registers host authority between A's reservation and D's capability issuance.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use duduclaw_execution::command::{NativeCommandSnapshot, ProtectedLaunch};
use duduclaw_isolation::{bounded::Text, execution::*, ids::EmployeeId};
use duduclaw_mcp_boundary::launch_steps::{ActivateStep, PrepareStep, SessionStep};
use super::{ProtectedMcpLaunch, authority::{BindStep, GatewayAuthority, PreparedAuthority}};
use super::refusal::{site, RefusalSite};

#[path = "launch_sites.rs"]
mod sites;

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
    /// R8z-3: the MCP-boundary launch (`ProtectedMcpLaunch::prepare_checked`)
    /// refused at this check.
    InnerPrepare(PrepareStep),
}

impl LaunchStep {
    /// The fixed `step` log value. R8z-3: an `InnerPrepare` step is its site
    /// label without the `launch.bind.` prefix (`inner_prepare.<substep>`),
    /// a slice of that `&'static str` literal.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PreparedMissing => "prepared_missing",
            Self::StateLock => "state_lock",
            Self::RevokedBefore => "revoked_before",
            Self::Bind(step) => step.as_str(),
            Self::RevokedAfter => "revoked_after",
            Self::VerifyBlocking => "verify_blocking",
            Self::InnerPrepare(_) => self.site().as_str().strip_prefix("launch.bind.").unwrap_or("inner_prepare"),
        }
    }

    /// The per-turn refusal site: `launch.bind.<step>`.
    pub(super) fn site(self) -> RefusalSite {
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
            Self::InnerPrepare(step) => sites::prepare(step),
        }
    }
}

/// R8z-3: the fixed check at which `ProtectedMcpLaunch::activate_checked`
/// refused (after StartRun, before the run's MCP endpoint is served).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ActivationStep(pub(super) ActivateStep);

impl ActivationStep {
    /// The per-turn refusal site: `launch.activate.inner_activate.<substep>`.
    pub(super) fn site(self) -> RefusalSite { sites::activate(self.0) }
    /// The fixed `step` log value: the site label without `launch.activate.`
    /// (`inner_activate.<substep>`), a slice of that `&'static str` literal.
    pub(super) fn as_str(self) -> &'static str {
        self.site().as_str().strip_prefix("launch.activate.").unwrap_or("inner_activate")
    }
}

/// R8z-2: the one line for a refused launch preparation (fixed `step` only),
/// and the step as this turn's refusal site. `prepare` is polled inline by
/// `duduclaw_execution::command::protected` on the turn's own task, so the
/// turn trace is in scope here (outside a turn, `note_site` is a no-op). The
/// blocking-pool job never logs or records: it returns its step to here.
/// R8z-3: the MCP-boundary layer does not log either; it returns its step
/// (`prepare_checked`) and this line is the only one for it.
fn log_refused(step: LaunchStep) {
    tracing::warn!(step = step.as_str(), "isolated launch preparation refused");
}

fn refused(step: LaunchStep, error: BackendError) -> BackendError {
    log_refused(step);
    super::runtime::note_site(step.site());
    error
}

/// R8z-3: the one line for a refused activation (fixed `step` only). Like
/// `prepare`, `activate` is awaited inline by `duduclaw_execution::command`
/// on the turn's own task, so the site reaches this turn's trace.
fn log_activation_refused(step: ActivationStep) {
    tracing::warn!(step = step.as_str(), "isolated launch activation refused");
}

fn activation_refused(step: ActivationStep, error: BackendError) -> BackendError {
    log_activation_refused(step);
    super::runtime::note_site(step.site());
    error
}

/// R8z-5: the one line for a run's MCP session that ended abnormally before
/// the run was released (fixed `step` only: `SessionStep::as_str`). The
/// boundary calls this from the session's own detached task, once; a normal
/// end or the run's release logs nothing. The turn's trace (task-local to
/// the turn's task) is not reachable from that task, so no site is recorded;
/// the turn still logs its own `isolated run refused` line.
pub(super) fn log_session_ended(step: SessionStep) {
    tracing::warn!(step = step.as_str(), "isolated MCP session ended");
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
            // R8z-3: the same preparation as `inner.prepare` (which ignores
            // the native snapshot), with the check that refused.
            let _ = native;
            match self.inner.prepare_checked(run, class, runtime, pty).await {
                Ok(policy) => {
                    lease.armed = false;
                    Ok(policy)
                }
                Err((step, error)) => Err(refused(LaunchStep::InnerPrepare(step), error)),
            }
        })
    }
    fn activate(&self, run: RunHandle, mcp: Option<std::os::fd::OwnedFd>) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            // R8z-3: `inner.activate` with the check that refused; the error
            // and every side effect (a dropped pending guard revokes the
            // capability; the caller cancels the started run) are unchanged.
            self.inner.activate_checked(run, mcp).await
                .map_err(|(step, error)| activation_refused(ActivationStep(step), error))
        })
    }
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.revoked.store(true, Ordering::Release);
            // R8z-5: dropping the association below fails the session's next
            // live check; that is the release, not an abnormal session end.
            self.inner.mark_released(run);
            self.authority.revoke_run(run);
            self.inner.revoke(run).await
        })
    }
}
impl Drop for AuthorizedLaunch {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::Release);
        if let Some(run) = self.run.get_mut().unwrap_or_else(|error| error.into_inner()).take() {
            self.inner.mark_released(run);
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

    fn every_step() -> Vec<LaunchStep> {
        let mut steps = vec![LaunchStep::PreparedMissing, LaunchStep::StateLock, LaunchStep::RevokedBefore,
            LaunchStep::RevokedAfter, LaunchStep::VerifyBlocking];
        steps.extend(BindStep::ALL.map(LaunchStep::Bind));
        steps.extend(PrepareStep::every().into_iter().map(LaunchStep::InnerPrepare));
        steps
    }

    fn every_activation() -> Vec<ActivationStep> { ActivateStep::every().into_iter().map(ActivationStep).collect() }

    /// R8z-3: every MCP-boundary step maps to the listed site whose label is
    /// the step's own segments (`launch_steps`) under its fixed prefix, so
    /// the boundary's structure and this crate's literals cannot drift.
    #[test]
    fn r8z3_every_boundary_step_maps_to_its_listed_site() {
        let mut seen = std::collections::BTreeSet::new();
        for step in PrepareStep::every() {
            let site = LaunchStep::InnerPrepare(step).site().as_str();
            assert_eq!(site, format!("launch.bind.inner_prepare.{}", step.label()), "{step:?}");
            assert_eq!(LaunchStep::InnerPrepare(step).as_str(), format!("inner_prepare.{}", step.label()));
            assert!(seen.insert(site), "duplicate site {site}");
        }
        for step in every_activation() {
            let site = step.site().as_str();
            assert_eq!(site, format!("launch.activate.inner_activate.{}", step.0.label()), "{step:?}");
            assert_eq!(step.as_str(), format!("inner_activate.{}", step.0.label()));
            assert!(super::super::refusal::SITES.contains(&site), "{site}");
            assert!(seen.insert(site), "duplicate site {site}");
        }
        assert_eq!(seen.len(), 61 + 83);
        // Every listed `launch.bind.inner_prepare.*` / `launch.activate.*`
        // site is reachable from exactly one step (no orphan literal).
        let listed = super::super::refusal::SITES.iter()
            .filter(|site| site.starts_with("launch.bind.inner_prepare.") || site.starts_with("launch.activate."))
            .count();
        assert_eq!(listed, seen.len());
        use duduclaw_mcp_boundary::launch_steps::{AcceptanceCheck, BridgeStep, PublishStep, ReceiptCheck, RegistrationStep};
        assert_eq!(LaunchStep::InnerPrepare(PrepareStep::Registry(RegistrationStep::Acceptance(AcceptanceCheck::RuntimeDigest)))
            .as_str(), "inner_prepare.registry.acceptance.runtime_digest");
        assert_eq!(ActivationStep(ActivateStep::Bridge(BridgeStep::Publish(PublishStep::Receipt(ReceiptCheck::Mount)))).site().as_str(),
            "launch.activate.inner_activate.bridge.publish.receipt.mount");
    }

    /// R8z-3: one activation line per refusal, fixed `step` field only.
    #[test]
    fn r8z3_the_activation_refused_line_has_the_fixed_step_field_only() {
        for step in every_activation() {
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_target(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, || log_activation_refused(step));
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("WARN") && logs.contains("isolated launch activation refused"), "{logs}");
            assert!(logs.trim_end().ends_with(&format!("refused step=\"{}\"", step.as_str())), "{logs}");
            assert_eq!(logs.matches('=').count(), 1, "{logs}");
        }
    }

    /// R8z-5: one session-end line per abnormal end, fixed `step` field only,
    /// for every session step (a distinct literal each).
    #[test]
    fn r8z5_the_session_ended_line_has_the_fixed_step_field_only() {
        let mut seen = std::collections::BTreeSet::new();
        for step in SessionStep::every() {
            assert!(seen.insert(step.as_str()), "duplicate step {}", step.as_str());
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_target(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, || log_session_ended(step));
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("WARN") && logs.contains("isolated MCP session ended"), "{logs}");
            assert!(logs.trim_end().ends_with(&format!("ended step=\"{}\"", step.as_str())), "{logs}");
            assert_eq!(logs.matches('=').count(), 1, "{logs}");
        }
    }

    /// R8z-3: a refused preparation and a refused activation each record
    /// their own site before the outer `spawn.backend.*` mapping.
    #[tokio::test]
    async fn r8z3_inner_refusals_record_their_sites() {
        use duduclaw_mcp_boundary::launch_steps::{AcceptanceCheck, RegistrationStep};
        let step = LaunchStep::InnerPrepare(PrepareStep::Registry(RegistrationStep::Acceptance(AcceptanceCheck::Version)));
        let site = super::super::runtime::tests::site_of(async move {
            assert_eq!(refused(step, BackendError::Denied), BackendError::Denied);
            super::super::runtime::note_site(site!("spawn.backend.denied"));
        }).await;
        assert_eq!(site, Some("launch.bind.inner_prepare.registry.acceptance.version"));
        let step = ActivationStep(ActivateStep::McpReceipt(BackendError::Unavailable));
        let site = super::super::runtime::tests::site_of(async move {
            assert_eq!(activation_refused(step, BackendError::Unavailable), BackendError::Unavailable);
            super::super::runtime::note_site(site!("spawn.backend.unavailable"));
        }).await;
        assert_eq!(site, Some("launch.activate.inner_activate.mcp_receipt.unavailable"));
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
        assert_eq!(LaunchStep::InnerPrepare(PrepareStep::Reservation(BackendError::Denied)).as_str(),
            "inner_prepare.reservation.denied");
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
