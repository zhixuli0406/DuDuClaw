//! O-0: agent-facing bridge for the dashboard-only `device.*` (WP-B) and
//! `system.*` (check_update/apply_update/doctor/status) RPCs.
//!
//! Design authority: `commercial/docs/DESIGN-agent-os-native-apps-2026-08.md`
//! §6 (NL-OS) — §6.1 core principle, §6.3 O-0, §6.4 coupling/security. Core
//! rule: **same capability, two front doors, same gate set**. Every tool
//! here reuses the SAME admin / appliance / confirm / ApprovalBroker / sysd
//! checks the dashboard `device.*`/`system.*` RPCs already enforce
//! (`duduclaw_gateway::handlers::MethodHandler::dispatch`) — never a
//! separately-derived or looser copy.
//!
//! ## Cross-process constraint (read this before touching a handler here)
//!
//! `duduclaw mcp-server` runs as its own stdio subprocess per Claude Code
//! session — a SEPARATE OS process from the long-running `duduclaw-gateway`
//! binary that owns the dashboard WebSocket RPC. Nothing here can reach
//! `MethodHandler`'s in-memory state (the connected-agent registry, the
//! `system.apply_update` `pending_update` URL cache, the WS `event_tx`
//! progress bridge) directly — only on-disk state and pure/stateless
//! functions are reachable from this process. Concretely:
//!
//! - `device.*` (WP-B) is ENTIRELY pure/file-based (sysinfo reads, shell-outs
//!   behind the `DeviceOps` trait, plain file I/O) — every tool below calls
//!   the EXACT SAME free functions the dashboard RPC handlers call
//!   (`duduclaw_gateway::device`, `device_ops::select_device_ops()` — which
//!   already picks the privilege-separated `SysdDeviceOps` on a real
//!   appliance, `backup_schedule`, `files_api`), so there is zero drift risk
//!   and privileged ops still route through `duduclaw-sysd`.
//! - `system.status` needs the LIVE gateway's in-memory registry/uptime/
//!   connected-channel state, which this process cannot read. `os_system_status`
//!   below reports a reduced, honestly-labelled payload (fresh disk-derived
//!   agent count, the `channel_status.json` snapshot the gateway already
//!   maintains for exactly this kind of cross-process read, edition profile
//!   resolved from env only) rather than fabricating the live-only fields.
//! - `system.apply_update` normally caches a trusted download/checksum URL
//!   pair in `MethodHandler::pending_update` at `check_update` time so
//!   `apply_update` never has to trust a caller-supplied URL. This process
//!   cannot share that cache, so `os_apply_update`'s `system` target instead
//!   resolves a FRESH trusted URL pair via `updater::check_update()` and
//!   applies it atomically within the same call — same "never trust a
//!   caller-supplied URL" invariant, different (still safe) mechanism. It
//!   does not go through a gateway extension's custom update provider
//!   (white-label/distributor channel) — see the function doc comment.
//!
//! Every tool is admin-scope-gated at the shared MCP dispatch choke point
//! (`mcp_auth::tool_requires_scope` → `Scope::Admin`, enforced in
//! `mcp_dispatch.rs` before this module is ever reached) and, for the
//! `device.*`-backed tools, additionally fail-closed on
//! `duduclaw_core::is_appliance()` here — mirroring the dashboard's
//! `require_admin!()` + `require_appliance!()` macros exactly. Destructive
//! ops mirror `require_confirm!()`; `os_factory_reset` additionally routes
//! through `ApprovalBroker` (a NEW gate beyond the dashboard RPC's
//! confirm-only check — see its doc comment for why).

use std::path::Path;

use serde_json::{json, Value};

// ── Small local result helpers (mirrors mcp_recording.rs's rec_text/rec_error
//    convention — each MCP submodule owns its own, not a shared import) ────

fn os_ops_text(text: &str) -> Value {
    json!({ "content": [{"type": "text", "text": text}] })
}

fn os_ops_error(msg: &str) -> Value {
    json!({ "content": [{"type": "text", "text": msg}], "isError": true })
}

