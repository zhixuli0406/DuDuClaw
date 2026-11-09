//! Returned B2 masters stay private until all isolation receipts arrive.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use duduclaw_isolation::{execution::PtySize, ipc::ErrorCode};
use duduclaw_run::{linux::fd, validation::{Access, FileKind}};
use crate::Result;

fn denied() -> ErrorCode { ErrorCode::IsolationUnavailable }

pub(crate) struct ReturnedPty { master: OwnedFd, taken: bool, pub mount_id: u64 }
impl ReturnedPty {
    /// `mount_id` comes only from the authenticated root B2 status channel.
    /// Compare the actual master AND its ioctl-opened peer with that receipt.
    pub fn validate(master: OwnedFd, mount_id: u64, uid: u32) -> Result<Self> {
        let observed = fd::observe(master.as_raw_fd()).map_err(|_| denied())?;
        let mount_flags = libc::ST_NOSUID as u64 | libc::ST_NOEXEC as u64;
        if mount_id == 0 || observed.identity.mount != mount_id || observed.fs_type != 0x1cd1
            || observed.kind != FileKind::Character || observed.access != Access::ReadWrite
            || !observed.cloexec || observed.uid != 0 || observed.fs_flags & mount_flags != mount_flags
            || (observed.rdev_major, observed.rdev_minor) != (5, 2) { return Err(denied()); }
        let mut number = 0u32;
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPTN, &mut number) } != 0 || number != 0 {
            return Err(denied());
        }
        // First allocation on a fresh devpts instance, never host /dev/ptmx.
        let raw = unsafe { libc::ioctl(master.as_raw_fd(), 0x5441, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if raw < 0 { return Err(denied()); }
        let peer = unsafe { OwnedFd::from_raw_fd(raw) };
        let slave = fd::observe(peer.as_raw_fd()).map_err(|_| denied())?;
        if slave.identity.mount != mount_id || slave.fs_type != 0x1cd1
            || slave.identity.dev_major != observed.identity.dev_major
            || slave.identity.dev_minor != observed.identity.dev_minor
            || slave.kind != FileKind::Character || slave.access != Access::ReadWrite || !slave.cloexec
            || slave.uid != uid || slave.mode & 0o7777 != 0o620
            || slave.rdev_major != 136 || slave.rdev_minor != 0 { return Err(denied()); }
        Ok(Self { master, taken: false, mount_id })
    }
    pub fn take(&mut self) -> Result<OwnedFd> {
        if self.taken { return Err(ErrorCode::AlreadyTaken); }
        let fd = self.master.try_clone().map_err(|_| denied())?;
        self.taken = true;
        Ok(fd)
    }
    pub fn resize(&self, size: PtySize) -> Result<()> {
        size.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        let value = libc::winsize { ws_row: size.rows, ws_col: size.columns, ws_xpixel: 0, ws_ypixel: 0 };
        let mut actual: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &value) } != 0
            || unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCGWINSZ, &mut actual) } != 0
            || actual.ws_row != size.rows || actual.ws_col != size.columns { return Err(denied()); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn returned_pty_refuses_regular_devices_and_zero_mount_identity() {
        for mount in [0, 1, u64::MAX] {
            let device = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
            assert!(ReturnedPty::validate(device.into(), mount, 70000).is_err());
        }
    }
}
