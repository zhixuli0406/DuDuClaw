//! Unix PTYs create a new session before exec (portable-pty's setsid).
//! Keep the leader unreaped until cleanup, so its session id cannot be reused.
use super::{command_with_env, writer::InputWriter};
use portable_pty::{Child, ExitStatus, MasterPty, PtySize};
use std::{
    collections::BTreeSet,
    io::{self, Read},
    sync::mpsc,
    thread,
};

type Spawned = (Process, mpsc::Receiver<Vec<u8>>, InputWriter);
pub(super) struct Process {
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    pid: u32,
    pub cleanup_complete: bool,
}
impl Process {
    pub fn spawn(command: Option<&[String]>, size: PtySize) -> io::Result<Spawned> {
        let pair = portable_pty::native_pty_system()
            .openpty(size)
            .map_err(io::Error::other)?;
        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;
        let mut child = pair
            .slave
            .spawn_command(command_with_env(command, std::env::vars_os()))
            .map_err(io::Error::other)?;
        let Some(pid) = child
            .process_id()
            .filter(|pid| *pid > 1 && *pid <= i32::MAX as u32)
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("missing PTY pid"));
        };
        let process = Self {
            master: pair.master,
            child: Some(child),
            pid,
            cleanup_complete: true,
        };
        drop(pair.slave);
        // Bounded byte queues; neither worker writes to stdout/stderr or logs.
        let (output, rx) = mpsc::sync_channel(16);
        thread::Builder::new()
            .name("tui-pty-read".into())
            .spawn(move || {
                let mut bytes = [0; 4096];
                loop {
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            if output.send(bytes[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })?;
        let input = InputWriter::spawn(writer)?;
        Ok((process, rx, input))
    }
    #[cfg(test)]
    pub fn pid_for_test(&self) -> u32 {
        self.pid
    }
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)
    }
    #[allow(unsafe_code)] // waitid with WNOWAIT is required to retain ownership until cleanup.
    pub fn exited(&self) -> bool {
        if self.child.is_none() {
            return true;
        }
        // SAFETY: valid child id and initialized siginfo; WNOWAIT deliberately
        // leaves the leader waitable until session/group cleanup has completed.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        result == 0 && unsafe { info.si_pid() } != 0
    }
    #[allow(unsafe_code)] // Validated, unreaped child/session ids; no borrowed memory crosses FFI.
    pub fn finish(&mut self) -> io::Result<ExitStatus> {
        let Some(mut child) = self.child.take() else {
            return Ok(ExitStatus::with_exit_code(0));
        };
        // Interactive shells put foreground/background jobs in separate groups.
        // Stop the session leader first, then include every group in its session.
        // Explicit daemonization into a new session is outside group containment.
        unsafe {
            libc::kill(self.pid as i32, libc::SIGSTOP);
        }
        let mut groups = BTreeSet::from([self.pid]);
        if let Some(group) = self.master.process_group_leader().filter(|p| *p > 1) {
            // SAFETY: getsid accepts a pid; only accept our still-owned session.
            if unsafe { libc::getsid(group) } == self.pid as i32 {
                groups.insert(group as u32);
            }
        }
        let inventory = std::process::Command::new("/bin/ps")
            .args(["-A", "-o", "pid="])
            .output();
        let mut cleanup_error = None;
        match inventory {
            Ok(output) if output.status.success() => {
                for pid in String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .filter_map(|s| s.parse::<i32>().ok())
                    .filter(|pid| *pid > 1)
                {
                    // SAFETY: scalar process ids from the OS inventory. Never
                    // signal the host session or a reused leader (not reaped yet).
                    if unsafe { libc::getsid(pid) } == self.pid as i32 {
                        unsafe {
                            libc::kill(pid, libc::SIGSTOP);
                        }
                        let group = unsafe { libc::getpgid(pid) };
                        if group > 1 {
                            groups.insert(group as u32);
                        }
                    }
                }
            }
            _ => cleanup_error = Some(io::Error::other("PTY session inventory failed")),
        }
        for group in groups {
            if let Err(error) = duduclaw_core::platform::kill_process_group(group)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                cleanup_error = Some(error);
            }
        }
        let _ = child.kill();
        let status = child.wait();
        self.cleanup_complete = cleanup_error.is_none();
        status
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.finish();
        }
    }
}