/// Fail-closed `not_appliance` refusal. SAME zh-TW copy as the dashboard
/// `device.*` RPC gate (`duduclaw_gateway::handlers::device_not_appliance_frame`,
/// code `DEVICE_NOT_APPLIANCE_ERROR_CODE = "not_appliance"`) so an operator
/// sees one consistent message regardless of which front door they used.
fn not_appliance_error() -> Value {
    debug_assert_eq!(
        duduclaw_gateway::handlers::DEVICE_NOT_APPLIANCE_ERROR_CODE,
        "not_appliance"
    );
    os_ops_error("此功能僅限 DuDuClaw 裝置版（appliance image）使用。")
}

/// Fail-closed refusal for a destructive op missing `"confirm": true` — SAME
/// zh-TW copy as the dashboard `device.*` RPC gate's `require_confirm!()`.
fn confirm_required_error() -> Value {
    os_ops_error("這是不可逆的操作，請在請求參數帶上 confirm: true 再次確認執行。")
}

fn confirm_flag(args: &Value) -> bool {
    args.get("confirm").and_then(Value::as_bool) == Some(true)
}

/// Render a `device_ops::OpResult` the same shape the dashboard's
/// `device_op_result_frame` uses (`success`/`stdout`/`stderr` on success;
/// `DeviceOpError`'s own `Display` — "unsupported: …" / "io error: …" — on
/// failure, byte-identical to the dashboard's error text).
fn device_op_result_text(result: duduclaw_gateway::device_ops::OpResult) -> Value {
    match result {
        Ok(out) => os_ops_text(
            &json!({ "success": out.success, "stdout": out.stdout, "stderr": out.stderr })
                .to_string(),
        ),
        Err(e) => os_ops_error(&e.to_string()),
    }
}

/// Count agents by scanning `<home>/agents/*/agent.toml` directly — a fresh,
/// honest recomputation of the same figure the live gateway's in-memory
/// registry would report (the registry itself is populated by scanning this
/// exact directory at startup), reachable without gateway IPC.
fn count_configured_agents(home_dir: &Path) -> usize {
    std::fs::read_dir(home_dir.join("agents"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("agent.toml").is_file())
                .count()
        })
        .unwrap_or(0)
}

// ── Read-only tools (admin scope only; device.*-backed ones also require
//    is_appliance()) ─────────────────────────────────────────────────────

/// `os_device_status` → `device.status`. Same collector
/// (`duduclaw_gateway::device::collect_status`), same appliance gate.
pub(crate) async fn handle_os_device_status(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let status = duduclaw_gateway::device::collect_status(home_dir);
    match serde_json::to_value(&status) {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&format!("device status serialize failed: {e}")),
    }
}

/// `os_network_info` → `device.network` (read path only — the RPC's
/// static-IP write path is `not_implemented` there too, so this tool never
/// exposes a write shape at all).
pub(crate) async fn handle_os_network_info() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    match serde_json::to_value(duduclaw_gateway::device::collect_network()) {
        Ok(v) => os_ops_text(&json!({ "interfaces": v }).to_string()),
        Err(e) => os_ops_error(&format!("network interfaces serialize failed: {e}")),
    }
}

/// `os_backup_list` → `device.backup_list`. Same two calls
/// (`backup_schedule::backups_dir` + `files_api::list_files`) as the
/// dashboard handler — that handler is already a 3-line wrapper over these,
/// so this is not a second implementation, it's the same one.
pub(crate) async fn handle_os_backup_list(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let dir = duduclaw_gateway::backup_schedule::backups_dir(home_dir);
    let files = duduclaw_gateway::files_api::list_files(&dir);
    match serde_json::to_value(&files) {
        Ok(v) => os_ops_text(&json!({ "files": v }).to_string()),
        Err(e) => os_ops_error(&format!("backup list serialize failed: {e}")),
    }
}

