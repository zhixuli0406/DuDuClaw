//! The OS side effects the maintenance SSH verbs need, behind one trait so the
//! whole open / status / close / tick flow runs in unit tests on any host.
//!
//! Production ([`SystemHost`]) spawns fixed argv literals (no shell), reads
//! `CLOCK_BOOTTIME`, and looks up the account through `getpwnam_r` /
//! `getgrnam_r`. The SSH unit start/stop stays in `dispatch.rs`
//! (`dispatch_ssh_start` / `dispatch_ssh_stop`), reached through
//! [`MaintHost::ssh_start`] / [`MaintHost::ssh_stop`].

use std::future::Future;
use std::path::PathBuf;
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
    /// Run `argv` and read at most `max_lines` lines of its stdout, stopping
    /// the process once that many were read (`truncated` is then true). A
    /// line longer than [`MAX_LINE_BYTES`] is replaced by an empty line. A
    /// non-zero exit after all output was read is an `Err`.
    fn run_lines(&self, argv: &[String], max_lines: usize) -> impl Future<Output = Result<Lines, String>> + Send;
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
    /// The real path of a pinned executable, as journald records it in
    /// `_EXE` (see [`resolve_exe`]).
    fn trusted_exe(&self, pin: &ExePin) -> Result<String, String>;
    /// This boot's id (`/proc/sys/kernel/random/boot_id`).
    fn boot_id(&self) -> Result<String, String>;
    /// `(uid, primary gid)` of a user name.
    fn user_ids(&self, name: &str) -> Option<(u32, u32)>;
    /// gid of a group name.
    fn group_id(&self, name: &str) -> Option<u32>;
}

/// Stdout lines read by [`MaintHost::run_lines`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lines {
    pub lines: Vec<String>,
    /// The line limit was reached and the rest was not read.
    pub truncated: bool,
}

/// Longest stdout line kept by [`MaintHost::run_lines`].
pub const MAX_LINE_BYTES: usize = 256 * 1024;

const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

/// An executable whose identity is checked through journald's `_EXE`: the
/// configured `path` may be a symlink (oe-core builds dropbear as
/// `dropbearmulti` with `/usr/sbin/dropbear` linking to it), and its real
/// path must be a file directly inside `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExePin {
    pub path: PathBuf,
    pub dir: PathBuf,
}

/// Resolve `pin.path` to its real path, requiring a regular file directly
/// inside `pin.dir` (both canonicalised), owned by `owner_uid` and not
/// writable by group or others. Any failure refuses (no events of that
/// process are trusted).
pub fn resolve_exe(pin: &ExePin, owner_uid: u32) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let real = std::fs::canonicalize(&pin.path).map_err(|e| format!("cannot resolve {}: {e}", pin.path.display()))?;
    let dir = std::fs::canonicalize(&pin.dir).map_err(|e| format!("cannot resolve {}: {e}", pin.dir.display()))?;
    if real.parent() != Some(dir.as_path()) {
        return Err(format!("{} resolves to {}, outside {}", pin.path.display(), real.display(), dir.display()));
    }
    let meta = std::fs::symlink_metadata(&real).map_err(|e| format!("{}: {e}", real.display()))?;
    if !meta.is_file() || meta.uid() != owner_uid || meta.mode() & 0o022 != 0 {
        return Err(format!("{} must be a regular file owned by uid {owner_uid}, not group/other writable", real.display()));
    }
    real.to_str().map(str::to_string).ok_or_else(|| format!("{} is not UTF-8", real.display()))
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

    async fn run_lines(&self, argv: &[String], max_lines: usize) -> Result<Lines, String> {
        let (program, args) = argv.split_first().ok_or_else(|| "empty argv".to_string())?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn {program}: {e}"))?;
        let stdout = child.stdout.take().ok_or_else(|| format!("{program}: no stdout"))?;
        let read = async {
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut out = Lines::default();
            loop {
                if out.lines.len() >= max_lines {
                    out.truncated = true;
                    return Ok::<_, String>(out);
                }
                match read_bounded_line(&mut reader).await.map_err(|e| format!("reading {program}: {e}"))? {
                    None => return Ok(out),
                    Some(line) => out.lines.push(line),
                }
            }
        };
        let out = match tokio::time::timeout(COMMAND_TIMEOUT, read).await {
            Ok(r) => r?,
            Err(_) => return Err(format!("{program} did not finish within {}s", COMMAND_TIMEOUT.as_secs())),
        };
        if out.truncated {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Ok(out);
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(out),
            Ok(Ok(status)) => Err(format!("{program} exited with {status}")),
            Ok(Err(e)) => Err(format!("waiting for {program}: {e}")),
            Err(_) => Err(format!("{program} did not exit within {}s", COMMAND_TIMEOUT.as_secs())),
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

    fn trusted_exe(&self, pin: &ExePin) -> Result<String, String> {
        resolve_exe(pin, 0)
    }

    fn boot_id(&self) -> Result<String, String> {
        let raw = std::fs::read_to_string(BOOT_ID_PATH).map_err(|e| format!("{BOOT_ID_PATH}: {e}"))?;
        let id = raw.trim();
        if id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            Ok(id.to_string())
        } else {
            Err(format!("{BOOT_ID_PATH} has unexpected content"))
        }
    }

    fn user_ids(&self, name: &str) -> Option<(u32, u32)> {
        lookup_user(name)
    }

    fn group_id(&self, name: &str) -> Option<u32> {
        lookup_group(name)
    }
}

