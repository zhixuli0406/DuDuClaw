//! Verb → hardcoded shell-out dispatch.
//!
//! Every `Command::new(...)` argument here is a literal, exactly the
//! discipline `duduclaw-gateway/src/device_ops.rs` documents for its own
//! `SystemDeviceOps` — this module is that same rule applied on the root
//! side of the privilege boundary. The only caller-supplied value that
//! ever reaches a spawned process is [`SysdRequest::Hostname`]'s `set`
//! field, and it is passed via `Command::arg()` (never a shell), so its
//! content can only ever be *the hostname value*, never *which command
//! runs*.

use tokio::process::Command;

use crate::protocol::{MAX_HOSTNAME_LEN, SysdError, SysdOpOutput, SysdRequest};

pub type DispatchResult = Result<SysdOpOutput, SysdError>;

async fn run(mut cmd: Command) -> DispatchResult {
    match cmd.output().await {
        Ok(out) => Ok(SysdOpOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Err(e) => Err(SysdError::unsupported(format!("failed to spawn: {e}"))),
    }
}

/// `systemctl enable duduclaw-firstboot-provision.service` then
/// `systemctl reboot`. The enable step is best-effort — an image without
/// that unit (or a dev/test host) should still complete the reboot rather
/// than abort the whole factory-reset flow; a failure there is folded into
/// the final `stdout` as a `[warn]` line, mirroring the equivalent note
/// `SystemDeviceOps::factory_reset` used to build itself before this
/// verb existed.
async fn dispatch_factory_reset() -> DispatchResult {
    let mut enable_cmd = Command::new("systemctl");
    enable_cmd.args(["enable", "duduclaw-firstboot-provision.service"]);
    let enable = enable_cmd.output().await;
    let warn_note = match &enable {
        Ok(out) if out.status.success() => String::new(),
        Ok(out) => format!(
            "\n[warn] re-arming first-boot provisioning failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => format!("\n[warn] re-arming first-boot provisioning failed: {e}"),
    };

    let mut reboot_cmd = Command::new("systemctl");
    reboot_cmd.arg("reboot");
    let reboot = run(reboot_cmd).await?;
    Ok(SysdOpOutput { stdout: format!("{}{warn_note}", reboot.stdout), ..reboot })
}

/// `hostnamectl set-hostname <name>`. Rejects an empty or over-length
/// value as a structured `bad_request` before ever spawning anything —
/// `Command::arg()` is already injection-safe regardless of content, this
/// check exists purely to refuse an obviously-wrong request early.
async fn dispatch_hostname(name: &str) -> DispatchResult {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SysdError::bad_request("hostname value must not be empty"));
    }
    if trimmed.chars().count() > MAX_HOSTNAME_LEN {
        return Err(SysdError::bad_request(format!(
            "hostname value exceeds {MAX_HOSTNAME_LEN} chars"
        )));
    }
    let mut cmd = Command::new("hostnamectl");
    cmd.args(["set-hostname", trimmed]);
    run(cmd).await
}

/// Dispatch one already-authorized, already-parsed request to its
/// hardcoded command sequence.
pub async fn dispatch(req: &SysdRequest) -> DispatchResult {
    match req {
        SysdRequest::Reboot => {
            let mut cmd = Command::new("systemctl");
            cmd.arg("reboot");
            run(cmd).await
        }
        SysdRequest::Poweroff => {
            let mut cmd = Command::new("systemctl");
            cmd.arg("poweroff");
            run(cmd).await
        }
        SysdRequest::SysupdateStatus => {
            let mut cmd = Command::new("systemd-sysupdate");
            cmd.args(["list", "--json=short"]);
            run(cmd).await
        }
        SysdRequest::SysupdateApply => {
            let mut cmd = Command::new("systemd-sysupdate");
            cmd.arg("update");
            run(cmd).await
        }
        SysdRequest::FactoryReset => dispatch_factory_reset().await,
        SysdRequest::Hostname { set } => dispatch_hostname(set).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hostname_rejects_empty_without_spawning() {
        let result = dispatch_hostname("   ").await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn hostname_rejects_over_length_without_spawning() {
        let long = "a".repeat(MAX_HOSTNAME_LEN + 1);
        let result = dispatch_hostname(&long).await;
        assert!(matches!(result, Err(e) if e.kind == "bad_request"));
    }

    #[tokio::test]
    async fn unsupported_binary_yields_unsupported_kind_not_panic() {
        // A command that (almost certainly) doesn't exist on the test host —
        // must degrade to a structured error, never panic the connection task.
        let cmd = Command::new("duduclaw-sysd-test-nonexistent-binary-xyz");
        let result = run(cmd).await;
        assert!(matches!(result, Err(e) if e.kind == "unsupported"));
    }
}
