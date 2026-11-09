//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Presentation preference only: routes and RPC authorization remain unchanged.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct DashboardConfig {
    employee_tasks: bool,
}

impl DashboardConfig {
    fn from_home(home: &std::path::Path) -> Self {
        std::fs::read_to_string(home.join("config.toml"))
            .ok()
            .map(|content| Self::from_content(&content))
            .unwrap_or_default()
    }

    fn from_content(content: &str) -> Self {
        content
            .parse::<toml::Table>()
            .ok()
            .and_then(|table| table.get("dashboard").cloned())
            .and_then(|section| section.try_into().ok())
            .unwrap_or_default()
    }
}

impl MethodHandler {
    // ── System ───────────────────────────────────────────────

    /// Build the `system.status` payload. Shared by the RPC handler and the
    /// `system.status_changed` broadcast so an open dashboard reflects a live
    /// edition change (e.g. license activation) without a manual refresh.
    pub(crate) async fn system_status_payload(&self) -> Value {
        let reg = self.registry.read().await;
        let uptime = self.start_time.elapsed().as_secs();
        let channel_map = self.channel_status.read().await;
        let channels_connected = channel_map.values().filter(|s| s.connected).count();
        drop(channel_map);
        let edition_profile = self.resolve_edition_profile().await;
        let mut payload = json!({
            "version": crate::updater::current_version(),
            "uptime_seconds": uptime,
            "agents_count": reg.list().len(),
            "channels_connected": channels_connected,
            "gateway_address": crate::deep_link::dashboard_base_url(&self.home_dir)
                .and_then(|url| url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")).map(str::to_string))
                .unwrap_or_else(|| "localhost:18889".to_string()),
            // Product form-factor (personal|enterprise). Orthogonal to the
            // license `edition` string returned by system.version. The
            // dashboard reads this to hide/show enterprise management surfaces.
            "edition_profile": edition_profile.as_str(),
            // Latest CLI quota advisory (`rate_limit_event` frame), if any —
            // e.g. "seven_day window at 92%, resets at <ts>". `null` until a
            // frame has been observed since boot. Telemetry only; never
            // implies a failure (see `rate_limit_watch`).
            "quota_warning": crate::rate_limit_watch::latest(),
            // R2 (2026-08): whether this gateway is the DuDuClaw appliance
            // image. A direct, unmodified forward of the single authority
            // (`duduclaw_core::is_appliance()`, also gating every `device.*`
            // RPC below) — never re-derived here. Unlike `device.status`,
            // `system.status` carries no `require_admin!()` gate, so this is
            // the one non-sensitive appliance signal a manager/employee
            // caller can read: just the yes/no fact, never the CPU/RAM/
            // network detail `device.status` exposes (that surface stays
            // admin-only, unchanged). The frontend's `useIsAppliance` hook
            // reads this field to decide whether to land any authenticated
            // role on the conversational console (`App.tsx::HomeLanding`).
            "is_appliance": duduclaw_core::is_appliance(),
            // 2026-09-29 feature audit (X1 落日條款): `config.toml [decision]
            // enabled = false` already 404s the whole `/api/decision/*`
            // surface (`decision_gate::decision_surface_gate`), but the
            // dashboard kept rendering the Decision Lab nav row and page, so
            // an operator who turned the line off still saw an entry that
            // could only fail. This forwards the same single authority
            // (`DecisionConfig::from_home`, read here rather than cached so a
            // config edit takes effect on the next `system.status` — the HTTP
            // gate itself still resolves at boot) so the frontend can hide
            // the row and show an honest "operator turned this off" state.
            // Absent on older gateways ⇒ the web treats it as `true`, i.e.
            // byte-identical to the behaviour before this field existed.
            "decision_enabled": crate::decision_gate::DecisionConfig::from_home(&self.home_dir).enabled,
            // Read-only employee navigation preference. Unreadable or malformed
            // config keeps task surfaces hidden; this never gates an RPC.
            "employee_tasks": DashboardConfig::from_home(&self.home_dir).employee_tasks,
            // v1.68: whether this binary was built with the `otel` feature.
            // Without it `[telemetry] otlp_endpoint` is accepted and saved but
            // exports nothing, so the settings page says so next to the field.
            "otel_compiled": cfg!(feature = "otel"),
        });
        let policy = crate::mfa::policy_status(&self.home_dir);
        if policy["degraded"] == true { payload["mfa_policy"] = policy; }
        // RM-155: present only while macOS folder access (TCC) is pending or
        // denied for a protected location this gateway uses; the dashboard
        // banner keys off its presence. Never populated off macOS (no
        // watcher runs there).
        if let Some(folder_access) = crate::macos_folder_access::status_json(&self.home_dir) {
            payload["folder_access"] = folder_access;
        }
        // RM-166: effective AI-employee identity mode, grace deadline, the
        // post-upgrade notice window and recent identity refusals by the
        // agent-file-guard hook. Admin-only (round 2, finding 3): the RPC
        // strips it for other callers ([`strip_admin_only_status`]) and so
        // does the push filter for `system.status_changed`.
        payload["identity_token"] = crate::identity_transition::status_json(&self.home_dir);
        payload
    }

    pub(crate) async fn handle_system_status(&self, ctx: &UserContext) -> WsFrame {
        let mut payload = self.system_status_payload().await;
        if !ctx.is_admin() {
            strip_admin_only_status(&mut payload);
        }
        WsFrame::ok_response("", payload)
    }

    /// Push a fresh `system.status` to every connected dashboard so live
    /// changes to the edition/license reflect without a browser refresh. The
    /// frontend `system-store` subscribes to `system.status_changed`.
    /// `pub(crate)`: also fired by the server's background edition watcher.
    pub(crate) async fn broadcast_system_status(&self) {
        let payload = self.system_status_payload().await;
        self.broadcast_event("system.status_changed", payload).await;
    }

    pub(crate) async fn handle_system_doctor(&self) -> WsFrame {
        let checks = self.run_doctor_checks().await;
        let summary = crate::os_ops::doctor_summary(&checks);
        WsFrame::ok_response("", json!({ "checks": checks, "summary": summary }))
    }

    pub(crate) async fn handle_system_doctor_repair(&self) -> WsFrame {
        let checks = self.run_doctor_checks().await;
        let summary = crate::os_ops::doctor_summary(&checks);
        let repair_hints = crate::os_ops::doctor_repair_hints(&checks);
        WsFrame::ok_response(
            "",
            json!({
                "checks": checks,
                "summary": summary,
                "repair_hints": repair_hints,
            }),
        )
    }
}

/// `system.status` fields only an Admin may read (RM-166 round 2). Shared
/// by the RPC and the `system.status_changed` push filter.
pub(crate) const ADMIN_ONLY_STATUS_FIELDS: &[&str] = &["identity_token"];

/// Remove [`ADMIN_ONLY_STATUS_FIELDS`] from a `system.status` payload.
/// Returns whether anything was removed.
pub(crate) fn strip_admin_only_status(payload: &mut Value) -> bool {
    let Some(map) = payload.as_object_mut() else { return false };
    let mut removed = false;
    for field in ADMIN_ONLY_STATUS_FIELDS {
        removed |= map.remove(*field).is_some();
    }
    removed
}

#[cfg(test)]
mod dashboard_config_tests {
    use super::DashboardConfig;

