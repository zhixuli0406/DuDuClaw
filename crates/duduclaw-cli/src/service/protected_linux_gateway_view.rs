//! R8m 30b: the gateway's own readiness view
//! (`/run/duduclaw-gateway-update/isolation-readiness.json`, written by the
//! gateway: `duduclaw_gateway::isolation_readiness::published`), read by root
//! as untrusted data from the less-privileged gateway account: the directory
//! is pinned below a root-owned, non-group/other-writable `/run` fd and
//! must be a real directory owned by that account; the file is opened with
//! `O_NOFOLLOW`, must be a regular single-link file owned by that account and
//! at most 64 KiB; its content must pass the strict shared check
//! (`published::parse`: `deny_unknown_fields`, fixed identifiers only, this
//! boot, at most 90 s old). Anything else is "gateway view unavailable".
//! It never makes a platform check pass or fail.
use std::{ffi::{CStr, CString}, fs::File, io::Read, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::Path};
use duduclaw_gateway::isolation_readiness::published::{self, View};

const ROOT: &str = "/run";

/// What root knows about the gateway's own readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GatewayView {
    /// Missing, unsafe, oversized, malformed, other boot or stale.
    Unavailable,
    Ready,
    NotReady(View),
}

impl GatewayView {
    fn of(view: Option<View>) -> Self {
        match view {
            None => Self::Unavailable,
            Some(view) if view.ready => Self::Ready,
            Some(view) => Self::NotReady(view),
        }
    }
}

/// The installed gateway's view: account `duduclaw`, this boot, now.
pub(super) fn read_installed() -> GatewayView {
    let Ok(Some(account)) = super::super::bootstrap::passwd_entry("duduclaw") else { return GatewayView::Unavailable };
    if account.uid == 0 { return GatewayView::Unavailable; }
    let Some(boot) = published::current_boot_id() else { return GatewayView::Unavailable };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    GatewayView::of(read_view(Path::new(ROOT), 0, account.uid, &boot, now))
}

/// The accepted document below `root` and the two fixed published names, or `None`.
pub(super) fn read_view(root: &Path, expected_root_uid: u32, owner: u32, boot: &str, now_unix: u64) -> Option<View> {
    let directory = open_gateway_directory(root, expected_root_uid, owner)?;
    let bytes = read_bounded_at(&directory, owner)?;
    published::parse(&bytes, boot, now_unix).ok()
}

fn open_gateway_directory(root: &Path, expected_root_uid: u32, owner: u32) -> Option<File> {
    let run = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root).ok()?;
    let meta = run.metadata().ok()?;
    if !meta.is_dir() || meta.uid() != expected_root_uid || meta.mode() & 0o022 != 0 { return None; }
    let name = CString::new(published::DIRECTORY).ok()?;
    let directory = open_at(&run, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    let meta = directory.metadata().ok()?;
    if !meta.is_dir() || meta.uid() != owner { return None; }
    Some(directory)
}

/// Only fixed single-component published names are passed by the callers.
fn open_at(directory: &File, name: &CStr, flags: libc::c_int) -> Option<File> {
    // SAFETY: the borrowed directory fd and NUL-terminated name remain live.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return None; }
    // SAFETY: openat returned a fresh descriptor, now uniquely owned by File.
    Some(unsafe { File::from_raw_fd(fd) })
}

