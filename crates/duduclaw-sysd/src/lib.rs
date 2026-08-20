//! `duduclaw-sysd` — the DuDuClaw appliance image's privilege-separated
//! root system service.
//!
//! See `commercial/docs/DESIGN-agent-os-native-apps-2026-08.md` §2 L1 for
//! the design context (internal doc, not part of this public crate — this
//! crate's own docs are self-contained). In short: the DuDuClaw gateway
//! process runs as an unprivileged `duduclaw` user and has no business
//! holding root — but a handful of operations (reboot, poweroff, applying
//! a `systemd-sysupdate` OS image, re-arming first-boot provisioning for a
//! factory reset, changing the hostname) genuinely require root. Rather
//! than run the whole gateway as root, or grant it broad sudo/polkit
//! rules, this crate is a small standalone root daemon exposing exactly
//! six fixed verbs over a Unix domain socket gated by peer-credential
//! (`SO_PEERCRED`) verification — the gateway is the only caller that can
//! ever reach it, and even the gateway can only ever ask for one of the
//! six fixed things, never an arbitrary command.
//!
//! Module map:
//! - [`protocol`] — the wire schema (closed request enum, response
//!   envelope, socket-path/allowed-uid env resolution). Shared by both
//!   halves of the socket.
//! - [`dispatch`] — verb → hardcoded `Command::new(...)` shell-out. Only
//!   reached AFTER a request is both peer-cred-authorized and successfully
//!   parsed against the closed enum.
//! - [`server`] — UDS bind + accept loop + per-connection peer-cred check
//!   + audit tracing. Runs as root in production (see `main.rs`).
//! - [`client`] — the gateway-side (or test-side) caller SDK.

pub mod client;
pub mod dispatch;
pub mod protocol;
pub mod server;

pub use client::{SysdClient, SysdClientError};
pub use protocol::{
    ALLOWED_UID_ENV, DEFAULT_SOCKET_PATH, SOCKET_PATH_ENV, SysdError, SysdOpOutput, SysdRequest,
    SysdResponse, resolve_socket_path,
};
pub use server::{SysdServerConfig, bind, serve};
