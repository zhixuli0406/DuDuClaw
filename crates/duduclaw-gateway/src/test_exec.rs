//! Test-only helper for writing fake executables (shell-script stand-ins for
//! `docker`, `claude`, `duduclaw eval`, judges…) that a test spawns right away.
//!
//! Why not `std::fs::write` + `set_permissions`: on Linux, `execve` fails with
//! `ETXTBSY` ("Text file busy") while any process holds the file open for
//! writing. Tests run on many threads; if another thread forks while we hold
//! the write fd, the child inherits it and keeps it until its own `exec`
//! (`O_CLOEXEC` only closes it then), so our spawn of the fresh script fails
//! intermittently. Here a child `/bin/sh` creates and fills the file, so this
//! process never owns a writable fd on it and no sibling fork can inherit one.
//! Once that child has been reaped the file has no writers left.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Create `path` with `contents` and permission bits `mode` (e.g. `0o755`).
pub(crate) fn write_executable(path: &Path, contents: impl AsRef<[u8]>, mode: u32) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod "$2" "$1""#, "sh"])
        .arg(path)
        .arg(format!("{mode:o}"))
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn /bin/sh to write a fake executable");
    // Dropping stdin after the write closes our end so `cat` sees EOF.
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(contents.as_ref())
        .expect("write fake executable contents");
    let status = child.wait().expect("wait for /bin/sh");
    assert!(status.success(), "writing fake executable {} failed: {status}", path.display());
}
