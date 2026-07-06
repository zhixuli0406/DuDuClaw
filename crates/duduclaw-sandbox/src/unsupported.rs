//! Fallback for any OS without a native sandbox impl (e.g. *BSD). Behaves like
//! the Windows stub: skip a FullAccess grant, otherwise fail-closed.

use crate::{Availability, Confinement, NativeSandbox, SandboxError, SandboxSpec};
use std::process::Command;

pub struct UnsupportedSandbox;

impl UnsupportedSandbox {
    pub fn new() -> Self {
        UnsupportedSandbox
    }
}

impl NativeSandbox for UnsupportedSandbox {
    fn confine(
        &self,
        _cmd: &mut Command,
        spec: &SandboxSpec,
    ) -> Result<Confinement, SandboxError> {
        if spec.unconfined {
            return Ok(Confinement::Skipped);
        }
        Ok(Confinement::Refused)
    }

    fn availability(&self) -> Availability {
        Availability::Unsupported
    }
}
