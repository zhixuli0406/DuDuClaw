//! Package D protected boot composition. Package A owns authenticated spawnd I/O and must
//! call issue before StartRun, attach only on matching TakeIo, and revoke before
//! Cancel/terminal status/control disconnect. Boot refuses absent trusted sources.
//! The binary injects CLI's Dispatcher adapter; gateway never depends on CLI.
pub use duduclaw_mcp_boundary::{bindings, Boundary, GrantPolicy, LiveAuthority, LiveSnapshot,
    Reservation, VerificationError};
pub use duduclaw_mcp_boundary::server::Dispatcher;
pub use duduclaw_mcp_boundary::config::{PreparedRuntimeBridge, RuntimeBridgeRegistry};
/// Reads the gateway's existing live employee registry without caching an allow.
pub type AgentAdmission = dyn Fn(&str) -> Result<(), VerificationError> + Send + Sync;
#[cfg(target_os = "linux")]
pub use duduclaw_mcp_boundary::protected_launch::ProtectedMcpLaunch;

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
                    let checking = sweeping.clone();
                    let result = duduclaw_mcp_boundary::authority::verify_blocking(move ||
                        duduclaw_mcp_boundary::authority::now_seconds().and_then(|now| checking.sweep(now))).await;
                    if result.is_err() { let _ = sweeping.revoke_all(); break; }
                }
            });
            Ok(Self { boundary, sweep })
        }

        pub fn boundary(&self) -> &Arc<Boundary> { &self.boundary }
        pub fn healthy(&self) -> bool { !self.sweep.is_finished() }

        /// Verify after StartRun/TakeIo, when B's per-run mount receipt exists.
        /// B publishes the native MCP projection and supplies exact-runtime
        /// acceptance that fd 3 reaches its MCP child. Missing or stale readback rejects the run;
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
            dispatcher.require_complete_services()?;
            let connection = self.boundary.activate(capability, reservation, delivery,
                duduclaw_mcp_boundary::authority::now_seconds()?)?;
            let context = connection.context(duduclaw_mcp_boundary::authority::now_seconds()?)?;
            dispatcher.require_context_services(&context)?;
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

#[cfg(target_os = "linux")]
#[path = "isolation_mcp/authority.rs"]
mod authority;
#[cfg(target_os = "linux")]
pub use authority::HostTurn;
#[cfg(target_os = "linux")]
#[path = "isolation_mcp/launch.rs"]
mod launch;
#[cfg(target_os = "linux")]
#[path = "isolation_mcp/boot.rs"]
mod boot;
#[cfg(target_os = "linux")]
#[path = "isolation_mcp/ingress.rs"]
pub(crate) mod ingress;
#[cfg(target_os = "linux")]
pub(crate) use boot::a_producers_ready;
#[cfg(target_os = "linux")]
pub(crate) use boot::control_recording;
#[cfg(target_os = "linux")]
#[path = "isolation_mcp/runtime.rs"]
pub(crate) mod runtime;
#[cfg(target_os = "linux")]
pub use runtime::{RuntimeAuthorization, with_runtime_authorization};
#[cfg(target_os = "linux")]
pub(crate) use boot::{human_authorization as record_authorization, delegation_source};
#[cfg(target_os = "linux")]
pub use boot::{BootSources, BootStatus, MissingProducer, DispatcherFactory, DispatcherInput, LaunchInput,
    install_boot_sources, install_dispatcher_factory, boot_protected, boot_status, prepare_launch};
#[cfg(not(target_os = "linux"))]
pub async fn boot_protected(_: &std::path::Path, _: std::sync::Arc<duduclaw_auth::UserDb>,
    _: std::sync::Arc<AgentAdmission>) {}
