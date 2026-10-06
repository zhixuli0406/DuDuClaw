//! Dispatcher side of workflow queue messages. Each message runs in its own
//! task so a long run never holds up other employees' queue messages; the
//! run lease, not the dispatcher, keeps two executions of one run apart.
use super::{DispatchOutcome, WorkflowService, WorkflowStore};
use crate::message_queue::{MessageQueue, QueueMessage};
use std::{path::PathBuf, sync::Arc, sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;
use tracing::warn;

/// Workflow runs executing at once in this gateway.
const MAX_CONCURRENT_RUNS: usize = 4;
/// Re-ack interval while a run executes, well under the 60 s stale sweep.
const KEEPALIVE_SECONDS: u64 = 20;
/// Delay before a retry of a busy or infrastructure-failed dispatch.
const RETRY_DELAY_SECONDS: u64 = 15;

fn permits() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_RUNS)))
        .clone()
}

/// The queue row id namespace workflow handoffs use.
pub fn is_workflow_message(message: &QueueMessage) -> bool {
    message.sender == "workflow"
        && message
            .id
            .split_once(':')
            .is_some_and(|(namespace, _)| namespace == "workflow")
}

/// Hand an already-acked message to its own task.
pub fn spawn(queue: Arc<MessageQueue>, home: PathBuf, message: QueueMessage) {
    tokio::spawn(async move {
        let id = message.id.clone();
        let work = async {
            // Waiting for a slot also counts as in progress (keepalive below).
            let _permit = permits()
                .acquire_owned()
                .await
                .map_err(|_| "workflow dispatch slots closed".to_string())?;
            let store = Arc::new(WorkflowStore::open(&home)?);
            let broker = Arc::new(crate::approval::ApprovalBroker::open(&home)?);
            let service = WorkflowService::new(
                home.clone(),
                std::env::current_exe().map_err(|e| e.to_string())?,
                store,
                broker,
            )?;
            service.dispatch_queue_message_outcome(&message).await
        };
        tokio::pin!(work);
        // Keep the message fresh while the run waits or executes so the stale-message
        // sweep does not hand it out a second time.
        let outcome = loop {
            tokio::select! {
                outcome = &mut work => break outcome,
                _ = tokio::time::sleep(Duration::from_secs(KEEPALIVE_SECONDS)) => {
                    if let Err(e) = queue.ack(&id).await {
                        warn!(message_id = %id, error = %e, "workflow message keepalive failed");
                    }
                }
            }
        };
        let settled = match outcome {
            Ok(DispatchOutcome::Done(run)) => {
                queue
                    .complete(
                        &id,
                        &serde_json::json!({
                            "workflow_run_id": run.run_id,
                            "status": run.status,
                            "error_code": run.error_code
                        })
                        .to_string(),
                    )
                    .await
            }
            Ok(DispatchOutcome::Retry(reason)) => {
                tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECONDS)).await;
                tracing::info!(message_id = %id, reason = %reason, "workflow dispatch deferred");
                // R-L8: "the run is busy elsewhere" is a deferral, not an
                // attempt, and must not use up the transient retry budget.
                if reason == super::runner::WORKFLOW_BUSY {
                    queue.defer_to_pending(&id).await
                } else {
                    queue.reset_to_pending(&id).await
                }
            }
            Err(error) => queue.fail(&id, &error).await,
        };
        if let Err(e) = settled {
            warn!(message_id = %id, error = %e, "workflow queue message not settled");
        }
    });
}
