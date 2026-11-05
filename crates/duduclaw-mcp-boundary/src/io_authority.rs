//! Carry verified isolation authority through legacy async adapters to the
//! precise write/commit point. Host/operator calls outside a scope retain their
//! existing authorization. Spawned tasks do not inherit Tokio task locals: an
//! adapter that spawns must explicitly wrap the child future in `scope` too.
use std::future::Future;
use crate::{RequestContext, VerificationError};

tokio::task_local! {
    static ACTIVE: RequestContext;
}

pub async fn scope<F: Future>(context: RequestContext, future: F) -> F::Output {
    ACTIVE.scope(context, future).await
}

/// Captured request authority for child futures and blocking workers. Capturing
/// outside an isolation request preserves the existing host/operator behavior.
#[derive(Clone)]
pub struct CapturedAuthority(Option<RequestContext>);

/// Used only to narrow transport behavior for isolated calls (no ambient proxy).
pub fn is_scoped() -> bool { ACTIVE.try_with(|_| ()).is_ok() }

pub fn capture() -> CapturedAuthority {
    CapturedAuthority(ACTIVE.try_with(Clone::clone).ok())
}

impl CapturedAuthority {
    pub fn is_scoped(&self) -> bool { self.0.is_some() }
    pub fn verify(&self) -> Result<(), VerificationError> {
        match &self.0 {
            Some(context) => context.verify_live(crate::authority::now_seconds()?),
            None => Ok(()),
        }
    }

    /// Install the captured authority on the thread executing a blocking job.
    /// The previous scope is restored on return or panic.
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        match &self.0 {
            Some(context) => ACTIVE.sync_scope(context.clone(), work),
            None => work(),
        }
    }

    /// Install the captured authority for a spawned async child. Dropping the
    /// parent never detaches the live verifier held by this token.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        match &self.0 {
            Some(context) => scope(context.clone(), future).await,
            None => future.await,
        }
    }
}

/// Call after any lock/approval await, immediately before an irreversible I/O
/// or transaction commit. Failure before commit lets the transaction roll back.
/// A scoped request with missing/unavailable/revoked authority fails closed.
pub fn verify_before_io() -> Result<(), VerificationError> {
    ACTIVE.try_with(|context| context.verify_live(crate::authority::now_seconds()?))
        .unwrap_or(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, task::Poll};
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, LiveRequestVerifier, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};

    struct Live(Arc<AtomicBool>);
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
        }
    }
    fn context(live: Arc<AtomicBool>) -> RequestContext {
        RequestContext::with_live_verifier(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
            nonce: [0; 32], jti: [0; 16],
        }, Arc::new(Live(live)))
    }

    #[tokio::test]
    async fn revoked_while_waiting_for_lock_performs_no_effect() {
        let live = Arc::new(AtomicBool::new(true));
        let lock = tokio::sync::Mutex::new(());
        let held = lock.lock().await;
        let effects = AtomicUsize::new(0);
        let request = scope(context(live.clone()), async {
            verify_before_io()?;
            let _guard = lock.lock().await;
            verify_before_io()?;
            effects.fetch_add(1, Ordering::SeqCst);
            Ok::<_, VerificationError>(())
        });
        tokio::pin!(request);
        std::future::poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        live.store(false, Ordering::SeqCst);
        drop(held);
        assert_eq!(request.await, Err(VerificationError::Revoked));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert!(verify_before_io().is_ok(), "isolated authority must not leak to host calls");
    }

    #[tokio::test]
    async fn captured_authority_survives_parent_scope_and_checks_blocking_work() {
        let live = Arc::new(AtomicBool::new(true));
        let captured = scope(context(live.clone()), async { capture() }).await;
        live.store(false, Ordering::SeqCst);
        let blocking = captured.clone();
        let result = tokio::task::spawn_blocking(move || blocking.run(verify_before_io)).await.unwrap();
        assert_eq!(result, Err(VerificationError::Revoked));
        assert_eq!(captured.scope(async { verify_before_io() }).await, Err(VerificationError::Revoked));
        assert!(verify_before_io().is_ok());
    }

    #[tokio::test]
    async fn captured_blocking_scope_restores_after_panic() {
        let live = Arc::new(AtomicBool::new(false));
        let captured = scope(context(live), async { capture() }).await;
        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| captured.run(|| {
                assert_eq!(verify_before_io(), Err(VerificationError::Revoked));
                panic!("test scope restoration");
            })));
            assert!(result.is_err());
            assert!(verify_before_io().is_ok());
        }).await.unwrap();
    }

    #[tokio::test]
    async fn missing_verifier_is_refused_and_scope_restores_after_error() {
        let detached = RequestContext::from_verified_record(context(Arc::new(AtomicBool::new(true))).record().clone());
        assert_eq!(scope(detached, async { verify_before_io() }).await, Err(VerificationError::Unavailable));
        assert!(verify_before_io().is_ok());
    }

    #[tokio::test]
    async fn live_scoped_and_unscoped_host_writes_remain_allowed() {
        assert!(verify_before_io().is_ok());
        assert!(scope(context(Arc::new(AtomicBool::new(true))), async {
            tokio::task::yield_now().await;
            verify_before_io()
        }).await.is_ok());
    }
}
