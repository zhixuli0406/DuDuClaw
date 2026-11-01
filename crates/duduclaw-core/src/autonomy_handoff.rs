//! Host-installed admission at transport boundaries, once per outbound call.
//! Task-local hooks keep human requests unchanged and avoid a gateway dependency.
use crate::autonomy_gate::AdmissionGuard;
use std::{future::Future, pin::Pin, sync::Arc};

pub type AdmissionFuture =
    Pin<Box<dyn Future<Output = Result<Option<AdmissionGuard>, String>> + Send>>;

#[derive(Clone)]
struct Hooks {
    admit: Arc<dyn Fn() -> AdmissionFuture + Send + Sync>,
    handed_off: Arc<dyn Fn() + Send + Sync>,
}

tokio::task_local! { static HOOKS: Hooks; }

pub fn scope<F: Future>(
    admit: fn() -> AdmissionFuture,
    handed_off: fn(),
    future: F,
) -> impl Future<Output = F::Output> {
    scope_owned(admit, handed_off, future)
}

/// Owned hooks let hosts capture identity instead of relying on task-locals in
/// detached transports. The callbacks must retain the initiating authority.
pub fn scope_owned<F: Future>(
    admit: impl Fn() -> AdmissionFuture + Send + Sync + 'static,
    handed_off: impl Fn() + Send + Sync + 'static,
    future: F,
) -> impl Future<Output = F::Output> {
    // Keep the transport future out of every enclosing task-local frame.
    // Boxing inside an async fn would still embed F in its initial state.
    HOOKS.scope(
        Hooks {
            admit: Arc::new(admit),
            handed_off: Arc::new(handed_off),
        },
        Box::pin(future),
    )
}

#[derive(Clone)]
pub struct CapturedScope(Hooks);

pub fn capture() -> Option<CapturedScope> {
    HOOKS.try_with(|hooks| CapturedScope(hooks.clone())).ok()
}

pub fn restore<F: Future>(
    scope: Option<CapturedScope>,
    future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        match scope {
            Some(scope) => HOOKS.scope(scope.0, future).await,
            None => future.await,
        }
    }
}

/// Must be acquired immediately before send/spawn, never around a tool loop.
pub async fn admit() -> Result<HandoffGuard, String> {
    let hooks = HOOKS.try_with(Clone::clone).ok();
    let admission = match &hooks {
        Some(hooks) => (hooks.admit)().await?,
        None => None,
    };
    Ok(HandoffGuard {
        admission,
        hooks,
        request_started: false,
    })
}

pub struct HandoffGuard {
    admission: Option<AdmissionGuard>,
    hooks: Option<Hooks>,
    request_started: bool,
}

impl HandoffGuard {
    /// A failed/cancelled HTTP send may already have reached the server. Keep
    /// the lease until send ends, but never later replay it as an unstarted turn.
    pub fn request_started(&mut self) {
        self.request_started = true;
    }

    /// Call after successful spawn or response headers, before waiting/reading.
    /// On failure/cancellation normal drop also releases the admission lease.
    pub fn handed_off(mut self) {
        if let Some(hooks) = &self.hooks {
            (hooks.handed_off)();
        }
        self.request_started = false;
        drop(self.admission.take());
    }
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        if self.request_started
            && let Some(hooks) = &self.hooks
        {
            (hooks.handed_off)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn large_future() {
        let payload = [0u8; 64 * 1024];
        std::future::pending::<()>().await;
        std::hint::black_box(payload);
    }

    #[test]
    fn autonomy_scopes_bound_transport_future_size() {
        fn admission() -> AdmissionFuture {
            Box::pin(async { Ok(None) })
        }
        fn assert_small(future: impl Future<Output = ()>) {
            assert!(std::mem::size_of_val(&future) < 4096);
        }
        assert!(std::mem::size_of_val(&large_future()) >= 64 * 1024);
        assert_small(scope(admission, || {}, large_future()));
        assert_small(scope_owned(admission, || {}, large_future()));
        assert_small(restore(None, large_future()));
    }
}
