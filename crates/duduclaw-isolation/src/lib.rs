//! Phase-zero contracts for native Linux employee isolation (design r7, §3–§17).
//! No OS operations, cryptography, persistent state or authorization live here.
#![forbid(unsafe_code)]

pub mod bounded;
pub mod capability;
pub mod egress;
pub mod execution;
pub mod ids;
pub mod ipc;
pub mod migration;
pub mod probe_exit;
pub mod recording_layout;
pub mod ssh_config;
pub mod update;
pub mod worker;

/// Shared employee-storage reserve policy for installation and runtime admission.
pub const STORAGE_MIN_RESERVE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const STORAGE_RESERVE_DIVISOR: u64 = 10;

/// Round up without overflowing, including at u64::MAX.
pub fn storage_reserve_bytes(total: u64) -> u64 {
    STORAGE_MIN_RESERVE_BYTES.max(total / STORAGE_RESERVE_DIVISOR
        + u64::from(total % STORAGE_RESERVE_DIVISOR != 0))
}

/// The highest Landlock ABI whose rule set this release knows (plan Q4). The
/// single source for every consumer: the trampoline's policy clamp
/// (`duduclaw-run` selects `min(detected, this)`), spawnd's evidence check,
/// the recording sandbox, the gateway's readiness notice and the installer's
/// composite readiness check. Raising it requires the matching rule set.
pub const LANDLOCK_HIGHEST_KNOWN_ABI: u32 = 7;

/// Fail-closed structural errors; never include input, paths or secrets (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// A typed value is invalid.
    #[error("invalid contract value: {0}")]
    InvalidValue(&'static str),
    /// A hard protocol bound was exceeded.
    #[error("contract limit exceeded: {0}")]
    LimitExceeded(&'static str),
    /// No edge exists in the lifecycle table.
    #[error("illegal lifecycle transition")]
    IllegalTransition,
    /// No compatible wire version exists.
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    /// Length, schema or ancillary data does not match the contract.
    #[error("invalid protocol frame")]
    InvalidFrame,
}
