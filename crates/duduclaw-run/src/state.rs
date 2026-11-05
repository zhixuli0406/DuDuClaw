use crate::{Error, Result};

/// Every edge is an observed success, never a proposal to perform a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Start, NonDumpable, InheritedClosed, BootstrapVerified, DescriptorsVerified,
    Namespaces, PrivatePropagation, RootBuilt, Pivoted, Loopback, PidNamespace,
    SupervisorClosed, SupervisorDropped, SupervisorFiltered,
    ProcMounted, Securebits, BoundingDropped, IdentityDropped, CapsCleared,
    DumpableVerified, NoNewPrivileges, PrivilegesVerified, Landlocked,
    CommonFiltered, Forked, InitClosed, InitFiltered, RuntimeSession,
    RuntimeFds, RuntimeFiltered, Executed, Failed,
}

#[derive(Debug)]
pub struct Machine { stage: Stage }
impl Default for Machine { fn default() -> Self { Self { stage: Stage::Start } } }
impl Machine {
    pub fn stage(&self) -> Stage { self.stage }
    pub fn advance(&mut self, next: Stage) -> Result<()> {
        use Stage::*;
        let allowed = matches!((self.stage, next),
            (Start, NonDumpable) | (NonDumpable, InheritedClosed)
            | (InheritedClosed, BootstrapVerified) | (BootstrapVerified, DescriptorsVerified)
            | (DescriptorsVerified, Namespaces) | (Namespaces, PrivatePropagation)
            | (PrivatePropagation, RootBuilt) | (RootBuilt, Pivoted) | (Pivoted, Loopback)
            | (Loopback, PidNamespace) | (PidNamespace, SupervisorClosed)
            | (SupervisorClosed, SupervisorDropped) | (SupervisorDropped, SupervisorFiltered)
            | (PidNamespace, ProcMounted) | (ProcMounted, Securebits)
            | (Securebits, BoundingDropped) | (BoundingDropped, IdentityDropped)
            | (IdentityDropped, CapsCleared) | (CapsCleared, DumpableVerified)
            | (DumpableVerified, NoNewPrivileges) | (NoNewPrivileges, PrivilegesVerified)
            | (PrivilegesVerified, Landlocked) | (Landlocked, CommonFiltered)
            | (CommonFiltered, Forked) | (Forked, InitClosed) | (InitClosed, InitFiltered)
            | (Forked, RuntimeSession) | (RuntimeSession, RuntimeFds)
            | (RuntimeFds, RuntimeFiltered) | (RuntimeFiltered, Executed));
        if !allowed { self.stage = Failed; return Err(Error::State); }
        self.stage = next;
        Ok(())
    }
    pub fn fail(&mut self) { self.stage = Stage::Failed; }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_order_never_skips_or_recovers() {
        use Stage::*;
        let child = [Start, NonDumpable, InheritedClosed, BootstrapVerified, DescriptorsVerified,
            Namespaces, PrivatePropagation, RootBuilt, Pivoted, Loopback, PidNamespace,
            ProcMounted, Securebits, BoundingDropped, IdentityDropped, CapsCleared,
            DumpableVerified, NoNewPrivileges, PrivilegesVerified, Landlocked,
            CommonFiltered, Forked, RuntimeSession, RuntimeFds, RuntimeFiltered, Executed];
        let mut m = Machine::default();
        for next in child.iter().skip(1) { m.advance(*next).unwrap(); }
        for i in 0..child.len() - 2 {
            let mut m = Machine { stage: child[i] };
            assert_eq!(m.advance(child[i + 2]), Err(Error::State));
            assert_eq!(m.advance(child[i + 1]), Err(Error::State));
        }
        let mut m = Machine { stage: PidNamespace };
        for next in [SupervisorClosed, SupervisorDropped, SupervisorFiltered] { m.advance(next).unwrap(); }
        assert!(m.advance(RuntimeSession).is_err());
        let mut m = Machine { stage: Forked };
        m.advance(InitClosed).unwrap(); m.advance(InitFiltered).unwrap();
        assert!(m.advance(Executed).is_err());
    }
}