/// Read from the pinned gateway directory, even if its pathname is replaced.
fn read_bounded_at(directory: &File, owner: u32) -> Option<Vec<u8>> {
    let name = CString::new(published::FILE).ok()?;
    let file = open_at(directory, &name, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != owner || meta.nlink() != 1 || meta.len() > published::MAX_BYTES { return None; }
    let mut bytes = Vec::new();
    file.take(published::MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= published::MAX_BYTES).then_some(bytes)
}

/// The `[資訊]` line for a gateway that reports not ready: each not-ready
/// component as `name(reason)` and the boot assembly's missing producers,
/// fixed identifiers only (validated by `published::parse`).
pub(super) fn not_ready_line(view: &View) -> String {
    let components: Vec<String> = view.components.iter().filter(|component| component.state != "ready")
        .map(|component| format!("{}({})", component.name, component.reason.as_deref().unwrap_or(&component.state)))
        .collect();
    let mut line = String::from("[資訊] 平台檢查均已通過。Gateway 回報尚未就緒");
    if components.is_empty() && view.assembly_missing.is_empty() {
        line.push('。');
        return line;
    }
    line.push('：');
    line.push_str(&components.join("、"));
    if !view.assembly_missing.is_empty() {
        if !components.is_empty() { line.push('；'); }
        line.push_str("啟動組裝尚缺：");
        line.push_str(&view.assembly_missing.join("、"));
    }
    line.push('。');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    const BOOT: &str = "0123abcd-4567-89ef-0123-456789abcdef";
    const NOW: u64 = 1_800_000_000;

    fn document(ready: bool) -> serde_json::Value {
        let names = published::COMPONENTS;
        let components: Vec<_> = names.iter().map(|name| if ready || *name != "mcp_boundary" {
            serde_json::json!({"name": name, "state": "ready", "reason": null})
        } else {
            serde_json::json!({"name": name, "state": "pending", "reason": "pending_integration"})
        }).collect();
        let missing: Vec<&str> = if ready { Vec::new() } else { vec!["connector_providers"] };
        serde_json::json!({"schema": 1, "boot_id": BOOT, "written_unix": NOW, "ready": ready,
            "components": components, "assembly_missing": missing})
    }
    fn me() -> u32 { unsafe { libc::geteuid() } }
    fn directory() -> tempfile::TempDir {
        let root = tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o750)).tempdir().unwrap();
        std::fs::create_dir(root.path().join(published::DIRECTORY)).unwrap();
        root
    }
    fn write(dir: &Path, bytes: &[u8]) {
        let path = dir.join(published::DIRECTORY).join(published::FILE);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn gateway_view_accepts_a_fresh_document_of_this_boot() {
        let dir = directory();
        write(dir.path(), document(true).to_string().as_bytes());
        assert_eq!(GatewayView::of(read_view(dir.path(), me(), me(), BOOT, NOW + 5)), GatewayView::Ready);
        write(dir.path(), document(false).to_string().as_bytes());
        let GatewayView::NotReady(view) = GatewayView::of(read_view(dir.path(), me(), me(), BOOT, NOW + 5)) else {
            panic!("not ready expected");
        };
        assert_eq!(not_ready_line(&view),
            "[資訊] 平台檢查均已通過。Gateway 回報尚未就緒：mcp_boundary(pending_integration)；啟動組裝尚缺：connector_providers。");
    }

    #[test]
    fn gateway_view_rejections_read_as_unavailable() {
        let unavailable = |dir: &Path, owner: u32, now: u64, boot: &str|
            GatewayView::of(read_view(dir, me(), owner, boot, now)) == GatewayView::Unavailable;
        // Missing file.
        let dir = directory();
        assert!(unavailable(dir.path(), me(), NOW, BOOT));
        // Symlinked file (O_NOFOLLOW), even to a valid document.
        let target = directory();
        write(target.path(), document(true).to_string().as_bytes());
        symlink(target.path().join(published::DIRECTORY).join(published::FILE), dir.path().join(published::DIRECTORY).join(published::FILE)).unwrap();
        assert!(unavailable(dir.path(), me(), NOW, BOOT));
        // Symlinked directory.
        let parent = directory();
        let alias = parent.path().join(published::DIRECTORY);
        std::fs::remove_dir(&alias).unwrap();
        symlink(target.path().join(published::DIRECTORY), &alias).unwrap();
        assert!(unavailable(parent.path(), me(), NOW, BOOT));
        // Wrong owner (the expected gateway uid differs from the file's).
        assert!(!unavailable(target.path(), me(), NOW, BOOT));
        assert!(unavailable(target.path(), me().wrapping_add(1), NOW, BOOT));
        // Hard link.
        let dir = directory();
        write(dir.path(), document(true).to_string().as_bytes());
        std::fs::hard_link(dir.path().join(published::DIRECTORY).join(published::FILE), dir.path().join(published::DIRECTORY).join("alias")).unwrap();
        assert!(unavailable(dir.path(), me(), NOW, BOOT));
        // Oversized (valid JSON padded past 64 KiB).
        let dir = directory();
        let mut padded = document(true).to_string().into_bytes();
        padded.extend(vec![b' '; published::MAX_BYTES as usize]);
        write(dir.path(), &padded);
        assert!(unavailable(dir.path(), me(), NOW, BOOT));
        // Unknown field.
        let dir = directory();
        let mut value = document(true);
        value["employee"] = serde_json::json!("alice");
        write(dir.path(), value.to_string().as_bytes());
        assert!(unavailable(dir.path(), me(), NOW, BOOT));
        // Other boot id, stale.
        let dir = directory();
        write(dir.path(), document(true).to_string().as_bytes());
        assert!(unavailable(dir.path(), me(), NOW, "fedcba98-7654-3210-fedc-ba9876543210"));
        assert!(unavailable(dir.path(), me(), NOW + published::MAX_AGE_SECS + 1, BOOT));
        assert!(!unavailable(dir.path(), me(), NOW + published::MAX_AGE_SECS, BOOT));
    }

    #[test]
    fn gateway_view_rejects_a_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let root = directory();
        let path = root.path().join(published::DIRECTORY).join(published::FILE);
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is NUL-terminated and points into this test's temporary directory.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(GatewayView::of(read_view(root.path(), me(), me(), BOOT, NOW)), GatewayView::Unavailable);
    }

    #[test]
    fn gateway_view_rejects_an_untrusted_root() {
        let root = directory();
        write(root.path(), document(true).to_string().as_bytes());
        assert!(read_view(root.path(), me().wrapping_add(1), me(), BOOT, NOW).is_none());
        for mode in [0o770, 0o757] {
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(GatewayView::of(read_view(root.path(), me(), me(), BOOT, NOW)), GatewayView::Unavailable);
        }
    }

    #[test]
    fn gateway_view_reads_the_pinned_directory_after_a_swap() {
        let root = directory();
        let original = document(false).to_string().into_bytes();
        write(root.path(), &original);
        let pinned = open_gateway_directory(root.path(), me(), me()).unwrap();
        let path = root.path().join(published::DIRECTORY);
        std::fs::rename(&path, root.path().join("original")).unwrap();
        std::fs::create_dir(&path).unwrap();
        write(root.path(), document(true).to_string().as_bytes());
        assert_eq!(read_bounded_at(&pinned, me()).unwrap(), original);
        assert_eq!(GatewayView::of(read_view(root.path(), me(), me(), BOOT, NOW)), GatewayView::Ready);
    }

    #[test]
    fn gateway_view_not_ready_line_lists_fixed_identifiers() {
        let mut view: View = serde_json::from_value(document(false)).unwrap();
        view.assembly_missing.clear();
        assert_eq!(not_ready_line(&view), "[資訊] 平台檢查均已通過。Gateway 回報尚未就緒：mcp_boundary(pending_integration)。");
        for component in &mut view.components { component.state = "ready".into(); component.reason = None; }
        view.assembly_missing = vec!["backend".into(), "execution_plans".into()];
        assert_eq!(not_ready_line(&view), "[資訊] 平台檢查均已通過。Gateway 回報尚未就緒：啟動組裝尚缺：backend、execution_plans。");
        view.assembly_missing.clear();
        assert_eq!(not_ready_line(&view), "[資訊] 平台檢查均已通過。Gateway 回報尚未就緒。");
    }
}