/// `os_system_status` — reduced, honestly-labelled counterpart to
/// `system.status`. See the module doc comment for exactly which fields are
/// unavailable cross-process and why (`uptime_seconds` / live connection
/// counts require the running gateway's in-memory state).
///
/// Unlike the four `device.*`-backed read tools, `system.status` itself has
/// NO appliance requirement in the dashboard RPC (it works on every
/// install) — this tool mirrors that: it always runs, `is_appliance()` is
/// reported as a plain field, not a gate.
pub(crate) async fn handle_os_system_status(home_dir: &Path) -> Value {
    let version = duduclaw_gateway::updater::current_version();
    let agents_count = count_configured_agents(home_dir);
    let is_appliance = duduclaw_core::is_appliance();
    // Edition profile: env-var override only (no `tier_key`) — the license
    // runtime tier lives in the live gateway process's in-memory cache and
    // is unreachable here. `config`-layer override is also unreachable (it
    // is the dashboard's own runtime toggle, not a file), so this can read
    // narrower than the dashboard's `system.status` in that one case —
    // documented, not silently approximated.
    let edition_profile = duduclaw_core::EditionProfile::resolve_from_env(None, None);

    // channels_connected: read the gateway-maintained `channel_status.json`
    // snapshot — the SAME cross-process bridge the existing `channel_status`
    // MCP tool already relies on (`handle_channel_status` in `mcp.rs`).
    // `None` (not `0`) when the gateway has never written a snapshot, so a
    // fresh/never-booted gateway is never misreported as "zero channels".
    let channels_connected: Option<usize> = std::fs::read_to_string(home_dir.join("channel_status.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("channels").and_then(|c| c.as_object()).map(|m| {
            m.values()
                .filter(|s| s.get("connected").and_then(Value::as_bool) == Some(true))
                .count()
        }));

    os_ops_text(
        &json!({
            "version": version,
            "agents_count": agents_count,
            "appliance": is_appliance,
            "edition_profile": edition_profile.as_str(),
            "channels_connected": channels_connected,
            "note": "跨程序限制：uptime_seconds 等僅存在於執行中 gateway 行程記憶體的欄位本工具無法回報；\
                     channels_connected 讀自 gateway 定期寫出的 channel_status.json 快照（可能有數秒延遲），\
                     null 代表 gateway 尚未寫出過快照。完整即時狀態請用 dashboard 的 system.status。",
        })
        .to_string(),
    )
}

/// `os_check_update` → `system.check_update` (always) + `device.update_status`
/// (only when `is_appliance()` — omitted, not hard-blocked, when not, since
/// this tool intentionally combines two independently-scoped RPCs into one
/// read and the system half is universally available).
///
/// Deliberately omits `download_url`/`checksum_url` from the `system` half
/// (present in the dashboard RPC's response): `os_apply_update`'s `system`
/// target re-resolves its own trusted URL pair rather than accepting one
/// from a caller (including this tool's own prior output) — see that
/// function's doc comment for the M2 "never trust a caller-supplied URL"
/// invariant this preserves.
pub(crate) async fn handle_os_check_update() -> Value {
    let system = match duduclaw_gateway::updater::check_update().await {
        Ok(info) => json!({
            "available": info.available,
            "current_version": info.current_version,
            "latest_version": info.latest_version,
            "release_notes": info.release_notes,
            "published_at": info.published_at,
            "install_method": info.install_method,
            "containerized": duduclaw_gateway::updater::is_containerized(),
        }),
        Err(e) => json!({ "error": e }),
    };
    let device = if duduclaw_core::is_appliance() {
        match duduclaw_gateway::device_ops::select_device_ops()
            .update_status()
            .await
        {
            Ok(out) => json!({ "success": out.success, "stdout": out.stdout, "stderr": out.stderr }),
            Err(e) => json!({ "error": e.to_string() }),
        }
    } else {
        json!({ "note": "非 appliance 安裝，無 OS image 更新可查（device.update_status 僅限 appliance）。" })
    };
    os_ops_text(&json!({ "system": system, "device": device }).to_string())
}