    #[test]
    fn employee_tasks_requires_explicit_boolean_true() {
        assert!(DashboardConfig::from_content("[dashboard]\nemployee_tasks = true").employee_tasks);
        for content in [
            "",
            "[dashboard]",
            "[dashboard]\nemployee_tasks = false",
            "[dashboard]\nemployee_tasks = \"true\"",
            "[dashboard]\nemployee_tasks = 1",
            "dashboard = true",
            "[dashboard",
        ] {
            assert!(!DashboardConfig::from_content(content).employee_tasks, "{content}");
        }
    }

    #[test]
    fn employee_tasks_defaults_false_when_config_cannot_be_read() {
        let home = tempfile::tempdir().unwrap();
        assert!(!DashboardConfig::from_home(home.path()).employee_tasks);
        std::fs::create_dir(home.path().join("config.toml")).unwrap();
        assert!(!DashboardConfig::from_home(home.path()).employee_tasks);
    }

    #[test]
    fn employee_tasks_reloads_the_config_without_changing_other_preferences() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "[dashboard]\nemployee_tasks = true\nother = true").unwrap();
        assert!(DashboardConfig::from_home(home.path()).employee_tasks);
        std::fs::write(&path, "[dashboard]\nemployee_tasks = false").unwrap();
        assert!(!DashboardConfig::from_home(home.path()).employee_tasks);
    }
}
