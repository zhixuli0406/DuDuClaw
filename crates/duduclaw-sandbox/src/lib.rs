//! Native OS process confinement for spawned agent CLI subprocesses.
//!
//! DuDuClaw already ships a *container*-based sandbox (`duduclaw-container`).
//! This crate is the complementary **in-process OS floor**: it takes a
//! [`std::process::Command`] that is about to spawn an agent CLI (codex / gemini
//! / antigravity / claude) and confines the resulting child process with a
//! native OS primitive — **macOS Seatbelt** (`sandbox-exec` + SBPL profile) and
//! **Linux Landlock** (`landlock` LSM, applied post-`fork` via `pre_exec`) — so
//! the child cannot write outside its workspace *without needing any container
//! runtime*.
//!
//! # Design invariants
//! - **Fail-closed** (I5): when a caller requires confinement and the OS
//!   primitive is unavailable or refuses, the spawn MUST be aborted, never
//!   downgraded to an unconfined run. Callers detect this via
//!   [`Confinement::Refused`] / [`SandboxError`].
//! - **unsafe / `#[cfg(target_os)]` isolation**: all `pre_exec` `unsafe` and the
//!   `landlock` dependency live in this crate only, keeping the gateway clean.
//! - **Deterministic profile generation**: the SBPL / ruleset planning is pure
//!   and unit-tested independently of any real spawn.
//!
//! # Platform status
//! | OS      | Primitive                         | Stage |
//! |---------|-----------------------------------|-------|
//! | macOS   | Seatbelt via `sandbox-exec -f`    | A — live-verifiable |
//! | Linux   | Landlock (`pre_exec` restrict)    | B — logic + Linux CI |
//! | Windows | none yet                          | C — `Unsupported` stub, fail-closed |

use std::path::{Path, PathBuf};
use std::process::Command;

pub use duduclaw_core::types::SandboxLevel;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod unsupported;

// The Landlock ruleset *plan* is a pure, OS-agnostic value so it can be
// unit-tested on any host (including this macOS dev box). The application of
// the plan lives in the linux module behind `cfg(target_os = "linux")`.
mod plan;
pub use plan::LandlockPlan;

/// Errors raised while confining a command. Every variant is a *deny* signal for
/// a caller that required confinement (fail-closed).
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// Rendering or writing the sandbox profile failed.
    #[error("sandbox profile error: {0}")]
    Profile(String),
    /// The requested primitive is not available on this host/kernel.
    #[error("sandbox primitive unavailable: {0}")]
    Unavailable(String),
    /// An OS call while applying the sandbox failed.
    #[error("sandbox io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Whether the platform's native sandbox primitive can actually enforce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Fully enforcing (macOS Seatbelt present; Linux Landlock ABI complete).
    Enforcing,
    /// Present but partial — some requested restrictions cannot be applied
    /// (e.g. an older Linux kernel without the network ABI). The `String`
    /// explains what degraded.
    Degraded(String),
    /// No native primitive on this platform (current Windows).
    Unsupported,
}

/// Outcome of a [`NativeSandbox::confine`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// The command was confined; it will run under the OS sandbox.
    Applied,
    /// Confinement intentionally skipped (a `FullAccess` grant — see
    /// [`SandboxSpec::unconfined`]). This is a *success*, not a failure.
    Skipped,
    /// Confinement was required but the primitive refused / is unsupported.
    /// Callers that required a sandbox MUST NOT spawn (fail-closed).
    Refused,
}

/// The concrete confinement request derived from a [`SandboxLevel`] and the
/// agent's working directory.
///
/// - `readable_paths` — subtrees the child may read (defaults to `/` so the CLI
///   and its dynamic libraries can load; the coarse [`SandboxLevel`] model
///   confines *writes*, not reads).
/// - `writable_paths` — the ONLY subtrees the child may write.
/// - `allow_network` — when `false`, the sandbox denies outbound network. The
///   `from_level` defaults keep this `true` because the agent CLI must reach its
///   model API; network egress confinement is delegated to the container
///   `--network=none` / egress-proxy layer. Operators can build a stricter spec
///   by hand.
/// - `unconfined` — set for a `FullAccess` grant; [`NativeSandbox::confine`]
///   returns [`Confinement::Skipped`] without touching the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    pub readable_paths: Vec<PathBuf>,
    pub writable_paths: Vec<PathBuf>,
    pub allow_network: bool,
    pub unconfined: bool,
}

