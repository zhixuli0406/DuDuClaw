//! Linux confinement via Landlock (kernel LSM, 5.13+).
//!
//! Landlock only constrains the calling thread and its descendants, so the
//! ruleset MUST be enforced inside the child (post-`fork`), never in the
//! multi-threaded gateway. We build the ruleset **in the parent** (all path
//! descriptors opened and registered into the kernel ruleset object before
//! `fork`) and enforce it in a `pre_exec` closure via `restrict_self()`, which
//! is a `prctl` + `landlock_restrict_self` syscall pair.
//!
//! Availability is probed up front with a `HardRequirement` ruleset: a kernel
//! without Landlock yields [`Availability::Unsupported`], and — because this
//! crate's callers require confinement when they opt in — `confine` then returns
//! [`Confinement::Refused`] (fail-closed) rather than installing a no-op.

use crate::{Availability, Confinement, LandlockPlan, NativeSandbox, SandboxError, SandboxSpec};
use landlock::{
    path_beneath_rules, Access, AccessFs, AccessNet, CompatLevel, Ruleset, RulesetAttr,
    RulesetCreatedAttr, ABI,
};
use std::process::Command;

/// Filesystem ABI to request. V1 (kernel 5.13) is broadly available; BestEffort
/// upgrades enforcement where newer kernels allow it.
const FS_ABI: ABI = ABI::V1;

pub struct LinuxSandbox;

impl LinuxSandbox {
    pub fn new() -> Self {
        LinuxSandbox
    }
}

/// Probe whether Landlock is present by attempting to create a strict ruleset.
fn landlock_available() -> bool {
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(FS_ABI))
        .and_then(|r| r.create())
        .is_ok()
}

impl NativeSandbox for LinuxSandbox {
    fn confine(
        &self,
        cmd: &mut Command,
        spec: &SandboxSpec,
    ) -> Result<Confinement, SandboxError> {
        if spec.unconfined {
            return Ok(Confinement::Skipped);
        }
        if !landlock_available() {
            // Required but no kernel support → fail-closed.
            return Ok(Confinement::Refused);
        }

        let plan = LandlockPlan::from_spec(spec);

        // Build the ruleset in the parent (BestEffort so a partial kernel still
        // enforces filesystem access even if the network ABI is missing).
        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(FS_ABI))
            .map_err(|e| SandboxError::Profile(format!("handle_access(fs): {e}")))?;

        if plan.deny_network {
            ruleset = ruleset
                .handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp)
                .map_err(|e| SandboxError::Profile(format!("handle_access(net): {e}")))?;
        }

        let mut created = ruleset
            .create()
            .map_err(|e| SandboxError::Profile(format!("create ruleset: {e}")))?;

        if !plan.read_only.is_empty() {
            created = created
                .add_rules(path_beneath_rules(&plan.read_only, AccessFs::from_read(FS_ABI)))
                .map_err(|e| SandboxError::Profile(format!("add read rules: {e}")))?;
        }
        if !plan.read_write.is_empty() {
            created = created
                .add_rules(path_beneath_rules(&plan.read_write, AccessFs::from_all(FS_ABI)))
                .map_err(|e| SandboxError::Profile(format!("add write rules: {e}")))?;
        }
        // When `deny_network` and no NetPort rule is added, all bind/connect are
        // denied by the handled AccessNet rights.

        // Enforce inside the child. SAFETY: `restrict_self()` performs only a
        // `prctl(PR_SET_NO_NEW_PRIVS)` + `landlock_restrict_self` syscall pair on
        // the already-created ruleset fd (built before `fork`); no path is opened
        // and no unbounded allocation happens in the child. The ruleset is moved
        // into the closure via an `Option` and consumed exactly once.
        let mut slot = Some(created);
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                let created = slot.take().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "landlock ruleset already consumed",
                    )
                })?;
                created
                    .restrict_self()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                Ok(())
            });
        }
        Ok(Confinement::Applied)
    }

    fn availability(&self) -> Availability {
        if landlock_available() {
            Availability::Enforcing
        } else {
            Availability::Unsupported
        }
    }
}
