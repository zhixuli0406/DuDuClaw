//! Birth/executable-bound cleanup of the dedicated child's process group.
//! Never signal a process merely because its name contains Chrome.
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    pid: i32,
    group: u32,
    birth: (u64, u64),
    executable: PathBuf,
}

fn identity(pid: i32) -> Option<Identity> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    } != size as i32
    {
        return None;
    }
    let info = unsafe { info.assume_init() };
    // Darwin sys/proc.h: SZOMB=5. Zombies cannot run or spawn more work.
    if info.pbi_uid != unsafe { libc::geteuid() } || info.pbi_status == 5 {
        return None;
    }
    let mut path = [0u8; 4096];
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if length <= 0 {
        return None;
    }
    let end = path
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length as usize);
    use std::os::unix::ffi::OsStrExt;
    Some(Identity {
        pid,
        group: info.pbi_pgid,
        birth: (info.pbi_start_tvsec, info.pbi_start_tvusec),
        executable: PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end])),
    })
}

fn members(group: u32) -> Result<Vec<Identity>, String> {
    let mut pids = [0i32; 4096];
    // macOS sys/proc_info.h: PROC_PGRP_ONLY=2; return value is bytes.
    let bytes = unsafe {
        libc::proc_listpids(
            2,
            group,
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(&pids) as i32,
        )
    };
    if bytes < 0
        || bytes as usize >= std::mem::size_of_val(&pids)
        || bytes as usize % std::mem::size_of::<i32>() != 0
    {
        return Err("local_process_inventory_unavailable".into());
    }
    let mut result = Vec::new();
    for pid in &pids[..bytes as usize / std::mem::size_of::<i32>()] {
        if let Some(identity) = identity(*pid) {
            result.push(identity);
        } else if unsafe { libc::kill(*pid, 0) } == 0 {
            // A zombie is safe, but any other unreadable live process is not.
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
            let size = std::mem::size_of::<libc::proc_bsdinfo>();
            let read = unsafe {
                libc::proc_pidinfo(
                    *pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size as i32,
                )
            };
            if read != size as i32 || unsafe { info.assume_init() }.pbi_status != 5 {
                return Err("local_process_inventory_unavailable".into());
            }
        }
    }
    Ok(result)
}

#[derive(Clone)]
pub(crate) struct OwnedGroup {
    group: u32,
    bundle: PathBuf,
    known: BTreeMap<i32, Identity>,
}
impl OwnedGroup {
    #[cfg(test)]
    pub(super) fn inactive(&self) -> bool {
        members(self.group).is_ok_and(|current| current.is_empty())
    }
    pub(super) fn manifest(&self) -> serde_json::Value {
        serde_json::json!({"group":self.group,"processes":self.known.values().map(|identity|serde_json::json!({"pid":identity.pid,"group":identity.group,"birth_sec":identity.birth.0,"birth_usec":identity.birth.1,"executable":identity.executable})).collect::<Vec<_>>()})
    }
    pub(crate) fn new(pid: u32) -> Result<Self, String> {
        let leader = identity(pid as i32).ok_or("local_child_identity_unavailable")?;
        if leader.group != pid
            || ![super::launch::CHROME, super::launch::EDGE]
                .iter()
                .any(|path| leader.executable == PathBuf::from(path))
        {
            return Err("local_child_identity_refused".into());
        }
        let bundle = leader
            .executable
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("local_child_bundle_unavailable")?
            .to_owned();
        // Retain the verified leader even if the first group inventory later
        // fails. The launch owner must sample before issuing any CDP command.
        let result = Self {
            group: pid,
            bundle,
            known: BTreeMap::from([(pid as i32, leader)]),
        };
        Ok(result)
    }
    pub(crate) fn sample(&mut self) -> Result<(), String> {
        let current = members(self.group)?;
        let anchored = self
            .known
            .values()
            .any(|old| identity(old.pid).as_ref() == Some(old));
        if !current.is_empty() && !anchored {
            return Err("local_process_identity_lost".into());
        }
        for member in current {
            if member.group != self.group || !member.executable.starts_with(&self.bundle) {
                return Err("local_process_identity_refused".into());
            }
            if self
                .known
                .get(&member.pid)
                .is_some_and(|prior| prior != &member)
            {
                return Err("local_process_pid_reused".into());
            }
            self.known.insert(member.pid, member);
        }
        Ok(())
    }
    fn signal_known(&self, signal: i32) {
        for old in self.known.values() {
            if identity(old.pid).as_ref() == Some(old) {
                unsafe {
                    libc::kill(old.pid, signal);
                }
            }
        }
    }
    pub(crate) async fn finish(mut self) -> bool {
        let mut certain = self.sample().is_ok();
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if members(self.group).is_ok_and(|members| members.is_empty()) {
                return certain;
            }
            self.signal_known(signal);
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                certain &= self.sample().is_ok();
                if members(self.group).is_ok_and(|members| members.is_empty()) {
                    return certain;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_unrelated_process_cannot_be_adopted_as_the_browser_group() {
        assert!(OwnedGroup::new(std::process::id()).is_err());
        assert!(identity(std::process::id() as i32).is_some());
    }
}
