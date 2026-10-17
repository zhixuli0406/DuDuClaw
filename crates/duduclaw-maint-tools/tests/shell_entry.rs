//! Exercise the real forced-command entry point without an SSH server.
#![cfg(unix)]

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Output, Stdio};

use duduclaw_maint_tools::tools::{Tool, resolve_tool};

fn run_shell(original: Option<&OsStr>, input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_duduclaw-maint-shell"));
    command
        // Dropbear invokes the account's shell with its fixed forced command.
        .args(["-c", "/usr/libexec/duduclaw/maint-shell"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(original) = original {
        command.env("SSH_ORIGINAL_COMMAND", original);
    }
    let mut child = command.spawn().expect("spawn maintenance shell");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write menu input");
    child.wait_with_output().expect("maintenance shell exit")
}

fn assert_interactive(original: Option<&OsStr>, input: &[u8]) {
    let output = run_shell(original, input);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Maintenance menu"), "{output:?}");
    assert!(stdout.contains("Choose [0-9]"), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("This account only runs"));
}

#[test]
fn missing_original_command_shows_menu_and_exits() {
    assert_interactive(None, b"0\n");
}

#[test]
fn dropbear_empty_original_command_shows_menu_and_exits() {
    assert_interactive(Some(OsStr::new("")), b"q\n");
}

#[test]
fn interactive_eof_exits_for_missing_and_empty_original_command() {
    for original in [None, Some(OsStr::new(""))] {
        assert_interactive(original, b"");
    }
}

#[test]
fn status_remains_command_mode() {
    let output = run_shell(Some(OsStr::new("status")), b"");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Failed units"), "{output:?}");
    assert!(stdout.contains("Service status"), "{output:?}");
    assert!(!stdout.contains("Maintenance menu"), "{output:?}");
    // macOS lacks systemctl: the action reports missing tools (1), not an
    // unrecognised command (2). Linux retains the successful status result.
    let expected = if resolve_tool(Tool::Systemctl, |path| path.is_file()).is_some() {
        0
    } else {
        1
    };
    assert_eq!(output.status.code(), Some(expected), "{output:?}");
}

fn assert_refused(original: &OsStr) {
    let output = run_shell(Some(original), b"");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Maintenance menu"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("This account only runs"));
}

#[test]
fn whitespace_original_command_does_not_enter_interactive_mode() {
    for original in [" ", "\t\r\n"] {
        assert_refused(OsStr::new(original));
    }
}

#[test]
fn unknown_and_shell_commands_remain_refused() {
    for original in [
        "unknown",
        "/bin/sh",
        "sh -c id",
        "status; id",
        "status extra",
    ] {
        assert_refused(OsStr::new(original));
    }
}

#[test]
fn non_utf8_original_command_remains_refused() {
    assert_refused(OsStr::from_bytes(b"\xff"));
}