/// `os_doctor_repair` — reduced counterpart to `system.doctor_repair`.
/// Reuses the exact same three-source API-key check
/// (`duduclaw_gateway::handlers::has_api_key_configured`) and the exact same
/// MCP cold-start probe + status/message renderer + repair-hint text
/// (`duduclaw_gateway::doctor_probes`) as the dashboard RPC. Deliberately
/// omits the dashboard's `container_runtime` (docker) and `grok_cli` probes:
/// neither is gate-relevant, and their underlying checks
/// (`handlers::check_docker`) are private to the gateway crate and tied to
/// heavier live subprocess probing not worth duplicating for an agent tool.
/// Full diagnostics remain available via the dashboard or `duduclaw doctor`.
pub(crate) async fn handle_os_doctor_repair(home_dir: &Path) -> Value {
    let config_exists = home_dir.join("config.toml").exists();
    let has_agents = count_configured_agents(home_dir) > 0;
    let has_key = duduclaw_gateway::handlers::has_api_key_configured(home_dir).await;
    let mcp_report = duduclaw_gateway::doctor_probes::mcp_cold_start_probe(home_dir).await;
    let (mcp_status, mcp_message) = duduclaw_gateway::doctor_probes::mcp_cold_start_status_and_message(
        &mcp_report.outcome,
        mcp_report.provision_error.as_deref(),
    );

    let checks = vec![
        json!({
            "name": "config_file",
            "status": if config_exists { "pass" } else { "fail" },
            "message": if config_exists { "config.toml exists" } else { "config.toml not found" },
        }),
        json!({
            "name": "agents",
            "status": if has_agents { "pass" } else { "warn" },
            "message": if has_agents { "Agents found" } else { "No agents found" },
        }),
        json!({
            "name": "api_key",
            "status": if has_key { "pass" } else { "warn" },
            "message": if has_key { "ANTHROPIC_API_KEY is set" } else { "ANTHROPIC_API_KEY not set" },
        }),
        json!({ "name": "mcp_server", "status": mcp_status, "message": mcp_message }),
    ];
    let repair_hints: Vec<Value> = checks
        .iter()
        .filter(|c| c["status"] != "pass")
        .map(|c| {
            let name = c["name"].as_str().unwrap_or("unknown");
            json!({ "check": name, "hint": duduclaw_gateway::handlers::doctor_repair_hint(name) })
        })
        .collect();
    let pass = checks.iter().filter(|c| c["status"] == "pass").count();
    let warn = checks.iter().filter(|c| c["status"] == "warn").count();
    let fail = checks.iter().filter(|c| c["status"] == "fail").count();

    os_ops_text(
        &json!({
            "checks": checks,
            "summary": { "pass": pass, "warn": warn, "fail": fail },
            "repair_hints": repair_hints,
            "note": "省略 dashboard system.doctor 的 container_runtime/grok_cli 探測（非閘相關，需較重的子行程探測）。",
        })
        .to_string(),
    )
}

// ── Change / destructive tools ───────────────────────────────────────────

/// `os_backup_create` → `device.backup_create`. Calls the SAME free function
/// (`duduclaw_gateway::handlers::create_device_backup_archive`) the dashboard
/// handler itself was refactored to call — genuinely one implementation, two
/// callers, not a second copy.
pub(crate) async fn handle_os_backup_create(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    use duduclaw_gateway::handlers::DeviceBackupOutcome;
    match duduclaw_gateway::handlers::create_device_backup_archive(home_dir).await {
        DeviceBackupOutcome::Created { filename, stdout, stderr } => os_ops_text(
            &json!({ "filename": filename, "stdout": stdout, "stderr": stderr }).to_string(),
        ),
        DeviceBackupOutcome::OpFailed(out) => os_ops_text(
            &json!({ "success": false, "stdout": out.stdout, "stderr": out.stderr }).to_string(),
        ),
        DeviceBackupOutcome::OpError(e) => os_ops_error(&e.to_string()),
        DeviceBackupOutcome::MoveFailed(msg) => os_ops_error(&msg),
    }
}

