//! Human-Machine Co-Drive (人機共駕) — CD-1 gateway driver.
//!
//! Runs a scripted GUI co-drive session against the `duduclaw-comp`
//! compositor's agent-injection socket. This crate never depends on
//! `duduclaw-comp` directly — it's a Linux-only, detached workspace member
//! (see the root `Cargo.toml` `exclude` list) — so the wire types in
//! [`client`] are a hand-mirrored copy of comp's socket protocol, locked
//! down by the serde shape tests at the bottom of that file. Do not import
//! `duduclaw_comp` from this module.
//!
//! Design authority: `commercial/docs/DESIGN-codrive-desktop-2026-08.md`
//! §3.2 (execution ladder), §3.4 (approval/guardrails reuse), §5 (CD-1
//! row), §6 (safety red lines), §8-3 (refuse-list). This module is the
//! CD-1 gateway-side driver: agent-seat injection over the comp socket,
//! ApprovalBroker-gated consequential steps, freeze/resume retry, and
//! emergency-stop handling.
//!
//! Module layout:
//! - [`config`] — `[codrive]` config.toml section ([`config::CodriveConfig`]).
//! - [`client`] — long-lived Unix-socket client speaking the comp wire
//!   protocol ([`client::CodriveClient`]).
//! - [`script`] — the MCP-facing script schema ([`script::CodriveScript`])
//!   and its structural validation/sanitization.
//! - [`driver`] — orchestrates one script run end to end
//!   ([`driver::run_script`]): refuse-list, approval gate, freeze/resume,
//!   emergency stop, and the activity-feed ticker.

pub mod client;
pub mod config;
pub mod driver;
pub mod script;
mod step;

#[cfg(all(test, unix))]
mod tests;

/// Permanent `#[ignore]` live-bridge harness against the real comp
/// container stack — see its module doc for the playbook.
#[cfg(all(test, unix))]
mod live_tests;

pub use client::{
    CodriveAck, CodriveButton, CodriveButtonState, CodriveClient, CodriveClientError, CodriveCmd,
    CodriveEvent,
};
pub use config::CodriveConfig;
pub use driver::{CodriveRunReport, CodriveStepReport, run_script};
pub use script::{
    CodriveAction, CodriveConsequential, CodriveHighlight, CodriveScript, CodriveStep,
    ConsequentialClass,
};
