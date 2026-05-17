//! `duduclaw-cli-worker` — standalone PTY-pool worker.
//!
//! Exposes the cross-platform [`duduclaw_cli_runtime::PtyPool`] as a small
//! HTTP+JSON-RPC service bound to `127.0.0.1`. The DuDuClaw gateway speaks
//! to it over localhost, optionally spawning the worker as a managed child
//! process or pointing at an externally-launched instance.
//!
//! Design lineage:
//! - **Phase 5** = worker process (this crate's binary).
//! - **Phase 6** = JSON-RPC IPC layer (this crate's library + protocol).
//! - **Phase 7** (deferred) = gateway-side lifecycle supervision.
//!
//! The library is exported so the gateway can reuse the same protocol
//! types and the integration tests can spin up an in-process server.

pub mod auth;
pub mod client;
pub mod protocol;
pub mod server;

pub use auth::TokenStore;
pub use client::{ClientError, WorkerClient};
pub use protocol::{
    HEALTHZ_PATH, InvokeParams, RPC_PATH, Request, Response, RpcError, RpcResult,
    ShutdownSessionParams, StatsResult,
};
pub use server::{ServerError, ServerHandle, WorkerServer, WorkerServerConfig};
