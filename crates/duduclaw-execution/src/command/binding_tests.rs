//! Native fixtures exercise binding at spawn, not after output collection.
use super::*;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const FIXTURE: &str = "command::binding_tests::binding_child_fixture";
const FIXTURE_ENV: &str = "DUDUCLAW_PROCESS_BINDING_FIXTURE";

#[test]
fn binding_child_fixture() {
    if std::env::var_os(FIXTURE_ENV).is_none() { return; }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

fn fixture() -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", FIXTURE, "--nocapture"]).env(FIXTURE_ENV, "1")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

fn pending() -> SpawnBinding { SpawnBinding::pending(Instant::now() + Duration::from_secs(30)) }

#[tokio::test]
async fn spawn_binds_before_return_and_root_exit_revokes_without_draining() {
    let binding = pending();
    let mut child = spawn_bound(&mut fixture(), ExecutionClass::EmployeeMain, false, Some(binding.clone())).await.unwrap();
    let root = binding.root().expect("backend must bind before returning");
    assert_eq!(Some(root.pid), child.id());
    assert_ne!(root.pid, std::process::id());
    // Retain output handles; revocation must not depend on the caller reading EOF.
    child.stdin.as_mut().unwrap().write_all(b"exit\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while binding.root().is_some() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(child.wait().await.unwrap().success());
    assert!(binding.root().is_none());
}

#[tokio::test]
async fn failed_spawn_and_child_drop_revoke() {
    let binding = pending();
    let mut missing = Command::new("duduclaw-nonexistent-binding-fixture");
    assert!(spawn_bound(&mut missing, ExecutionClass::EmployeeMain, false, Some(binding.clone())).await.is_err());
    assert!(binding.bind_spawn(crate::process_identity::RootProcess::capture(std::process::id()).unwrap()).is_err());
    let binding = pending();
    let child = spawn_bound(&mut fixture(), ExecutionClass::EmployeeMain, false, Some(binding.clone())).await.unwrap();
    assert!(binding.root().is_some());
    drop(child);
    assert!(binding.root().is_none());
}

#[tokio::test]
async fn protected_launch_rejects_host_credential_before_spawning() {
    let binding = pending();
    assert!(spawn_bound(&mut fixture(), ExecutionClass::EmployeeMain, true, Some(binding.clone())).await.is_err());
    assert!(binding.root().is_none());
}

#[tokio::test]
async fn pty_binding_uses_the_client_pid() {
    let binding = pending();
    let mut cmd = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", FIXTURE, "--nocapture"]);
    cmd.env(FIXTURE_ENV, "1");
    let mut child = spawn_pty_bound(cmd, PtySize { rows: 24, columns: 80 },
        ExecutionClass::EmployeeMain, false, Some(binding.clone())).await.unwrap();
    assert_eq!(Some(binding.root().unwrap().pid), child.id());
    assert_ne!(child.id(), Some(std::process::id()));
    // ConPTY translates Enter (CR) into a console line-input event. LF alone
    // is not Enter there; pipes and Unix PTYs still use their existing LF.
    #[cfg(windows)]
    let input = b"exit\r";
    #[cfg(not(windows))]
    let input = b"exit\n";
    child.input.as_mut().unwrap().write_all(input).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), child.wait()).await.unwrap().unwrap();
    assert!(binding.root().is_none());
}