/// `os_power` → `device.power`. Same admin + appliance + confirm gate as
/// the dashboard RPC (`require_admin!()` + `require_appliance!()` +
/// `require_confirm!()`); NOT additionally approval-gated — restart/shutdown
/// is recoverable (the device comes back up), unlike `os_factory_reset`.
pub(crate) async fn handle_os_power(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let result = match action {
        "restart" => duduclaw_gateway::device_ops::select_device_ops().reboot().await,
        "shutdown" => duduclaw_gateway::device_ops::select_device_ops().poweroff().await,
        _ => return os_ops_error("action 必須是 \"restart\" 或 \"shutdown\""),
    };
    device_op_result_text(result)
}

/// TTL for the `os_factory_reset` approval — mirrors `INSTALL_APPROVAL_TTL_SECONDS`
/// in `mcp.rs` (5 minutes: a realistic window for a human to see the push
/// notification and decide before it auto-denies).
const FACTORY_RESET_APPROVAL_TTL_SECONDS: i64 = 300;
/// Poll cadence while blocking on the decision — mirrors `INSTALL_APPROVAL_POLL`.
const FACTORY_RESET_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// `os_factory_reset` → `device.factory_reset`. Same admin + appliance +
/// confirm gate as the dashboard RPC, PLUS a NEW `ApprovalBroker` gate not
/// present on the RPC itself.
///
/// Why the addition: the dashboard's `"confirm": true` param is meaningful
/// because a HUMAN is physically clicking through the dashboard's confirm
/// dialog — the human-in-the-loop already happened by the time the RPC
/// fires. An agent-issued `confirm: true` carries no such guarantee (the
/// agent decided it itself). Factory reset wipes device state and is
/// genuinely irreversible, so per design §6.1 ("不可逆走 ApprovalBroker")
/// this front door requires a live human decision via the SAME
/// `ApprovalBroker` + `run_install_approval` polling primitive
/// `crate::mcp::gate_install_approval` already uses for install-class tools
/// — fail-closed on broker-unavailable / denial / expiry, identical
/// semantics, not a re-derived approximation.
pub(crate) async fn handle_os_factory_reset(args: &Value, home_dir: &Path, caller_client_id: &str) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    if let Err(msg) = require_factory_reset_approval(home_dir, caller_client_id).await {
        return os_ops_error(&msg);
    }
    device_op_result_text(
        duduclaw_gateway::device_ops::select_device_ops()
            .factory_reset(home_dir)
            .await,
    )
}

async fn require_factory_reset_approval(home_dir: &Path, caller_client_id: &str) -> Result<(), String> {
    use duduclaw_gateway::approval::ApprovalBroker;

    let broker = match ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "os_factory_reset: ApprovalBroker unavailable — denying (fail-closed)");
            return Err(
                "審批系統暫時無法使用，已拒絕 factory reset（fail-closed）。請稍後再試或由管理員經 dashboard 執行。"
                    .to_string(),
            );
        }
    };
    require_factory_reset_approval_via(&broker, caller_client_id).await
}

/// Broker-injected inner half of [`require_factory_reset_approval`] — split
/// out purely for testability (mirrors `mcp.rs`'s own
/// `gate_install_approval` → `run_install_approval` split): tests drive this
/// with an in-memory `ApprovalBroker` instead of a disk-backed one opened
/// twice from two independent connections (the production caller and a test
/// "decider" task each `ApprovalBroker::open`-ing the same path was found to
/// race under load during O-0 test development — using one shared,
/// in-process broker instance removes that cross-connection variable
/// entirely, exactly like the existing `approval_granted_proceeds`/
/// `approval_denied_blocks` tests in `mcp.rs` already do).
async fn require_factory_reset_approval_via(
    broker: &duduclaw_gateway::approval::ApprovalBroker,
    caller_client_id: &str,
) -> Result<(), String> {
    let agent_id = if caller_client_id.is_empty() { "unknown-agent" } else { caller_client_id };
    match crate::mcp::run_install_approval(
        broker,
        agent_id,
        "系統操作員 agent 要求執行 factory reset：清除裝置狀態並於下次開機重新佈建。",
        json!({ "tool": "os_factory_reset" }),
        FACTORY_RESET_APPROVAL_TTL_SECONDS,
        FACTORY_RESET_APPROVAL_POLL,
    )
    .await
    {
        crate::mcp::InstallApprovalOutcome::Proceed => Ok(()),
        crate::mcp::InstallApprovalOutcome::Denied(msg) => Err(msg),
    }
}

