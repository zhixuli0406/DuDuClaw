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
pub mod recording_layout;
pub mod update;
pub mod worker;

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
