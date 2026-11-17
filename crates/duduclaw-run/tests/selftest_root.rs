//! R7a: the shipped `duduclaw-run-selftest` binary, run directly as root on a
//! disposable Linux VM (no systemd unit; spawnd's unit path is covered by the
//! spawnd root tests). Guarded by DUDUCLAW_ISO_DISPOSABLE=1 because it creates
//! the `duduclaw-selftest` system account when it is missing.
use std::process::{Command, Stdio};

fn require_disposable_root() {
    assert!(cfg!(target_os = "linux"), "iso_root_ tests require native Linux");
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
        "set DUDUCLAW_ISO_DISPOSABLE=1 only on a disposable root VM");
    let uid = Command::new("/usr/bin/id").arg("-u").output().expect("id -u");
    assert_eq!(String::from_utf8_lossy(&uid.stdout).trim(), "0", "run iso_root_ tests as root");
}

fn ensure_selftest_account() {
    let name = duduclaw_isolation::ids::SELFTEST_USER;
    let present = Command::new("/usr/bin/getent").args(["passwd", name]).stdout(Stdio::null()).status()
        .expect("getent passwd");
    if present.success() { return; }
    let status = Command::new("/usr/sbin/useradd")
        .args(["--system", "--user-group", "--no-create-home", "--home-dir", "/nonexistent",
            "--shell", "/usr/sbin/nologin", name])
        .status().expect("useradd");
    assert!(status.success(), "create the {name} system account");
}

/// Bounded run of the built binary; returns (exit code, stdout, stderr).
fn run(arguments: &[&str]) -> (Option<i32>, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_duduclaw-run-selftest"))
        .args(arguments).env_clear().stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("start duduclaw-run-selftest");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().expect("self-test status").is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().expect("stop timed-out self-test");
            panic!("duduclaw-run-selftest exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("self-test output");
    (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
#[ignore = "iso_root_: disposable root Linux VM with namespaces, Landlock and seccomp; DUDUCLAW_ISO_DISPOSABLE=1"]
fn iso_root_selftest_binary_passes_on_this_kernel() {
    require_disposable_root();
    ensure_selftest_account();
    let (code, stdout, stderr) = run(&[]);
    assert_eq!(code, Some(0), "self-test failed: {stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one JSON line: {stdout}");
    let value: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON line");
    let object = value.as_object().expect("object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["detected_abi", "kernel", "selected_abi", "version"]);
    assert_eq!(value["version"], 1);
    let kernel = value["kernel"].as_array().expect("kernel pair");
    assert_eq!(kernel.len(), 2);
    let major = kernel[0].as_u64().unwrap();
    let minor = kernel[1].as_u64().unwrap();
    assert!((major, minor) >= (5, 15));
    let detected = value["detected_abi"].as_u64().unwrap() as u32;
    let selected = value["selected_abi"].as_u64().unwrap() as u32;
    assert!(detected >= 1);
    assert_eq!(selected, detected.min(duduclaw_isolation::LANDLOCK_HIGHEST_KNOWN_ABI));
}

#[test]
#[ignore = "iso_root_: disposable root Linux VM; DUDUCLAW_ISO_DISPOSABLE=1"]
fn iso_root_selftest_binary_refuses_arguments() {
    require_disposable_root();
    for arguments in [&["--help"][..], &[""][..], &["a", "b"][..]] {
        let (code, stdout, stderr) = run(arguments);
        assert_eq!(code, Some(3), "{arguments:?}: {stderr}");
        assert!(stdout.is_empty());
        assert_eq!(stderr.trim_end(), r#"{"code":"selftest_failed","stage":"arguments"}"#);
    }
}