/// `os_apply_update` — bridges TWO independent RPCs behind one `target`
/// param (required — the two operations have very different blast radii, so
/// this tool never silently guesses which one was meant):
/// - `"device"` → `device.update_apply` (appliance-only OS image update via
///   `systemd-sysupdate`, through `duduclaw-sysd` on a real appliance). Same
///   admin + appliance gate as the RPC; the RPC itself has no confirm, so
///   neither does this.
/// - `"system"` → `system.apply_update` (duduclaw's own binary self-update).
///   Same admin-only gate as the RPC (no appliance requirement — this works
///   on every install). See [`apply_system_update`] for how it preserves the
///   RPC's "never trust a caller-supplied URL" invariant without the
///   cross-process `pending_update` cache.
pub(crate) async fn handle_os_apply_update(args: &Value, home_dir: &Path) -> Value {
    let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
    match target {
        "device" => {
            if !duduclaw_core::is_appliance() {
                return not_appliance_error();
            }
            device_op_result_text(
                duduclaw_gateway::device_ops::select_device_ops()
                    .update_apply()
                    .await,
            )
        }
        "system" => apply_system_update(home_dir).await,
        _ => os_ops_error(
            "target 必須是 \"device\"（appliance OS image 更新，經 duduclaw-sysd）或 \
             \"system\"（duduclaw 本體自我更新）。",
        ),
    }
}

