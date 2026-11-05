//! Package D integration seam. Package A owns authenticated spawnd I/O and must
//! call issue before StartRun, attach only on matching TakeIo, and revoke before
//! Cancel/terminal status/control disconnect. This module is not startup-enabled.
//! The binary injects CLI's Dispatcher adapter; gateway never depends on CLI.
pub use duduclaw_mcp_boundary::{bindings, Boundary, GrantPolicy, LiveAuthority, LiveSnapshot,
    Reservation, VerificationError};
pub use duduclaw_mcp_boundary::server::Dispatcher;
pub use duduclaw_mcp_boundary::config::{PreparedRuntimeBridge, RuntimeBridgeRegistry};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use duduclaw_isolation::{capability::CapabilityId, execution::RunHandle};

    /// Own for the complete gateway epoch; dropping revokes every pending/active
    /// capability. No registry, socket listener or on-disk bearer is created.
    pub struct McpBoundaryService {
        boundary: Arc<Boundary>,
        sweep: tokio::task::JoinHandle<()>,
    }
    impl McpBoundaryService {
        pub fn start(live: Arc<dyn LiveAuthority>) -> Result<Self, VerificationError> {
            let boundary = Boundary::new(live)?;
            let sweeping = boundary.clone();
            let sweep = tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let result = duduclaw_mcp_boundary::authority::now_seconds()
                        .and_then(|now| sweeping.sweep(now));
                    if result.is_err() { let _ = sweeping.revoke_all(); break; }
                }
            });
            Ok(Self { boundary, sweep })
        }

        pub fn boundary(&self) -> &Arc<Boundary> { &self.boundary }

        /// A calls this after reserve and before issue/spawn. B publishes the native MCP
        /// projection and proves the registered runtime preserves fd 3 into its
        /// MCP child. Missing acceptance or any stale readback rejects the run;
        /// callers must not reuse legacy config or retry under the gateway uid.
        pub fn prepare_runtime(&self, registry: &dyn RuntimeBridgeRegistry,
            runtime: &duduclaw_isolation::execution::RuntimeKind, run: RunHandle,
            employee: duduclaw_isolation::ids::EmployeeId)
            -> Result<PreparedRuntimeBridge, VerificationError> {
            duduclaw_mcp_boundary::config::prepare_runtime_bridge(registry, runtime, run, employee)
        }

        /// `delivery` must be the McpGateway fd in the validated TakeIo response
        /// for `reservation.run`, received on `reservation.control`. Never call
        /// this with employee-supplied metadata or a pathname-connected socket.
        pub fn attach(&self, capability: CapabilityId, reservation: &Reservation,
            delivery: duduclaw_mcp_boundary::socket::McpDelivery, dispatcher: Arc<dyn Dispatcher>)
            -> Result<McpRunSession, VerificationError> {
            let connection = self.boundary.activate(capability, reservation, delivery,
                duduclaw_mcp_boundary::authority::now_seconds()?)?;
            let task = tokio::spawn(async move {
                duduclaw_mcp_boundary::server::serve(connection, dispatcher.as_ref()).await
            });
            Ok(McpRunSession { boundary: self.boundary.clone(), run: reservation.run, task })
        }
    }
    impl Drop for McpBoundaryService {
        fn drop(&mut self) {
            let _ = self.boundary.revoke_all();
            self.sweep.abort();
        }
    }

    /// Package A retains this guard for the run. Dropping it revokes first and
    /// aborts the connection task; dropping a bare JoinHandle would detach it.
    pub struct McpRunSession {
        boundary: Arc<Boundary>,
        run: RunHandle,
        task: tokio::task::JoinHandle<Result<(), VerificationError>>,
    }
    impl McpRunSession {
        pub async fn finish(mut self) -> Result<(), VerificationError> {
            (&mut self.task).await.map_err(|_| VerificationError::Unavailable)?
        }
    }
    impl Drop for McpRunSession {
        fn drop(&mut self) {
            let _ = self.boundary.revoke_run(self.run);
            self.task.abort();
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::{McpBoundaryService, McpRunSession};
