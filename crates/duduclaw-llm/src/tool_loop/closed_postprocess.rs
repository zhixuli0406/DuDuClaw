//! Call-local Closed validity and exact-call metadata cleanup, never credentials.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Clone)]
pub(super) struct WorkerState {
    cancelled: Arc<AtomicBool>,
    deadline: tokio::time::Instant,
}
impl WorkerState {
    pub(super) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) || tokio::time::Instant::now() >= self.deadline
    }
}
struct Cleanup {
    store: crate::ccr::CcrStore,
    scope: CcrScope,
    tool: String,
    call: String,
    state: WorkerState,
}
#[derive(Default)]
pub(super) struct ClosedPostprocess {
    #[cfg(feature = "mcp-client")]
    policy: Option<crate::mcp_client::ClosedPostprocessPolicy>,
    cleanup: Option<Cleanup>,
    invalid: AtomicBool,
}
impl ClosedPostprocess {
    #[cfg(feature = "mcp-client")]
    pub(super) fn adopt(
        &mut self,
        policy: Option<crate::mcp_client::ClosedPostprocessPolicy>,
        runtime: Option<&CcrRuntime>,
        tool: String,
        call: String,
    ) {
        if let Some(policy) = &policy {
            if let Some(runtime) = runtime {
                self.cleanup = Some(Cleanup {
                    store: runtime.store.clone(),
                    scope: runtime.scope.clone(),
                    tool,
                    call,
                    state: WorkerState {
                        cancelled: Arc::new(AtomicBool::new(false)),
                        deadline: policy.deadline(),
                    },
                });
            }
        }
        self.policy = policy;
    }
    pub(super) fn active(&self) -> bool {
        #[cfg(feature = "mcp-client")]
        {
            return self.policy.is_some();
        }
        #[cfg(not(feature = "mcp-client"))]
        false
    }
    pub(super) async fn metadata<F: std::future::Future<Output = Result<(), LlmError>>>(
        &self,
        future: F,
    ) -> Result<(), LlmError> {
        match self.wait(future).await {
            Ok(Err(error)) if self.active() => {
                self.invalid.store(true, Ordering::SeqCst);
                let _ = error;
                Ok(())
            }
            Ok(result) => result,
            Err(()) => Ok(()),
        }
    }
    pub(super) fn failed(&self) {
        if self.active() {
            self.invalid.store(true, Ordering::SeqCst);
        }
    }
    pub(super) fn stop(&self) -> bool {
        if self.invalid.load(Ordering::SeqCst) {
            return true;
        }
        #[cfg(feature = "mcp-client")]
        if let Some(policy) = &self.policy {
            return policy.expired() || policy.before_execute_refusal();
        }
        false
    }
    pub(super) fn message(&self) -> &'static str {
        #[cfg(feature = "mcp-client")]
        if let Some(policy) = &self.policy {
            return policy.failure_message();
        }
        "Closed MCP result unconfirmed; remote execution may have occurred"
    }
    pub(super) fn worker(&self) -> Option<WorkerState> {
        self.cleanup.as_ref().map(|c| c.state.clone())
    }
    pub(super) async fn wait<F: std::future::Future>(&self, future: F) -> Result<F::Output, ()> {
        #[cfg(feature = "mcp-client")]
        if let Some(policy) = &self.policy {
            return policy.wait(future).await;
        }
        Ok(future.await)
    }
    pub(super) fn retire_saved(&self, saved: &mut Vec<CcrSavedResult>) {
        if let Some(cleanup) = &self.cleanup {
            saved.retain(|item| {
                item.scope != cleanup.scope
                    || item.source_tool != cleanup.tool
                    || item.source_call_id != cleanup.call
            });
        }
    }
    pub(super) async fn recheck_metadata(
        &self,
        saved: Option<&CcrSavedResult>,
        runtime: Option<&CcrRuntime>,
    ) {
        let Some(cleanup) = &self.cleanup else {
            return;
        };
        if self.stop() {
            return;
        }
        let store = cleanup.store.clone();
        let scope = cleanup.scope.clone();
        let tool = cleanup.tool.clone();
        let call = cleanup.call.clone();
        let saved = saved.cloned();
        let runtime = runtime.cloned();
        let result = self
            .wait(tokio::task::spawn_blocking(move || {
                store.ensure_source_call_active(&scope, &tool, &call)?;
                if let Some(saved) = saved {
                    let Some(runtime) = runtime else {
                        return Ok(false);
                    };
                    return runtime.valid_saved_reference(
                        &saved.scope,
                        &saved.id,
                        &saved.source_tool,
                        &saved.source_call_id,
                        saved.original_bytes,
                    );
                }
                Ok(true)
            }))
            .await;
        if !matches!(result, Ok(Ok(Ok(true)))) {
            self.failed();
        }
    }
    pub(super) fn admit(&mut self) {
        self.cleanup = None;
    }
}
impl ClosedPostprocess {
    pub(super) fn retire(&mut self) {
        let Some(cleanup) = self.cleanup.as_ref() else {
            return;
        };
        if cleanup.state.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        // Retain the nonsecret exact binding for staged-handle retirement;
        // cancellation and Drop enqueue this finite metadata cleanup once.
        let store = cleanup.store.clone();
        let scope = cleanup.scope.clone();
        let tool = cleanup.tool.clone();
        let call = cleanup.call.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                if store.revoke_source_call(&scope, &tool, &call).is_err() {
                    tracing::error!("Closed CCR cancellation metadata cleanup failed");
                }
            });
        } else {
            tracing::error!("Closed CCR cancellation cleanup runtime unavailable");
        }
    }
}

impl Drop for ClosedPostprocess {
    fn drop(&mut self) {
        self.retire();
    }
}
