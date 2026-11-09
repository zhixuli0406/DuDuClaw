//! Descriptor shuffle plan (r7 §4, R6-M1 and R7-L1).
use crate::{Error, Result};

pub const HIGH_FD_MIN: i32 = 1024;
pub const MAX_SOURCES: usize = 72;
pub const MAX_TARGET: i32 = 4 + duduclaw_isolation::ipc::MAX_INPUT_FDS as i32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination { Runtime(Vec<i32>), Executable, Script, ExecStatus }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source { pub fd: i32, pub destination: Destination }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdPlan { sources: Vec<Source>, last: i32 }
impl FdPlan {
    pub fn new(sources: Vec<Source>, soft_limit: u64) -> Result<Self> {
        if sources.is_empty() || sources.len() > MAX_SOURCES
            || soft_limit < Self::required_limit(sources.len())? {
            return Err(Error::DescriptorLimit);
        }
        let mut source_fds = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        let mut executable = 0;
        let mut script = 0;
        let mut exec_status = 0;
        for source in &sources {
            if source.fd < 0 || u64::try_from(source.fd).unwrap_or(u64::MAX) >= soft_limit
                || !source_fds.insert(source.fd) { return Err(Error::Descriptors); }
            match &source.destination {
                Destination::Runtime(fds) => {
                    if fds.is_empty() { return Err(Error::Descriptors); }
                    for fd in fds {
                        if !(0..=MAX_TARGET).contains(fd) || !targets.insert(*fd) {
                            return Err(Error::Descriptors);
                        }
                    }
                }
                Destination::Executable => executable += 1,
                Destination::Script => script += 1,
                Destination::ExecStatus => exec_status += 1,
            }
        }
        let last = *targets.last().ok_or(Error::Descriptors)?;
        if executable != 1 || script > 1 || exec_status > 1 || last < 4 || targets.len() != (last + 1) as usize {
            return Err(Error::Descriptors);
        }
        Ok(Self { sources, last })
    }
    pub fn required_limit(count: usize) -> Result<u64> {
        if count > MAX_SOURCES { return Err(Error::DescriptorLimit); }
        Ok(HIGH_FD_MIN as u64 + 2 * count as u64 + 16)
    }
    pub fn sources(&self) -> &[Source] { &self.sources }
    pub fn last_target(&self) -> i32 { self.last }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Vec<Source> {
        vec![Source { fd: 4, destination: Destination::Runtime(vec![0, 1, 2]) },
            Source { fd: 3, destination: Destination::Runtime(vec![4]) },
            Source { fd: 0, destination: Destination::Runtime(vec![3]) },
            Source { fd: 6, destination: Destination::Executable }]
    }
    #[test]
    fn fd_plan_allows_cycles_and_pty_alias_but_not_source_alias() {
        let plan = FdPlan::new(valid(), 4096).unwrap();
        assert_eq!(plan.last_target(), 4);
        let mut bad = valid(); bad[1].fd = 4;
        assert_eq!(FdPlan::new(bad, 4096), Err(Error::Descriptors));
    }
    #[test]
    fn fd_plan_enforces_limit_formula_and_dense_targets() {
        let limit = FdPlan::required_limit(4).unwrap();
        assert!(FdPlan::new(valid(), limit).is_ok());
        assert_eq!(FdPlan::new(valid(), limit - 1), Err(Error::DescriptorLimit));
        let mut bad = valid(); bad[1].destination = Destination::Runtime(vec![5]);
        assert!(FdPlan::new(bad, 4096).is_err());
        let mut bad = valid(); bad[1].destination = Destination::Runtime(vec![0]);
        assert!(FdPlan::new(bad, 4096).is_err());
        assert!(FdPlan::required_limit(73).is_err());
    }
    #[test]
    fn exec_notification_is_single_and_never_a_runtime_target() {
        let mut sources = valid();
        sources.push(Source { fd: 7, destination: Destination::ExecStatus });
        let plan = FdPlan::new(sources.clone(), 4096).unwrap();
        assert_eq!(plan.last_target(), 4);
        sources.push(Source { fd: 8, destination: Destination::ExecStatus });
        assert_eq!(FdPlan::new(sources, 4096), Err(Error::Descriptors));
    }
    #[test]
    fn fd_plan_requires_exactly_one_executable_and_at_most_one_script() {
        let mut bad = valid(); bad.pop(); assert!(FdPlan::new(bad, 4096).is_err());
        let mut bad = valid(); bad.push(Source { fd: 7, destination: Destination::Executable });
        assert!(FdPlan::new(bad, 4096).is_err());
        let mut valid = valid(); valid.push(Source { fd: 7, destination: Destination::Script });
        assert!(FdPlan::new(valid.clone(), 4096).is_ok());
        valid.push(Source { fd: 8, destination: Destination::Script });
        assert!(FdPlan::new(valid, 4096).is_err());
    }
}
