//! HX6 root tests (D8 revision §4 [F10], H15 hosts: Ubuntu 22.04/24.04,
//! Debian 12). They exercise the recording helper's nested browser Landlock
//! (`recording_sandbox::BrowserLayer`) with NNP set, exactly as the helper
//! applies it to node/Chromium, against a host Chromium:
//!
//!   ISO_ROOT_CHROMIUM=/path/to/chromium cargo test -p duduclaw-execution \
//!     --test recording_root -- --ignored --test-threads=1
//!
//! The full gate also needs F_L ∩ F_H around this layer; that is the
//! trampoline fixture's job (`duduclaw-run` root fixture) — see the HX6
//! report. A missing `ISO_ROOT_CHROMIUM` fails the test (never a silent pass).
#![cfg(target_os = "linux")]
use std::{os::unix::process::CommandExt, path::{Path, PathBuf}, process::Command};
use duduclaw_execution::recording_sandbox::{probe_abi, enforce_raw, BrowserLayer};

fn chromium() -> String {
    std::env::var("ISO_ROOT_CHROMIUM").expect("ISO_ROOT_CHROMIUM must name the host's Chromium binary")
}

/// A scratch directory standing in for `/helper-tmp/<call>`, and a fake
/// `/work` sibling holding a secret the browser must never read.
fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let scratch = base.path().join("0123456789abcdef0123456789abcdef");
    let work = base.path().join("work");
    std::fs::create_dir(&scratch).unwrap();
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("secret.html"), "<p>HX6-SECRET-WORK</p>").unwrap();
    (base, scratch, work)
}

/// `program args…` with NNP + the browser layer installed in the child.
#[allow(unsafe_code)]
fn contained(layer: &BrowserLayer, scratch: &Path, program: &str, args: &[&str]) -> std::process::Output {
    let ruleset = layer.raw();
    let mut command = Command::new(program);
    command.args(args).env_clear().env("HOME", scratch).env("TMPDIR", scratch).env("PATH", "/usr/bin:/bin")
        .current_dir(scratch);
    let in_child = move || -> std::io::Result<()> {
        // SAFETY: sets the caller's own no_new_privs flag (required by Landlock).
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 { return Err(std::io::Error::last_os_error()); }
        enforce_raw(ruleset)
    };
    // SAFETY: prctl + two Landlock syscalls in the forked child; no allocation.
    unsafe { command.pre_exec(in_child); }
    command.output().unwrap()
}

fn chromium_args<'a>(url: &'a str) -> Vec<&'a str> {
    // The driver's contained flags (pipe transport is Playwright's; here the
    // browser itself must start and stay inside the layer).
    vec!["--headless=new", "--no-sandbox", "--disable-dev-shm-usage", "--js-flags=--jitless", "--disable-gpu",
        "--no-first-run", "--disable-background-networking", "--dump-dom", url]
}

#[test]
#[ignore = "requires root Linux with Landlock and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_launches_under_helper_sandbox() {
    let chromium = chromium();
    let (_base, scratch, _work) = dirs();
    let layer = BrowserLayer::prepare(&scratch, &[chromium.as_str()], 3128).expect("browser layer");
    let output = contained(&layer, &scratch, &chromium, &chromium_args("data:text/html,<p>HX6-LAUNCHED</p>"));
    assert!(output.status.success(), "Chromium did not start under the browser layer: {:?}\n{}",
        output.status, String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("HX6-LAUNCHED"));
}

#[test]
#[ignore = "requires root Linux with Landlock and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_cannot_read_work() {
    let chromium = chromium();
    let (_base, scratch, work) = dirs();
    let layer = BrowserLayer::prepare(&scratch, &[chromium.as_str()], 3128).expect("browser layer");
    let url = format!("file://{}", work.join("secret.html").display());
    let output = contained(&layer, &scratch, &chromium, &chromium_args(&url));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("HX6-SECRET-WORK"), "the browser read /work");
    // The same layer still lets it write its own scratch (trace staging).
    let shell = contained(&layer, &scratch, "/bin/sh", &["-c", &format!("echo ok > {0}/probe && cat {1}/secret.html",
        scratch.display(), work.display())]);
    assert!(scratch.join("probe").exists());
    assert!(!shell.status.success() && !String::from_utf8_lossy(&shell.stdout).contains("HX6-SECRET-WORK"));
}

#[test]
#[ignore = "requires root Linux with Landlock ABI >= 4 (Ubuntu 24.04 6.8) and a host Chromium (ISO_ROOT_CHROMIUM); H15"]
fn iso_root_helper_chromium_tcp_connect_only_forwarder_port() {
    let abi = probe_abi().expect("Landlock");
    if abi < 4 {
        // Documented residual (threat row T5): nothing to pin below ABI 4.
        eprintln!("Landlock ABI {abi} < 4: TCP pinning unavailable on this kernel (T5 residual)");
        return;
    }
    let chromium = chromium();
    let (_base, scratch, _work) = dirs();
    let forwarder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (port, other_port) = (forwarder.local_addr().unwrap().port(), other.local_addr().unwrap().port());
    let layer = BrowserLayer::prepare(&scratch, &[chromium.as_str()], port).expect("browser layer");
    let probe = |script: String| contained(&layer, &scratch, "/usr/bin/python3", &["-c", &script]).status.success();
    let connect = |target: u16| format!("import socket; socket.create_connection(('127.0.0.1', {target}), 2)");
    assert!(probe(connect(port)), "the forwarder port must stay reachable");
    assert!(!probe(connect(other_port)), "any other loopback TCP port must be refused");
    assert!(!probe("import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen(1)".into()),
        "the browser tree may not bind any TCP port");
    // Chromium itself cannot fetch from a non-forwarder loopback listener.
    let url = format!("http://127.0.0.1:{other_port}/");
    let output = contained(&layer, &scratch, &chromium, &chromium_args(&url));
    other.set_nonblocking(true).unwrap();
    assert!(other.accept().is_err(), "Chromium reached a non-forwarder port: {}", String::from_utf8_lossy(&output.stderr));
}
