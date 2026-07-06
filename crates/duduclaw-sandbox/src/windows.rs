//! Windows confinement — **not yet implemented**.
//!
//! A native Windows sandbox (WFP for network + AppContainer / job objects for
//! filesystem) is a large undertaking and deferred to a later milestone. Until
//! then this impl is [`Availability::Unsupported`] and, per the fail-closed
//! invariant, refuses to confine a command that required a sandbox — the caller
//! must fall back to the container sandbox or abort the spawn.

use crate::{Availability, Confinement, NativeSandbox, SandboxError, SandboxSpec};
use std::process::Command;

pub struct WindowsSandbox;

impl WindowsSandbox {
    pub fn new() -> Self {
        WindowsSandbox
    }
}

impl NativeSandbox for WindowsSandbox {
    fn confine(
        &self,
        _cmd: &mut Command,
        spec: &SandboxSpec,
    ) -> Result<Confinement, SandboxError> {
        // A FullAccess grant asks for no confinement — honour it uniformly so a
        // Windows host behaves like the others for the unconfined case.
        if spec.unconfined {
            return Ok(Confinement::Skipped);
        }
        // Confinement required but unsupported → fail-closed.
        Ok(Confinement::Refused)
    }

    fn availability(&self) -> Availability {
        Availability::Unsupported
    }
}