/// `system.apply_update`'s core effect (download + verify + swap the running
/// binary), reached without the dashboard RPC's `MethodHandler::pending_update`
/// cache: this call resolves a FRESH trusted `(download_url, checksum_url)`
/// pair via `updater::check_update()` — the same trusted resolution the
/// dashboard RPC's `check_update` step performs — and applies it
/// immediately, in the same call, so no untrusted URL is ever accepted from
/// an argument.
///
/// Known gap (documented, not silently papered over): this does NOT consult
/// `self.extension.update_provider()` — a gateway-extension-supplied custom
/// update channel some white-label/distributor builds configure. Those
/// deployments' `os_apply_update(target="system")` falls back to the OSS
/// default GitHub/control-plane channel instead. Reaching the extension's
/// provider would require exposing its resolution to a cross-process
/// reader, which is out of scope for the O-0 tool-face skeleton.
async fn apply_system_update(home_dir: &Path) -> Value {
    let info = match duduclaw_gateway::updater::check_update().await {
        Ok(info) => info,
        Err(e) => return os_ops_error(&format!("更新檢查失敗：{e}")),
    };
    if !info.available {
        return os_ops_text("目前已是最新版本，無需更新。");
    }

    // Same audit event kind/shape as the dashboard RPC ("system_update"/
    // "apply") so existing audit tooling picks up an agent-initiated update
    // identically to a human-initiated one.
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "system_update",
            "system",
            duduclaw_security::audit::Severity::Info,
            json!({ "action": "apply", "target_version": info.latest_version, "via": "os_apply_update" }),
        ),
    );

    let no_progress = |_p: duduclaw_gateway::updater::UpdateProgress| {};
    match duduclaw_gateway::updater::apply_update_with_progress(
        &info.download_url,
        &info.checksum_url,
        &no_progress,
    )
    .await
    {
        Ok(res) => match serde_json::to_value(&res) {
            Ok(v) => os_ops_text(&json!({ "applied": true, "version": info.latest_version, "result": v }).to_string()),
            Err(e) => os_ops_text(&format!("更新已套用（version={}），但結果序列化失敗：{e}", info.latest_version)),
        },
        Err(e) => os_ops_error(&format!("更新套用失敗：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ── Pure helpers ─────────────────────────────────────────────────────

    #[test]
    fn confirm_flag_only_accepts_literal_true() {
        assert!(!confirm_flag(&json!({})));
        assert!(!confirm_flag(&json!({"confirm": false})));
        assert!(!confirm_flag(&json!({"confirm": "true"})), "a string \"true\" must not satisfy the gate");
        assert!(confirm_flag(&json!({"confirm": true})));
    }

    #[test]
    fn not_appliance_error_is_fail_closed_shape() {
        let v = not_appliance_error();
        assert_eq!(v["isError"], true);
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("appliance"), "unexpected message: {text}");
    }

    #[test]
    fn confirm_required_error_is_fail_closed_shape() {
        let v = confirm_required_error();
        assert_eq!(v["isError"], true);
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("confirm"), "unexpected message: {text}");
    }

    #[test]
    fn device_op_result_text_maps_success_and_error_variants() {
        use duduclaw_gateway::device_ops::{DeviceOpError, OpOutput};

        let ok = device_op_result_text(Ok(OpOutput {
            success: true,
            stdout: "done".to_string(),
            stderr: String::new(),
        }));
        assert_ne!(ok["isError"], true);

        let unsupported = device_op_result_text(Err(DeviceOpError::Unsupported("nope".to_string())));
        assert_eq!(unsupported["isError"], true);
        assert!(unsupported["content"][0]["text"].as_str().unwrap().contains("unsupported"));

        let io_err = device_op_result_text(Err(DeviceOpError::Io("disk full".to_string())));
        assert_eq!(io_err["isError"], true);
        assert!(io_err["content"][0]["text"].as_str().unwrap().contains("io error"));
    }

    #[test]
    fn count_configured_agents_counts_only_dirs_with_agent_toml() {
        let home = tmp_home();
        let agents = home.path().join("agents");
        std::fs::create_dir_all(agents.join("a")).unwrap();
        std::fs::write(agents.join("a").join("agent.toml"), "").unwrap();
        std::fs::create_dir_all(agents.join("b")).unwrap(); // no agent.toml — must not count
        assert_eq!(count_configured_agents(home.path()), 1);
    }

    // ── Appliance fail-closed gate ──────────────────────────────────────
    //
    // Mirrors `duduclaw_gateway::handlers`'s own
    // `all_device_methods_fail_closed_off_appliance` test: never flips
    // `DUDUCLAW_APPLIANCE` in-process (that test's doc comment explains why
    // — avoids any risk of racing other tests in the same process that read
    // it). CI/dev hosts are never appliances, so this path is exercised on
    // every run; the "on appliance" branches reuse the exact same
    // `device_ops`/`device` free functions the dashboard RPC handlers call,
    // whose own behavior is covered by `device_ops.rs`/`device.rs`'s tests.

    #[tokio::test]
    async fn appliance_gated_tools_fail_closed_off_appliance() {
        assert!(
            std::env::var(duduclaw_core::APPLIANCE_ENV).is_err(),
            "precondition: DUDUCLAW_APPLIANCE must be unset in the test process"
        );
        let home = tmp_home();

        let results = vec![
            handle_os_device_status(home.path()).await,
            handle_os_network_info().await,
            handle_os_backup_list(home.path()).await,
            handle_os_backup_create(home.path()).await,
            handle_os_power(&json!({"action": "restart", "confirm": true})).await,
            handle_os_factory_reset(&json!({"confirm": true}), home.path(), "sysop").await,
            handle_os_apply_update(&json!({"target": "device"}), home.path()).await,
        ];
        for v in &results {
            assert_eq!(v["isError"], true, "{v:?}");
            let text = v["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("appliance"), "unexpected message: {text}");
        }
    }

    /// `os_system_status` has NO appliance requirement (`system.status`
    /// doesn't either) — it must succeed off-appliance, not refuse.
    ///
    /// `os_doctor_repair` is deliberately NOT exercised end-to-end here: it
    /// calls the live `doctor_probes::mcp_cold_start_probe`, which spawns a
    /// real `duduclaw mcp-server` subprocess — `doctor_probes.rs`'s own test
    /// module only ever unit-tests the pure `classify_mcp_cold_start`
    /// classifier and never invokes the live probe either, for the same
    /// reason (needs the built binary, live subprocess spawn, not a fast/
    /// deterministic unit test). `handle_os_doctor_repair`'s other three
    /// checks (config_file/agents/api_key) and the repair-hint mapping are
    /// covered indirectly by `count_configured_agents`'s test above and by
    /// reusing `duduclaw_gateway::handlers::{has_api_key_configured,
    /// doctor_repair_hint}` — the SAME functions `duduclaw-gateway`'s own
    /// test suite covers.
    #[tokio::test]
    async fn system_status_works_off_appliance() {
        let home = tmp_home();
        std::fs::create_dir_all(home.path().join("agents")).unwrap();

        let status = handle_os_system_status(home.path()).await;
        assert_ne!(status["isError"], true, "{status:?}");
    }

    #[tokio::test]
    async fn apply_update_rejects_unknown_or_missing_target() {
        let home = tmp_home();
        let unknown = handle_os_apply_update(&json!({"target": "nonsense"}), home.path()).await;
        assert_eq!(unknown["isError"], true);
        assert!(unknown["content"][0]["text"].as_str().unwrap().contains("target"));

        let missing = handle_os_apply_update(&json!({}), home.path()).await;
        assert_eq!(missing["isError"], true);
    }

    // ── ApprovalBroker gate for os_factory_reset ────────────────────────
    //
    // `require_factory_reset_approval_via` (the broker-injected inner half
    // of `require_factory_reset_approval`, split out for exactly this) is
    // driven with a SHARED in-memory `ApprovalBroker` — same technique
    // `mcp.rs`'s own `approval_granted_proceeds`/`approval_denied_blocks`
    // tests use for `run_install_approval`. An earlier version of these
    // tests opened a SECOND, independent disk-backed `ApprovalBroker` for
    // the decider task (pointed at the same tempdir path) and was observed
    // to hang for the full TTL on the "granted" case under load — sharing
    // one in-process broker instance (cloned, `Arc`-backed store) removes
    // that cross-connection variable entirely.

    fn in_mem_broker() -> duduclaw_gateway::approval::ApprovalBroker {
        duduclaw_gateway::approval::ApprovalBroker::new(std::sync::Arc::new(
            duduclaw_gateway::approval::ApprovalStore::open_in_memory().unwrap(),
        ))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn factory_reset_approval_granted_proceeds() {
        let broker = in_mem_broker();
        let decider = broker.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(pending) = decider.list_pending(Some("sysop")).await {
                    if let Some(rec) = pending.first() {
                        decider.decide(&rec.id, true, "dashboard:admin").await.unwrap();
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let result = require_factory_reset_approval_via(&broker, "sysop").await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn factory_reset_approval_denied_blocks() {
        let broker = in_mem_broker();
        let decider = broker.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(pending) = decider.list_pending(Some("sysop")).await {
                    if let Some(rec) = pending.first() {
                        decider.decide(&rec.id, false, "dashboard:admin").await.unwrap();
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let result = require_factory_reset_approval_via(&broker, "sysop").await;
        match result {
            Err(msg) => assert!(msg.contains("拒絕"), "got: {msg}"),
            Ok(()) => panic!("denied approval must NOT proceed"),
        }
    }

    /// `require_factory_reset_approval` (the outer, `ApprovalBroker::open`
    /// wrapper) is exercised separately, once, purely for the fail-closed
    /// "broker unavailable" path — pointing it at a path that cannot hold a
    /// SQLite file (a plain file, not a directory) so `ApprovalBroker::open`
    /// itself errors, without waiting on any decision loop at all.
    #[tokio::test]
    async fn broker_unavailable_denies_fail_closed() {
        let home = tmp_home();
        let not_a_dir = home.path().join("not-a-directory");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let result = require_factory_reset_approval(&not_a_dir, "sysop").await;
        match result {
            Err(msg) => assert!(msg.contains("拒絕"), "got: {msg}"),
            Ok(()) => panic!("broker-open failure must fail closed, not proceed"),
        }
    }
}
