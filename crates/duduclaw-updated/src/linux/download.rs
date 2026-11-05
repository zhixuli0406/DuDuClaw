//! Fixed official repository. Only the non-root downloader performs network I/O.
use std::{fs::File, io::{Read, Write}, os::{fd::AsRawFd, unix::process::CommandExt}, process::{Child, ChildStdout, Command, Stdio}, time::{Duration, Instant}};
use duduclaw_isolation::update::{Architecture, Product, ReleaseVersion};
use crate::{Error, Result};

pub struct Downloader { pub uid: u32, pub gid: u32 }
pub struct Transfer { child: Child, output: ChildStdout, deadline: Instant }
impl Downloader {
    pub fn new(uid: u32, gid: u32) -> Result<Self> {
        if uid == 0 || gid == 0 || uid >= 60000 || gid >= 60000 { return Err(Error::Maintenance); }
        Ok(Self { uid, gid })
    }
    pub fn release(&self, product: Product, arch: Architecture, version: ReleaseVersion, asset: &str) -> Result<Transfer> {
        if asset.is_empty() || asset.len() > 160 || !asset.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || asset == "." || asset == ".." { return Err(Error::Invalid); }
        // Pro uses a separate publication pipeline; no guessed or caller-selected URL.
        if product != Product::Duduclaw { return Err(Error::Incompatible); }
        let arch = match arch { Architecture::X86_64 => "x86_64", Architecture::Aarch64 => "aarch64" };
        let url = format!("https://github.com/zhixuli0406/DuDuClaw/releases/download/v{}/isolated-linux-{arch}-{asset}", crate::version_text(version));
        self.url(&url)
    }
    pub fn latest(&self) -> Result<Transfer> {
        self.url("https://api.github.com/repos/zhixuli0406/DuDuClaw/releases/latest")
    }
    fn url(&self, url: &str) -> Result<Transfer> {
        let uid = self.uid;
        let gid = self.gid;
        // /usr/bin is a trusted OS executable, not release content. Clear all
        // inherited proxy, credential, loader and curl config environments.
        super::files::Directory::open(std::path::Path::new("/usr/bin"))?;
        let m = std::fs::metadata("/usr/bin/curl")?;
        use std::os::unix::fs::MetadataExt;
        if !m.is_file() || m.uid() != 0 || m.mode() & 0o022 != 0 { return Err(Error::Maintenance); }
        let mut command = Command::new("/usr/bin/curl");
        command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", "/nonexistent").env("LC_ALL", "C")
            .current_dir("/").args(["-q", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
                "--fail", "--silent", "--location", "--max-redirs", "3", "--connect-timeout", "15", "--max-time", "300",
                "--user-agent", "DuDuClaw-isolated-updater", "--", url])
            .stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped());
        drop_identity(&mut command, uid, gid);
        let mut child = command.spawn()?;
        let output = child.stdout.take().ok_or(Error::Maintenance)?;
        Ok(Transfer { child, output, deadline: Instant::now() + Duration::from_secs(305) })
    }
}
/// Shared by the downloader and its real uid/group acceptance test.
fn drop_identity(command: &mut Command, uid: u32, gid: u32) {
    // SAFETY: pre_exec performs only async-signal-safe scalar syscalls.
    unsafe { command.pre_exec(move || {
        let mut real_uid = 0; let mut effective_uid = 0; let mut saved_uid = 0;
        let mut real_gid = 0; let mut effective_gid = 0; let mut saved_gid = 0;
        if libc::setgroups(0, std::ptr::null()) != 0 || libc::setresgid(gid, gid, gid) != 0
            || libc::setresuid(uid, uid, uid) != 0 || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::getresuid(&mut real_uid, &mut effective_uid, &mut saved_uid) != 0
            || libc::getresgid(&mut real_gid, &mut effective_gid, &mut saved_gid) != 0
            || [real_uid, effective_uid, saved_uid] != [uid; 3]
            || [real_gid, effective_gid, saved_gid] != [gid; 3]
            || libc::getgroups(0, std::ptr::null_mut()) != 0
            || libc::prctl(libc::PR_GET_DUMPABLE) != 0
            || libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 {
            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
        }
        Ok(())
    }); }
}

impl Read for Transfer {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(std::io::ErrorKind::TimedOut.into()); }
            let mut pollfd = libc::pollfd { fd: self.output.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let result = unsafe { libc::poll(&mut pollfd, 1, remaining.as_millis().min(1000) as i32) };
            if result < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted { continue; }
                return Err(e);
            }
            if result > 0 { return self.output.read(bytes); }
        }
    }
}
impl Transfer {
    pub fn finish(&mut self) -> Result<()> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return if status.success() { Ok(()) } else { Err(Error::Maintenance) };
            }
            if Instant::now() >= self.deadline { return Err(Error::Maintenance); }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn bytes(mut self, cap: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        (&mut self).take(cap as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > cap { return Err(Error::Invalid); }
        self.finish()?;
        Ok(bytes)
    }
    pub fn artifact(mut self, file: &mut File, artifact: &duduclaw_isolation::update::ReleaseArtifact) -> Result<()> {
        super::files::copy_artifact(&mut self, file, artifact)?;
        self.finish()?;
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(artifact.mode as u32))?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }
}
impl Drop for Transfer {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires root Linux and the provisioned duduclaw-dl account; no network"]
    fn iso_root_downloader_real_saved_ids_and_empty_supplementary_groups() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let user = nix::unistd::User::from_name(duduclaw_isolation::ids::DOWNLOAD_USER).unwrap().expect("provision duduclaw-dl");
        let downloader = Downloader::new(user.uid.as_raw(), user.gid.as_raw()).unwrap();
        // /proc/self/status reflects the actual exec child, including saved IDs,
        // NoNewPrivileges and effective/permitted capabilities after uid drop.
        let mut command = Command::new("/usr/bin/cat");
        command.env_clear().arg("/proc/self/status").stdin(Stdio::null()).stderr(Stdio::null());
        drop_identity(&mut command, downloader.uid, downloader.gid);
        let output = command.output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let field = |name: &str| text.lines().find_map(|line| line.split_once(':').filter(|(key, _)| *key == name).map(|(_, value)| value.trim())).unwrap();
        for (name, expected) in [("Uid", downloader.uid), ("Gid", downloader.gid)] {
            assert_eq!(field(name).split_whitespace().map(|v| v.parse::<u32>().unwrap()).collect::<Vec<_>>(), vec![expected; 4]);
        }
        assert_eq!(field("Groups"), "");
        assert_eq!(field("NoNewPrivs"), "1");
        for name in ["CapEff", "CapPrm", "CapAmb"] { assert_eq!(u64::from_str_radix(field(name), 16).unwrap(), 0); }
    }
}
