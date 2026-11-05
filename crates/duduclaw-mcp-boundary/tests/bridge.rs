use std::process::Command;

#[test]
fn bridge_without_inherited_socket_fails_without_stdout() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_duduclaw-mcp-bridge"));
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: close is async-signal-safe, and fd 3 is not a Rust-owned object
        // in this child after fork. The command uses only stdio descriptors.
        unsafe { command.pre_exec(|| { libc::close(3); Ok(()) }); }
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn bridge_forwards_bytes_and_exits_on_gateway_eof_with_stdin_open() {
    use std::{io::{Read, Write}, os::unix::net::UnixStream,
        process::Stdio, time::{Duration, Instant}};
    let (mut host, employee) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_duduclaw-mcp-bridge"));
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    duduclaw_mcp_boundary::config::fd::McpChildFd::new(&employee).unwrap().configure(&mut command);
    // The command retains its own CLOEXEC source until fork, not a borrowed fd.
    drop(employee);
    let mut child = command.spawn().unwrap();
    // Bound the entire exchange, including pipe reads/writes. Killing the
    // bridge on timeout also releases the reader thread's stdout pipe.
    host.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let exchange = || -> std::io::Result<()> {
            input.write_all(b"request\n")?;
            let mut bytes = [0; 8];
            host.read_exact(&mut bytes)?;
            if &bytes != b"request\n" { return Err(std::io::ErrorKind::InvalidData.into()); }
            host.write_all(b"response\n")?;
            let mut bytes = [0; 9];
            output.read_exact(&mut bytes)?;
            if &bytes != b"response\n" { return Err(std::io::ErrorKind::InvalidData.into()); }
            // Include a response without a newline: MCP is a byte pump and
            // must not rely on stdout's line buffering or gateway EOF.
            host.write_all(b"response")?;
            let mut bytes = [0; 8];
            output.read_exact(&mut bytes)?;
            if &bytes != b"response" { return Err(std::io::ErrorKind::InvalidData.into()); }
            Ok(())
        }();
        drop(host);
        let _ = sent.send((exchange, input));
    });
    let _input = match received.recv_timeout(Duration::from_secs(5)) {
        Ok((Ok(()), input)) => input,
        result => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bridge request/response exchange failed before gateway EOF: {result:?}");
        }
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() { assert!(status.success()); break; }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("bridge retained a live stdin after gateway EOF");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// This is a transport/uid transition fixture, not vendor CLI acceptance. The
/// registry still requires an exact-version CLI receipt before enabling a run.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires root on a Linux VM; executes a disposable child as uid 70000"]
fn iso_root_bridge_fd_survives_uid_drop_and_two_execs() {
    use std::{io::{Read, Write}, os::unix::{net::UnixStream, process::CommandExt},
        process::Stdio, time::Duration};
    // SAFETY: geteuid has no side effects.
    assert_eq!(unsafe { libc::geteuid() }, 0, "run this ignored test as root");
    let (mut host, employee) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    host.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    // Copy the fixture binary out of a potentially root-only build directory.
    let root = tempfile::tempdir_in("/tmp").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let bridge = root.path().join("duduclaw-mcp-bridge");
    std::fs::copy(env!("CARGO_BIN_EXE_duduclaw-mcp-bridge"), &bridge).unwrap();
    std::fs::set_permissions(&bridge, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec \"$1\"", "fd-fixture"]).arg(&bridge).env_clear()
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    duduclaw_mcp_boundary::config::fd::McpChildFd::new(&employee).unwrap().configure(&mut command);
    // SAFETY: child-only async-signal-safe credential syscalls, no account edits.
    unsafe {
        command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(70000, 70000, 70000) != 0
                || libc::setresuid(70000, 70000, 70000) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(employee);
    drop(command);
    child.stdin.take().unwrap().write_all(b"request\n").unwrap();
    let mut request = [0; 8];
    host.read_exact(&mut request).unwrap();
    assert_eq!(&request, b"request\n");
    host.write_all(b"response\n").unwrap();
    drop(host);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"response\n");
    assert!(output.stderr.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn bridge_refuses_named_socket_before_forwarding_any_bytes() {
    use std::{os::{fd::AsRawFd, unix::{net::{UnixListener, UnixStream}, process::CommandExt}}, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("socket");
    let listener = UnixListener::bind(&path).unwrap();
    let client = UnixStream::connect(path).unwrap();
    let (_host, _) = listener.accept().unwrap();
    assert!(duduclaw_mcp_boundary::config::fd::McpChildFd::new(&client).is_err());
    let mut command = Command::new(env!("CARGO_BIN_EXE_duduclaw-mcp-bridge"));
    let source = client.as_raw_fd();
    command.stdin(Stdio::null());
    // SAFETY: intentionally bypass the host helper to exercise shim validation.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
