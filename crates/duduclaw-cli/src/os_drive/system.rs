//! A7a `system` group — about/timezone/ntp/update-check. Every read reuses
//! a pure/file-based `duduclaw-gateway` function directly (no WS RPC, no
//! running-gateway dependency — the exact "same capability, two front doors"
//! pattern `mcp_os_ops.rs`'s O-0 module already established for the
//! agent-facing MCP surface; see `commercial/docs/DESIGN-os-self-drive-
//! 2026-08.md` §3 for why this sidesteps the dashboard WS/Ed25519 auth
//! entirely). The two writes (`timezone-set`/`ntp-set`) go through
//! `device_ops::select_sysd_ops()`, which dials `duduclaw-sysd`'s own Unix
//! socket directly — same-uid boundary reasoning in §7 of the design doc.

use std::path::Path;

use serde_json::json;

pub async fn about() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let version = duduclaw_gateway::updater::current_version();
    let about = duduclaw_gateway::device_about::collect_device_about(&version);
    serde_json::to_string_pretty(&about).map_err(|e| format!("序列化失敗：{e}"))
}

pub async fn timezone_get() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let status = duduclaw_gateway::device_about::collect_timedate().await;
    serde_json::to_string_pretty(&json!({
        "timezone": status.timezone,
        "local_time": status.local_time,
        "utc_time": status.utc_time,
        "available": status.available,
    }))
    .map_err(|e| format!("序列化失敗：{e}"))
}

pub async fn ntp_get() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let status = duduclaw_gateway::device_about::collect_timedate().await;
    serde_json::to_string_pretty(&json!({
        "ntp_enabled": status.ntp_enabled,
        "ntp_synchronized": status.ntp_synchronized,
        "available": status.available,
    }))
    .map_err(|e| format!("序列化失敗：{e}"))
}

/// `system timezone-set` core effect, run AFTER the `requires_approval` gate
/// has already cleared (`os_drive::approval::gate`) — this function itself
/// does not gate, matching `mcp_os_ops.rs`'s split between the gate and the
/// effect.
pub async fn timezone_set(home_dir: &Path, timezone: &str) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    if !duduclaw_gateway::device_about::validate_timezone_shape(timezone) {
        return Err("timezone 格式不正確（見 duduclaw_gateway::device_about::validate_timezone_shape）。".to_string());
    }
    let Some(ops) = duduclaw_gateway::device_ops::select_sysd_ops() else {
        return Err(sysd_unreachable_message());
    };
    let out = ops.set_timezone(timezone).await.map_err(|e| e.to_string())?;
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "os_drive_system",
            "system",
            duduclaw_security::audit::Severity::Info,
            json!({ "action": "timezone_set", "timezone": timezone, "success": out.success, "via": "duduclaw os system timezone-set" }),
        ),
    );
    Ok(format!("success={} stdout={:?} stderr={:?}", out.success, out.stdout, out.stderr))
}

pub async fn ntp_set(home_dir: &Path, enabled: bool) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let Some(ops) = duduclaw_gateway::device_ops::select_sysd_ops() else {
        return Err(sysd_unreachable_message());
    };
    let out = ops.set_ntp(enabled).await.map_err(|e| e.to_string())?;
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "os_drive_system",
            "system",
            duduclaw_security::audit::Severity::Info,
            json!({ "action": "ntp_set", "enabled": enabled, "success": out.success, "via": "duduclaw os system ntp-set" }),
        ),
    );
    Ok(format!("success={} stdout={:?} stderr={:?}", out.success, out.stdout, out.stderr))
}

