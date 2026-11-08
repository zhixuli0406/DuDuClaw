//! Root trampoline, native Linux isolation package B2 (design r7 §4).
//! This crate never accepts a gateway-selected execution policy.
pub mod error;
pub mod fd_plan;
pub mod state;
pub mod validation;
pub mod seccomp;
pub mod landlock;
pub mod status;
pub mod helper_policy;
pub use error::{Error, Result};
#[cfg(target_os = "linux")]
pub mod linux;
