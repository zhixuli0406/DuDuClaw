//! The OS side effects the maintenance SSH verbs need, behind one trait so the
//! whole open / status / close / tick flow runs in unit tests on any host.
//!
//! Production ([`SystemHost`]) spawns fixed argv literals (no shell), reads
//! `CLOCK_BOOTTIME`, and looks up the account through `getpwnam_r` /
//! `getgrnam_r`. The SSH unit start/stop stays in `dispatch.rs`
//! (`dispatch_ssh_start` / `dispatch_ssh_stop`), reached through
//! [`MaintHost::ssh_start`] / [`MaintHost::ssh_stop`].

use std::future::Future;
use std::time::Duration;

use crate::dispatch::DispatchResult;

/// Output of one spawned command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdOut {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Upper bound for one spawned command, so a hung `systemctl` or `journalctl`
/// cannot hold [`super::SSH_LOCK`] forever.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

pub trait MaintHost: Send + Sync {
    /// Run `argv` (argv[0] is the program). `Err` only when it could not be
    /// spawned or timed out; a non-zero exit is `Ok` with `success: false`.
    fn run(&self, argv: &[String]) -> impl Future<Output = Result<CmdOut, String>> + Send;
    /// The legacy open sequence (firewall port, then the SSH unit).
    fn ssh_start(&self) -> impl Future<Output = DispatchResult> + Send;
    /// The legacy close sequence (stop units, kill sessions, close the port,
    /// read back).
    fn ssh_stop(&self) -> impl Future<Output = DispatchResult> + Send;
    /// `CLOCK_BOOTTIME`: counts suspend, unaffected by `settimeofday`.
    fn boottime(&self) -> Result<Duration, String>;
    /// Wall clock since the Unix epoch (only for records and the PID 1
    /// fallback timer's calendar time, never for the deadline).
    fn realtime(&self) -> Result<Duration, String>;
    /// `(uid, primary gid)` of a user name.
    fn user_ids(&self, name: &str) -> Option<(u32, u32)>;
    /// gid of a group name.
    fn group_id(&self, name: &str) -> Option<u32>;
}

/// Build an owned argv from literals.
pub fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// The real OS.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl MaintHost for SystemHost {
    async fn run(&self, argv: &[String]) -> Result<CmdOut, String> {
        let (program, args) = argv.split_first().ok_or_else(|| "empty argv".to_string())?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args).kill_on_drop(true);
        match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => Ok(CmdOut {
                success: out.status.success(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }),
            Ok(Err(e)) => Err(format!("failed to spawn {program}: {e}")),
            Err(_) => Err(format!("{program} did not finish within {}s", COMMAND_TIMEOUT.as_secs())),
        }
    }

    async fn ssh_start(&self) -> DispatchResult {
        crate::dispatch::dispatch_ssh_start().await
    }

    async fn ssh_stop(&self) -> DispatchResult {
        crate::dispatch::dispatch_ssh_stop().await
    }

    fn boottime(&self) -> Result<Duration, String> {
        boottime_now()
    }

    fn realtime(&self) -> Result<Duration, String> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("system clock before the epoch: {e}"))
    }

    fn user_ids(&self, name: &str) -> Option<(u32, u32)> {
        lookup_user(name)
    }

    fn group_id(&self, name: &str) -> Option<u32> {
        lookup_group(name)
    }
}

#[cfg(target_os = "linux")]
fn boottime_now() -> Result<Duration, String> {
    // SAFETY: `timespec` is plain old data; all-zero is a valid value (and
    // covers targets whose definition has padding fields).
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: `ts` is a valid, writable timespec; CLOCK_BOOTTIME exists on
    // every kernel the appliance supports (2.6.39+).
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    if rc != 0 {
        return Err(format!("clock_gettime(CLOCK_BOOTTIME) failed: {}", std::io::Error::last_os_error()));
    }
    Ok(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

/// Only the Linux appliance has `CLOCK_BOOTTIME`; anywhere else a window
/// cannot be given a trustworthy deadline, so opening one fails.
#[cfg(not(target_os = "linux"))]
fn boottime_now() -> Result<Duration, String> {
    Err("CLOCK_BOOTTIME is only available on Linux".to_string())
}

fn lookup_user(name: &str) -> Option<(u32, u32)> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut pwd: libc::passwd = unsafe_zeroed_passwd();
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to a live, correctly sized buffer owned by
    // this frame; getpwnam_r writes only within `buf.len()`.
    let rc = unsafe { libc::getpwnam_r(c_name.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some((pwd.pw_uid, pwd.pw_gid))
}

fn lookup_group(name: &str) -> Option<u32> {
    let c_name = std::ffi::CString::new(name).ok()?;
    // SAFETY: `group` is plain old data; all-zero is a valid initial value.
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result: *mut libc::group = std::ptr::null_mut();
    // SAFETY: as in `lookup_user`.
    let rc = unsafe { libc::getgrnam_r(c_name.as_ptr(), &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(grp.gr_gid)
}

fn unsafe_zeroed_passwd() -> libc::passwd {
    // SAFETY: `passwd` is plain old data; all-zero is a valid initial value.
    unsafe { std::mem::zeroed() }
}