/// One line of at most [`MAX_LINE_BYTES`] (without the newline); an overlong
/// line is consumed and returned empty. `None` at end of input.
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    let mut buf = Vec::new();
    let mut overlong = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !overlong { None } else { Some(finish_line(buf, overlong)) });
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !overlong {
            let piece = if done { &chunk[..take - 1] } else { &chunk[..take] };
            if buf.len() + piece.len() > MAX_LINE_BYTES {
                overlong = true;
                buf.clear();
            } else {
                buf.extend_from_slice(piece);
            }
        }
        reader.consume(take);
        if done {
            return Ok(Some(finish_line(buf, overlong)));
        }
    }
}

fn finish_line(buf: Vec<u8>, overlong: bool) -> String {
    if overlong { String::new() } else { String::from_utf8_lossy(&buf).into_owned() }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getuid() }
    }

    #[test]
    fn exe_pins_resolve_plain_and_multi_call_layouts() {
        let t = tempfile::tempdir().unwrap();
        let sbin = t.path().join("usr/sbin");
        std::fs::create_dir_all(&sbin).unwrap();
        let sbin = std::fs::canonicalize(&sbin).unwrap();
        // Plain: /usr/sbin/dropbear is the binary.
        std::fs::write(sbin.join("dropbear"), b"x").unwrap();
        let pin = ExePin { path: sbin.join("dropbear"), dir: sbin.clone() };
        assert_eq!(resolve_exe(&pin, me()).unwrap(), sbin.join("dropbear").to_str().unwrap());
        // Multi-call: /usr/sbin/dropbear -> dropbearmulti.
        std::fs::remove_file(sbin.join("dropbear")).unwrap();
        std::fs::write(sbin.join("dropbearmulti"), b"x").unwrap();
        std::os::unix::fs::symlink("dropbearmulti", sbin.join("dropbear")).unwrap();
        assert_eq!(resolve_exe(&pin, me()).unwrap(), sbin.join("dropbearmulti").to_str().unwrap());
        // A link leaving the directory, a missing target, a writable file, a wrong owner.
        let elsewhere = t.path().join("tmp-dropbear");
        std::fs::write(&elsewhere, b"x").unwrap();
        std::fs::remove_file(sbin.join("dropbear")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, sbin.join("dropbear")).unwrap();
        assert!(resolve_exe(&pin, me()).is_err());
        std::fs::remove_file(sbin.join("dropbear")).unwrap();
        assert!(resolve_exe(&pin, me()).is_err());
        std::os::unix::fs::symlink("dropbearmulti", sbin.join("dropbear")).unwrap();
        std::fs::set_permissions(sbin.join("dropbearmulti"), std::os::unix::fs::PermissionsExt::from_mode(0o775)).unwrap();
        assert!(resolve_exe(&pin, me()).is_err());
        std::fs::set_permissions(sbin.join("dropbearmulti"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(resolve_exe(&pin, me().wrapping_add(1)).is_err());
    }

    #[tokio::test]
    async fn bounded_lines_skip_overlong_ones() {
        let mut data = b"a\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES + 10));
        data.extend_from_slice(b"\nb\nc");
        let mut r = tokio::io::BufReader::with_capacity(1024, &data[..]);
        let mut got = Vec::new();
        while let Some(l) = read_bounded_line(&mut r).await.unwrap() {
            got.push(l);
        }
        assert_eq!(got, ["a", "", "b", "c"]);
    }

    #[tokio::test]
    async fn run_lines_stops_at_the_limit() {
        let host = SystemHost;
        let out = host.run_lines(&argv(&["sh", "-c", "i=0; while [ $i -lt 100000 ]; do echo $i; i=$((i+1)); done"]), 10).await.unwrap();
        assert_eq!(out.lines.len(), 10);
        assert!(out.truncated);
        assert_eq!(out.lines[9], "9");
        let all = host.run_lines(&argv(&["sh", "-c", "echo a; echo b"]), 10).await.unwrap();
        assert_eq!((all.lines.len(), all.truncated), (2, false));
        assert!(host.run_lines(&argv(&["sh", "-c", "echo a; exit 3"]), 10).await.is_err());
    }
}
