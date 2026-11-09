//! Windows process-tree ownership independent of the leader's lifetime.

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetProcessId, OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

/// Owns a non-inheritable, unnamed job. Descendants cannot break away, and
/// closing the last handle kills them even after the original leader exits.
pub struct WindowsJob {
    handle: Option<OwnedHandle>,
}

impl WindowsJob {
    pub const CREATION_FLAGS: u32 = CREATE_SUSPENDED;

    pub fn new() -> io::Result<Self> {
        // SAFETY: null attributes/name request an unnamed, non-inheritable job.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() { return Err(io::Error::last_os_error()); }
        // SAFETY: CreateJobObjectW returned a fresh valid handle owned here.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: all-zero fields are valid defaults for this Win32 POD struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the live job and initialized buffer match the class and size.
        let ok = unsafe { SetInformationJobObject(
            handle.as_raw_handle(), JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) };
        if ok == 0 { return Err(io::Error::last_os_error()); }
        Ok(Self { handle: Some(handle) })
    }

    /// Attach a freshly CREATE_SUSPENDED child before resuming its initial
    /// thread. On error the caller must kill/reap the still-owned child; the
    /// job's drop kills any process already assigned before the failure.
    pub fn assign_and_resume(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        let job = self.handle.as_ref().ok_or_else(|| io::Error::other("job already closed"))?;
        // SAFETY: both handles remain live for this call; no ownership transfers.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the borrowed process handle remains valid throughout this call.
        let pid = unsafe { GetProcessId(process.as_raw_handle()) };
        if pid == 0 { return Err(io::Error::last_os_error()); }
        resume_initial_thread(pid)
    }

    /// Terminate the tree and close the job even if TerminateJobObject fails.
    /// KILL_ON_JOB_CLOSE is the final backstop on every exit/drop path.
    pub fn terminate(&mut self) -> io::Result<()> {
        let Some(handle) = self.handle.take() else { return Ok(()); };
        // SAFETY: this is our valid job handle; closing happens after the call.
        let ok = unsafe { TerminateJobObject(handle.as_raw_handle(), 1) };
        let result = if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) };
        drop(handle);
        result
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn resume_initial_thread(pid: u32) -> io::Result<()> {
    // SAFETY: this flag snapshots thread metadata only; no pointers are passed.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    // SAFETY: the snapshot is a fresh owned handle, checked against its sentinel.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: THREADENTRY32 is a POD struct; dwSize is initialized before use.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    // SAFETY: snapshot and correctly sized output buffer are valid for this call.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    let mut thread_id = None;
    let mut thread_count = 0usize;
    while found != 0 {
        if entry.th32OwnerProcessID == pid {
            thread_count += 1;
            thread_id = Some(entry.th32ThreadID);
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        // SAFETY: snapshot and correctly sized output buffer remain valid.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) { return Err(error); }
    if thread_count != 1 {
        return Err(io::Error::other("suspended child must have exactly one thread"));
    }
    let thread_id = thread_id.ok_or_else(|| io::Error::other("suspended child initial thread was not found"))?;
    // SAFETY: OpenThread consumes the unique PID thread id from the complete snapshot.
    let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if raw.is_null() { return Err(io::Error::last_os_error()); }
    // SAFETY: OpenThread succeeded and ownership has not been transferred.
    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: this handle has THREAD_SUSPEND_RESUME and remains live.
    let count = unsafe { ResumeThread(thread.as_raw_handle()) };
    match count {
        u32::MAX => Err(io::Error::last_os_error()),
        1 => Ok(()),
        _ => Err(io::Error::other("child initial thread was not suspended exactly once")),
    }
}