/// Reuses `mcp_os_ops::handle_os_check_update`'s exact logic shape (system
/// self-update check always; appliance OS image update status + real
/// upstream freshness check only when `is_appliance()`) — duplicated here
/// rather than calling that `pub(crate)` MCP-shaped function directly
/// because its return type is an MCP tool-result envelope
/// (`{"content":[...],"isError":...}`), not something a plain CLI print
/// wants; the underlying calls (`updater::check_update()` /
/// `device_ops::select_device_ops().update_status()` /
/// `os_update::check_update()`) are the SAME ones, called the same way.
///
/// `device_check` (Y5-3, agent-body update vertical slice): this third front
/// door had the SAME gap `handle_os_check_update` was found to have — only
/// `device.update_status`'s local-staging-only view, never the real
/// `device.update_check` upstream-freshness signal `DevicePage.tsx`'s own
/// "check for update" button asks for. Fixed here too so the three front
/// doors (dashboard RPC / MCP tool / this CLI) answer the same question the
/// same way, not two correct answers and one stale one.
pub async fn update_check(home_dir: &Path) -> Result<String, String> {
    let system = match duduclaw_gateway::updater::check_update().await {
        Ok(info) => json!({
            "available": info.available,
            "current_version": info.current_version,
            "latest_version": info.latest_version,
            "install_method": info.install_method,
        }),
        Err(e) => json!({ "error": e }),
    };
    let (device, device_check) = if duduclaw_core::is_appliance() {
        let device = match duduclaw_gateway::device_ops::select_device_ops().update_status().await {
            Ok(out) => json!({ "success": out.success, "stdout": out.stdout, "stderr": out.stderr }),
            Err(e) => json!({ "error": e.to_string() }),
        };
        let device_check = match duduclaw_gateway::os_update::check_update(home_dir).await {
            Ok(report) => json!({
                "available": report.available,
                "current_version": report.current_version,
                "latest_version": report.latest_version,
            }),
            Err(e) => json!({ "error": { "code": e.code(), "message": e.user_message() } }),
        };
        (device, device_check)
    } else {
        let note = json!({ "note": "非 appliance 安裝，無 OS image 更新可查。" });
        (note.clone(), note)
    };
    serde_json::to_string_pretty(&json!({ "system": system, "device": device, "device_check": device_check }))
        .map_err(|e| format!("序列化失敗：{e}"))
}

fn not_appliance_message() -> String {
    "此功能僅限 DuDuClaw 裝置版（appliance image）使用。".to_string()
}

fn sysd_unreachable_message() -> String {
    "duduclaw-sysd 無法連線（appliance 檢查通過但 socket 不存在，或不是以能連上 sysd 的身分執行）。"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Off-appliance fail-closed behavior — mirrors `mcp_os_ops.rs`'s own
    // `appliance_gated_tools_fail_closed_off_appliance` discipline: never
    // flip `DUDUCLAW_APPLIANCE` in-process, so this exercises the real
    // (non-appliance) branch on every CI/dev host.

    #[tokio::test]
    async fn about_and_timedate_reads_fail_closed_off_appliance() {
        assert!(std::env::var(duduclaw_core::APPLIANCE_ENV).is_err());
        assert!(about().await.unwrap_err().contains("appliance"));
        assert!(timezone_get().await.unwrap_err().contains("appliance"));
        assert!(ntp_get().await.unwrap_err().contains("appliance"));
    }

    #[tokio::test]
    async fn writes_fail_closed_off_appliance_before_touching_sysd() {
        assert!(std::env::var(duduclaw_core::APPLIANCE_ENV).is_err());
        let home = tempfile::tempdir().unwrap();
        assert!(timezone_set(home.path(), "Asia/Taipei").await.unwrap_err().contains("appliance"));
        assert!(ntp_set(home.path(), true).await.unwrap_err().contains("appliance"));
    }

    /// `update_check` deliberately has NO appliance gate on its `system`
    /// half (mirrors `system.check_update`'s own universal availability) —
    /// it must succeed off-appliance, not refuse.
    #[tokio::test]
    async fn update_check_works_off_appliance() {
        let home = tempfile::tempdir().unwrap();
        let result = update_check(home.path()).await;
        assert!(result.is_ok(), "{result:?}");
        let text = result.unwrap();
        assert!(text.contains("\"system\""));
        // Off-appliance: `device_check` degrades to the same honest `note`
        // shape as `device` — never a fabricated freshness answer.
        assert!(text.contains("\"device_check\""));
    }
}
