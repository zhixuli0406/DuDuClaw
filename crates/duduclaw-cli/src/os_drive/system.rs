//! A7a `system` group — about/timezone/ntp/update-check.
//!
//! O16: every verb here is a thin adapter over
//! [`duduclaw_gateway::os_ops`] — the single authority the agent-facing MCP
//! tools (`mcp_os_ops.rs`) and the dashboard `device.*`/`system.*` RPCs also
//! go through. This module keeps only what is genuinely this front door's
//! own: the operator-terminal appliance fast-fail, the pretty-printed stdout
//! rendering, and the `os_drive_system` audit rows (a deliberately different
//! event name from the dashboard's `timedate_set`, so the trail can tell an
//! operator-terminal change apart from a dashboard one). See
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md` §3 for why this
//! sidesteps the dashboard WS/Ed25519 auth entirely, and §7 for the same-uid
//! boundary the two writes hit.

use std::path::Path;

use duduclaw_gateway::os_ops;
use serde_json::json;

/// Render an authority payload as this front door's pretty-printed stdout
/// text. The serialization here cannot fail (the value is already a
/// `serde_json::Value`), but the `Result` is real — degrade honestly.
fn render(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|e| format!("序列化失敗：{e}"))
}

/// Map an [`os_ops::OsOpError`] onto this front door's bare-message shape.
/// The serialization arm keeps this CLI's own already-shipped wording —
/// the one place the three `os_*` front doors word a failure differently.
fn render_err(err: os_ops::OsOpError) -> String {
    match err.serialize_detail() {
        Some(detail) => format!("序列化失敗：{detail}"),
        None => err.message(),
    }
}

fn finish(result: Result<serde_json::Value, os_ops::OsOpError>) -> Result<String, String> {
    match result {
        Ok(v) => render(v),
        Err(e) => Err(render_err(e)),
    }
}

pub async fn about() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    finish(os_ops::device_about())
}

pub async fn timezone_get() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let status = os_ops::timedate().await;
    render(json!({
        "timezone": status.timezone,
        "local_time": status.local_time,
        "utc_time": status.utc_time,
        "available": status.available,
    }))
}

pub async fn ntp_get() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let status = os_ops::timedate().await;
    render(json!({
        "ntp_enabled": status.ntp_enabled,
        "ntp_synchronized": status.ntp_synchronized,
        "available": status.available,
    }))
}

/// `system timezone-set` core effect, run AFTER the `requires_approval` gate
/// has already cleared (`os_drive::approval::gate`) — this function itself
/// does not gate, matching `mcp_os_ops.rs`'s split between the gate and the
/// effect.
pub async fn timezone_set(home_dir: &Path, timezone: &str) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let out = os_ops::set_timezone(timezone).await.map_err(render_err)?;
    audit_system_change(
        home_dir,
        json!({ "action": "timezone_set", "timezone": timezone, "success": out.success, "via": "duduclaw os system timezone-set" }),
    );
    Ok(op_output_line(&out))
}

pub async fn ntp_set(home_dir: &Path, enabled: bool) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let out = os_ops::set_ntp(enabled).await.map_err(render_err)?;
    audit_system_change(
        home_dir,
        json!({ "action": "ntp_set", "enabled": enabled, "success": out.success, "via": "duduclaw os system ntp-set" }),
    );
    Ok(op_output_line(&out))
}

/// `duduclaw os system update-check` — the brief projection of the combined
/// `{system, device, device_check}` read. The verbose projection (with
/// `release_notes`/`published_at`/`containerized`) belongs to the
/// `os_check_update` MCP tool; both come from the SAME
/// [`os_ops::check_update`] body, differing only in the named option below.
pub async fn update_check(home_dir: &Path) -> Result<String, String> {
    let report = os_ops::check_update(
        home_dir,
        os_ops::CheckUpdateOptions {
            include_release_metadata: false,
            not_appliance_note: "非 appliance 安裝，無 OS image 更新可查。",
        },
    )
    .await;
    render(report)
}

/// This front door's own audit row — `os_drive_system`, deliberately NOT the
/// dashboard's `timedate_set` (see the module doc).
fn audit_system_change(home_dir: &Path, payload: serde_json::Value) {
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "os_drive_system",
            "system",
            duduclaw_security::audit::Severity::Info,
            payload,
        ),
    );
}

fn op_output_line(out: &duduclaw_gateway::device_ops::OpOutput) -> String {
    format!(
        "success={} stdout={:?} stderr={:?}",
        out.success, out.stdout, out.stderr
    )
}

fn not_appliance_message() -> String {
    os_ops::NOT_APPLIANCE_MESSAGE.to_string()
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
        assert!(
            timezone_set(home.path(), "Asia/Taipei")
                .await
                .unwrap_err()
                .contains("appliance")
        );
        assert!(
            ntp_set(home.path(), true)
                .await
                .unwrap_err()
                .contains("appliance")
        );
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

    /// O16: this front door's wording for the (unreachable) serialize arm is
    /// its own — `render_err` must NOT leak the MCP/RPC "<what> serialize
    /// failed" phrasing, and must pass every other variant through the
    /// shared [`os_ops::OsOpError::message`].
    #[test]
    fn render_err_keeps_this_front_doors_own_serialize_wording() {
        assert_eq!(
            render_err(os_ops::OsOpError::Serialize("bad".into())),
            "序列化失敗：bad"
        );
        assert_eq!(
            render_err(os_ops::OsOpError::NotAppliance),
            os_ops::NOT_APPLIANCE_MESSAGE
        );
        assert_eq!(
            render_err(os_ops::OsOpError::Coded {
                code: "backend_unavailable".into(),
                message: os_ops::SYSD_UNREACHABLE_MESSAGE.into(),
            }),
            os_ops::SYSD_UNREACHABLE_MESSAGE
        );
    }
}
