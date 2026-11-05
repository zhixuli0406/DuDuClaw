#![cfg(target_os = "linux")]

use std::{io::Write, os::unix::process::CommandExt, process::{Command, Stdio}};
use duduclaw_import::{ImportRequest, SourceKind};
use duduclaw_isolation::ids::RequestId;

#[test]
fn nonroot_binary_missing_source_fd_refuses_without_io_safety_abort() {
    if rustix::process::geteuid().is_root() {
        eprintln!("SKIP missing-fd coverage: root is refused by check_worker before fd validation; rerun as an unprivileged user");
        return;
    }
    let request = ImportRequest { version: 1, migration_id: RequestId([7; 16]),
        employee: None, source_id: "memory".into(), kind: SourceKind::MemoryText,
        title: String::new(), legacy_agent: None, has_wal: false, blob_handle: None };
    let mut command = Command::new(env!("CARGO_BIN_EXE_duduclaw-import"));
    command.env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: the child executes only the async-signal-safe prctl syscall before
    // exec. Do not close stdio or Rust's CLOEXEC exec-error pipe here.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn importer");
    let mut stdin = child.stdin.take().expect("request pipe");
    stdin.write_all(&serde_json::to_vec(&request).unwrap()).expect("send request");
    drop(stdin);
    // Command's temporary descriptors close on exec; no source fd is supplied.
    let output = child.wait_with_output().expect("wait importer");
    assert_eq!(output.status.code(), Some(3), "must exit normally, never abort or die by signal");
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"import worker isolation unavailable\n");
}
