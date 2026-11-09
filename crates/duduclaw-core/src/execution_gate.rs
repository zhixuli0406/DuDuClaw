//! Transitional fence for native child APIs while isolation adapters are wired.
//! A refused launch must not be retried through an unclassified spawn path.
use std::sync::atomic::{AtomicBool, Ordering};
pub use duduclaw_isolation::execution::ExecutionClass;

pub use duduclaw_execution::command::{Child, ChildStdin, ChildStdout, ChildStderr, PtyChild, RunControl,
    NativeCommandSnapshot, ProtectedLaunch, command_snapshot, local_command_roundtrip, pty_snapshot, local_pty_roundtrip,
    capture_context, with_context, capture_runtime, with_runtime, with_context_sync,
    CapturedLaunch, capture_class, capture_class_or, with_class, capture_memory_limit, with_memory_limit};
pub use duduclaw_isolation::{bounded::Text, execution::{RuntimeKind, PtySize}};

pub async fn spawn(cmd: &mut tokio::process::Command, class: ExecutionClass) -> std::io::Result<Child> {
    duduclaw_execution::command::spawn(cmd, class, isolation_required()).await
}

pub async fn spawn_pty(command: portable_pty::CommandBuilder, size: PtySize, class: ExecutionClass) -> std::io::Result<PtyChild> {
    duduclaw_execution::command::spawn_pty(command, size, class, isolation_required()).await
}

pub async fn output(cmd: &mut tokio::process::Command, class: ExecutionClass) -> std::io::Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    spawn(cmd, class).await?.wait_with_output().await
}

pub async fn status(cmd: &mut tokio::process::Command, class: ExecutionClass) -> std::io::Result<std::process::ExitStatus> {
    let mut child = spawn(cmd, class).await?;
    child.stdout.take();
    child.stderr.take();
    child.wait().await
}

static REQUIRED: AtomicBool = AtomicBool::new(false);

pub const REFUSAL: &str = "受保護模式無法啟動員工：隔離執行服務尚未接妥，請由管理員檢查服務狀態。";

/// Called by the trusted bootstrap/migration integration. This latch can only
/// narrow execution for the rest of the process lifetime.
pub fn require_isolation() { REQUIRED.store(true, Ordering::Release); }

/// SR5 round 3 N3: the environment flag OR a host-owned durable fact
/// ([`crate::protected_mode::active`], which latches itself); an
/// employee-editable environment cannot turn it off.
pub fn isolation_required() -> bool {
    REQUIRED.load(Ordering::Acquire) || crate::protected_mode::active()
}

pub fn local_allowed(required: bool, class: ExecutionClass) -> bool {
    !required || matches!(class, ExecutionClass::InstallationRequester
        | ExecutionClass::AccountProbe | ExecutionClass::ModelProbe
        | ExecutionClass::InteractiveLogin)
}

pub fn check_local(class: ExecutionClass) -> std::io::Result<()> {
    if local_allowed(isolation_required(), class) { Ok(()) }
    else { Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, REFUSAL)) }
}

/// Preserve the original native Command, including non-UTF-8 arguments, cwd,
/// environment removals and platform flags. Protected employee work is refused.
pub fn spawn_local(cmd: &mut tokio::process::Command, class: ExecutionClass) -> std::io::Result<tokio::process::Child> {
    check_local(class)?;
    if class == ExecutionClass::InteractiveLogin { scrub_login_environment(cmd.as_std_mut()); }
    cmd.spawn()
}

pub async fn output_local(cmd: &mut tokio::process::Command, class: ExecutionClass) -> std::io::Result<std::process::Output> {
    check_local(class)?;
    if class == ExecutionClass::InteractiveLogin { scrub_login_environment(cmd.as_std_mut()); }
    cmd.output().await
}

/// Login exceptions must not inherit a desktop session or an agent socket.
pub const LOGIN_ENV_BLOCKLIST: &[&str] = &["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS", "SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_AGENT_INFO"];

pub fn scrub_login_environment(cmd: &mut std::process::Command) {
    for key in LOGIN_ENV_BLOCKLIST {
        cmd.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_child_fence_denies_all_employee_classes() {
        use ExecutionClass::*;
        for class in [EmployeeMain, Scheduled, Dispatch, Heartbeat, Mail, Reminder,
            Judge, Fork, Discovery, EmployeeTool, ThirdPartyMcp, ToolLoopMcp, ParseWorker, ImportWorker] {
            assert!(!local_allowed(true, class));
            assert!(local_allowed(false, class));
        }
        for class in [InstallationRequester, AccountProbe, ModelProbe, InteractiveLogin] {
            assert!(local_allowed(true, class));
        }
    }
    #[test]
    fn login_environment_drops_desktop_and_agent_sockets() {
        let mut cmd = std::process::Command::new("fixture");
        cmd.env("SSH_AUTH_SOCK", "secret").env("DISPLAY", ":0").env("KEEP", "value");
        scrub_login_environment(&mut cmd);
        let env: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
        for key in ["DISPLAY", "WAYLAND_DISPLAY", "SSH_AUTH_SOCK", "XDG_RUNTIME_DIR"] {
            assert_eq!(env.get(std::ffi::OsStr::new(key)), Some(&None));
        }
        assert_eq!(env.get(std::ffi::OsStr::new("KEEP")), Some(&Some(std::ffi::OsStr::new("value"))));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn protected_native_fixture() {
        if std::env::var_os("DUDUCLAW_EXECUTION_GATE_FIXTURE").is_none() { return; }
        let mut command = tokio::process::Command::new("/does-not-exist-isolation-fixture");
        let error = spawn_local(&mut command, ExecutionClass::EmployeeMain).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), REFUSAL);
        let error = crate::process_tree::ManagedChild::spawn(&mut command).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_mode_refuses_before_executable_lookup() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "execution_gate::tests::protected_native_fixture", "--nocapture"])
            .env("DUDUCLAW_EXECUTION_GATE_FIXTURE", "1")
            .env(crate::root_key::MODE_ENV, "native-systemd")
            .status().unwrap();
        assert!(result.success());
    }
}
