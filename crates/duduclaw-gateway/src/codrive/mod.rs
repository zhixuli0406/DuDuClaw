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
//! - [`identity`] — CD-2: resolves + authorizes the `agent`-parameter
//!   identity override for `codrive_run` ([`identity::resolve_run_identity`]).
//! - [`mode`] — A2: the closed driving-mode / handover-reason enums comp
//!   reports ([`mode::CodriveDrivingMode`]). Parsing only — comp owns the
//!   one state machine; this side never re-derives it.
//! - `plan_approval` — DESIGN §3.5: the opt-in session-level approval card
//!   `driver.rs` files before connecting when `[codrive] plan_approval`.
//! - [`status`] — A2: the read-only `codrive_status` query
//!   ([`status::query_status`]) behind the MCP tool of the same name.
//! - [`registry`] — CD-4/WP-CD4a: the C-L2 third-party app API/CLI/D-Bus
//!   action registry ([`registry::dispatch`]), tried by `step.rs` ahead of
//!   the ordinary C-L1 coordinate dispatch.
//! - [`atspi_locate`] — CD-4/WP-CD4b: the C-L3 AT-SPI2 semantic-layer
//!   locator ([`atspi_locate::locate`]), tried by `step.rs` one rung below
//!   `registry` — resolves a step's `locate` role/name query into screen
//!   coordinates that override the step's own literal `action` x/y before
//!   the unchanged C-L1 coordinate dispatch runs. WP-CD4b-fix (B3): it
//!   takes the session's [`client::CodriveClient`] because a real screen
//!   coordinate needs BOTH halves — AT-SPI's window-relative offset and
//!   comp's answer to "where is that window", asked over the new read-only
//!   `window_geometry` op. See that module's doc for the GTK4
//!   `CoordType::Screen`-returns-zeros defect this closes.
//!
//! `tests.rs`/`driver.rs` are both already near this project's per-file size
//! convention (200-400 lines typical, 800 max) — new codrive test scenarios
//! should open their own `tests_<topic>.rs` (see `tests_identity.rs` for the
//! pattern) rather than growing either further; new *driver* logic that
//! doesn't fit `step.rs`'s existing split should get its own module the same
//! way `identity.rs` did, not be appended to `driver.rs`.

pub mod atspi_locate;
pub mod client;
pub mod config;
pub mod driver;
pub mod identity;
pub mod mode;
mod plan_approval;
pub mod registry;
pub mod script;
pub mod status;
mod step;

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
mod tests_identity;

#[cfg(all(test, unix))]
mod tests_takeover;

#[cfg(all(test, unix))]
mod tests_registry;

#[cfg(all(test, unix))]
mod tests_atspi_locate;

#[cfg(all(test, unix))]
mod tests_mode;

/// Permanent `#[ignore]` live-bridge harness against the real comp
/// container stack — see its module doc for the playbook.
#[cfg(all(test, unix))]
mod live_tests;

pub use atspi_locate::{locate as atspi_locate_dispatch, LocateOutcome as AtspiLocateOutcome};
pub use client::{
    CodriveAck, CodriveButton, CodriveButtonState, CodriveClient, CodriveClientError, CodriveCmd,
    CodriveEvent,
};
pub use config::CodriveConfig;
pub use driver::{CodriveRunReport, CodriveStepReport, run_script};
pub use mode::{CodriveDrivingMode, CodriveHandoverReason};
pub use status::{
    query_status as query_codrive_status, CodriveDrivingState, CodriveStatusReport,
};
pub use identity::{resolve_run_identity, RunIdentityError};
pub use registry::{dispatch as registry_dispatch, DispatchOutcome as RegistryDispatchOutcome};
pub use script::{
    ApiActionRequest, CodriveAction, CodriveConsequential, CodriveHighlight, CodriveScript,
    CodriveStep, ConsequentialClass, LocateRequest,
};
