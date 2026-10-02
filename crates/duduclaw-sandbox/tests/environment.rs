#![cfg(any(target_os = "macos", target_os = "linux"))]

use duduclaw_sandbox::{Availability, Confinement, SandboxSpec, platform_sandbox};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const PROBE_MODE: &str = "DUDUCLAW_SANDBOX_ENV_PROBE";
const CANARY: &str = "DUDUCLAW_SANDBOX_PARENT_SECRET";
const OVERRIDE: &str = "DUDUCLAW_SANDBOX_OVERRIDE";
const ALLOWED: &str = "DUDUCLAW_SANDBOX_ALLOWED";

// Run each probe in its own test-harness process. The canary belongs only to
// that process, so parallel tests never mutate the shared process environment.
fn probe_in_subprocess(test_name: &str, mode: &str) {
    if std::env::var(PROBE_MODE).as_deref() == Ok(mode) {
        probe_child_environment(mode);
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env_clear()
        .env(PROBE_MODE, mode)
        .env(CANARY, "parent-secret-canary")
        .env(OVERRIDE, "parent-value")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated environment probe failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn probe_child_environment(mode: &str) {
    let sandbox = platform_sandbox();
    if sandbox.availability() == Availability::Unsupported {
        assert!(
            cfg!(target_os = "linux"),
            "Seatbelt must be available on macOS"
        );
        eprintln!("native environment probe skipped: Landlock unavailable");
        return;
    }
    let spec = SandboxSpec {
        readable_paths: vec![PathBuf::from("/")],
        writable_paths: Vec::new(),
        allow_network: true,
        unconfined: false,
    };
    let mut cmd = Command::new("/usr/bin/env");
    match mode {
        "clear" => {
            cmd.env(OVERRIDE, "discarded-before-clear")
                .env_clear()
                .env(ALLOWED, "allowlisted-value")
                .env(CANARY, "discarded-after-clear")
                .env_remove(CANARY);
        }
        "inherit" => {}
        "remove_override" => {
            cmd.env_remove(CANARY).env(OVERRIDE, "child-value");
        }
        _ => panic!("unexpected probe mode"),
    }
    assert_eq!(
        sandbox.confine(&mut cmd, &spec).unwrap(),
        Confinement::Applied
    );
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "confined env command must start");
    let text = String::from_utf8(output.stdout).unwrap();
    let env: BTreeMap<_, _> = text
        .lines()
        .map(|line| line.split_once('=').expect("environment entry"))
        .collect();
    match mode {
        "clear" => {
            assert_eq!(env, BTreeMap::from([(ALLOWED, "allowlisted-value")]));
        }
        "inherit" => {
            assert_eq!(env.get(CANARY), Some(&"parent-secret-canary"));
            assert_eq!(env.get(OVERRIDE), Some(&"parent-value"));
        }
        "remove_override" => {
            assert!(!env.contains_key(CANARY));
            assert_eq!(env.get(OVERRIDE), Some(&"child-value"));
            assert_eq!(env.get(PROBE_MODE), Some(&mode));
        }
        _ => unreachable!(),
    }
    assert!(
        !env.contains_key(""),
        "environment probe key must never reach child"
    );
}

#[test]
fn confined_env_clear_excludes_parent_canary() {
    probe_in_subprocess("confined_env_clear_excludes_parent_canary", "clear");
}

#[test]
fn confined_default_environment_preserves_inheritance() {
    probe_in_subprocess(
        "confined_default_environment_preserves_inheritance",
        "inherit",
    );
}

#[test]
fn confined_environment_preserves_explicit_remove_and_override() {
    probe_in_subprocess(
        "confined_environment_preserves_explicit_remove_and_override",
        "remove_override",
    );
}