impl SandboxSpec {
    /// Derive a spec from the coarse [`SandboxLevel`] and the agent directory.
    ///
    /// - `ReadOnly` → read `/`, write nothing.
    /// - `WorkspaceWrite` → read `/`, write `[agent_dir, temp_dir]`.
    /// - `FullAccess` → `unconfined` (intentional escape hatch; caller sees
    ///   [`Confinement::Skipped`]).
    pub fn from_level(level: SandboxLevel, agent_dir: &Path) -> SandboxSpec {
        match level {
            SandboxLevel::ReadOnly => SandboxSpec {
                readable_paths: vec![PathBuf::from("/")],
                writable_paths: Vec::new(),
                allow_network: true,
                unconfined: false,
            },
            SandboxLevel::WorkspaceWrite => SandboxSpec {
                readable_paths: vec![PathBuf::from("/")],
                writable_paths: vec![agent_dir.to_path_buf(), std::env::temp_dir()],
                allow_network: true,
                unconfined: false,
            },
            SandboxLevel::FullAccess => SandboxSpec {
                readable_paths: Vec::new(),
                writable_paths: Vec::new(),
                allow_network: true,
                unconfined: true,
            },
        }
    }
}

/// A native OS sandbox primitive that confines a to-be-spawned child process.
pub trait NativeSandbox: Send + Sync {
    /// Confine `cmd` in place per `spec`. Call this AFTER the command is fully
    /// built but BEFORE spawning.
    ///
    /// Returns:
    /// - [`Confinement::Applied`] — the command will run sandboxed.
    /// - [`Confinement::Skipped`] — `spec.unconfined` (FullAccess); no-op.
    /// - [`Confinement::Refused`] — required but unsupported (fail-closed).
    /// - `Err` — profile generation / OS error (fail-closed).
    fn confine(&self, cmd: &mut Command, spec: &SandboxSpec)
        -> Result<Confinement, SandboxError>;

    /// Report whether this primitive can enforce on the current host.
    fn availability(&self) -> Availability;
}

/// Select the sandbox implementation for the current OS.
pub fn platform_sandbox() -> Box<dyn NativeSandbox> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosSandbox::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxSandbox::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsSandbox::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Box::new(unsupported::UnsupportedSandbox::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_level_read_only_has_no_writable_paths() {
        let spec = SandboxSpec::from_level(SandboxLevel::ReadOnly, Path::new("/agents/bob"));
        assert!(spec.writable_paths.is_empty());
        assert_eq!(spec.readable_paths, vec![PathBuf::from("/")]);
        assert!(!spec.unconfined);
    }

    #[test]
    fn from_level_workspace_write_includes_agent_dir_and_tmp() {
        let agent = Path::new("/agents/bob");
        let spec = SandboxSpec::from_level(SandboxLevel::WorkspaceWrite, agent);
        assert!(spec.writable_paths.contains(&agent.to_path_buf()));
        assert!(spec.writable_paths.contains(&std::env::temp_dir()));
        assert!(!spec.unconfined);
    }

    #[test]
    fn from_level_full_access_is_unconfined() {
        let spec = SandboxSpec::from_level(SandboxLevel::FullAccess, Path::new("/agents/bob"));
        assert!(spec.unconfined);
    }

    #[test]
    fn platform_sandbox_reports_expected_availability_shape() {
        // On macOS this box should be Enforcing (sandbox-exec present); we only
        // assert the call is total and does not panic across platforms.
        let sb = platform_sandbox();
        let _ = sb.availability();
    }

    #[test]
    fn full_access_spec_is_skipped_by_confine() {
        let sb = platform_sandbox();
        let spec = SandboxSpec::from_level(SandboxLevel::FullAccess, Path::new("/tmp"));
        let mut cmd = Command::new("true");
        // FullAccess must be skipped on every platform (including Windows).
        match sb.confine(&mut cmd, &spec) {
            Ok(Confinement::Skipped) => {}
            other => panic!("expected Skipped for FullAccess, got {other:?}"),
        }
    }
}
