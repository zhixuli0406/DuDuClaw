//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

impl MethodHandler {
    pub(crate) async fn dispatch_system(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
        conn: crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        // ── ACL macros ───────────────────────────────────────
        // Helper: require minimum role, return error frame on failure.
        macro_rules! require_admin {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Admin) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        macro_rules! require_manager {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Manager) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        // Helper: check agent access from params, return error frame on failure.
        macro_rules! check_agent {
            ($min_level:expr) => {
                match acl::extract_and_check_agent(ctx, &params, $min_level) {
                    Ok(id) => id,
                    Err(e) => return WsFrame::error_response("", &e),
                }
            };
        }

        match method {
            "finetune.datasets.delete" => {
                require_admin!();
                let id = params.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
                finetune_frame(
                    crate::finetune::dataset::delete(&self.home_dir, id)
                        .map(|()| json!({ "deleted": true })),
                )
            }
            "finetune.datasets.build" => {
                require_admin!();
                let id = params.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
                let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("sharegpt");
                let prefs = params
                    .get("preference_pairs")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let sources: crate::finetune::dataset::DatasetSources = params
                    .get("sources")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .unwrap_or_default()
                    .unwrap_or_default();
                finetune_frame(
                    crate::finetune::dataset::build(&self.home_dir, id, sources, format, prefs)
                        .map(|d| json!({ "dataset": d })),
                )
            }
            "finetune.datasets.preview" => {
                require_admin!();
                let id = params.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
                let n = params.get("n").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                finetune_frame(crate::finetune::dataset::preview(&self.home_dir, id, n))
            }
            // Privacy gate: naming the on-disk path is the first step of
            // moving curated customer data off this machine, so the caller
            // must acknowledge that explicitly. Absent field = refusal.
            "finetune.datasets.export" => {
                require_admin!();
                let id = params.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
                let ack = params
                    .get("acknowledged_data_leaves_device")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                finetune_frame(crate::finetune::dataset::export(&self.home_dir, id, ack))
            }
            "finetune.jobs.list" => {
                require_admin!();
                finetune_frame(
                    crate::finetune::jobs::list(&self.home_dir).map(|j| json!({ "jobs": j })),
                )
            }
            "finetune.jobs.create" => {
                require_admin!();
                let ack = params
                    .get("acknowledged_data_leaves_device")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match crate::finetune::jobs::config_from_params(&params) {
                    Ok(cfg) => finetune_frame(
                        crate::finetune::jobs::create(&self.home_dir, cfg, ack)
                            .await
                            .map(|j| json!({ "job": j })),
                    ),
                    Err(e) => finetune_error_frame(&e),
                }
            }
            "finetune.jobs.status" => {
                require_admin!();
                let id = params.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
                finetune_frame(
                    crate::finetune::jobs::status(&self.home_dir, id)
                        .await
                        .map(|j| json!({ "job": j })),
                )
            }
            "finetune.jobs.cancel" => {
                require_admin!();
                let id = params.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
                finetune_frame(
                    crate::finetune::jobs::cancel(&self.home_dir, id)
                        .await
                        .map(|j| json!({ "job": j })),
                )
            }
            // Import moves data ONTO the box, so it is not gated.
            "finetune.import" => {
                require_admin!();
                let src = params
                    .get("path_or_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                finetune_frame(crate::finetune::import::import(&self.home_dir, src).await)
            }

            "runtime.detect" => self.handle_runtime_detect().await,
            // ── WP2 / D16: onboarding one-click CLI install ──────────────
            // Admin-only. The only accepted parameter is a provider NAME,
            // matched against a hard-coded whitelist in `runtime_install.rs`;
            // the command that runs is a compile-time constant. See that
            // module's header for the full security model.
            "runtime.install" => {
                require_admin!();
                self.handle_runtime_install(params, ctx).await
            }
            "system.config" => {
                require_admin!();
                self.handle_system_config().await
            }
            "system.update_config" => {
                require_admin!();
                self.handle_system_update_config(params, ctx).await
            }
            // ── v1.68: raw config editor (admin; masked read, validated +
            //    backed-up + audited write) ──
            "config.raw.get" => {
                require_admin!();
                self.handle_config_raw_get(params).await
            }
            "config.raw.set" => {
                require_admin!();
                self.handle_config_raw_set(params, ctx).await
            }
            // ── WP21 §2.8: delegation permissions (owner/admin only) ─────
            // Who may hand work to whom, org-wide. Admin-gated like every
            // other config-writing RPC; the write takes effect on the next
            // delegation decision (enforcement re-reads config.toml).
            "delegation.get" => {
                require_admin!();
                self.handle_delegation_get().await
            }
            "delegation.set" => {
                require_admin!();
                self.handle_delegation_set(params, ctx).await
            }
            // ── v1.54: 校準式 forward model + held-out 學習閘 全域開關 ─────
            // Global `[task_forward_model]` toggles. Admin-gated like every
            // other config-writing RPC. `calibration_enabled` /
            // `held_out_gate_enabled` are re-read per task settle/consolidate,
            // so they take effect on the next task with no restart; the
            // `enabled` master switch also gates a forward-model predict hook
            // constructed once at gateway startup, so flipping it fully takes
            // effect after a restart (see `handle_task_forward_model_set`).
            "task_forward_model.get" => {
                require_admin!();
                self.handle_task_forward_model_get().await
            }
            "task_forward_model.set" => {
                require_admin!();
                self.handle_task_forward_model_set(params, ctx).await
            }
            "system.autostart.status" => {
                require_admin!();
                self.handle_system_autostart_status().await
            }
            "system.autostart.set" => {
                require_admin!();
                self.handle_system_autostart_set(params).await
            }
            "system.version" => self.handle_system_version().await,
            "system.check_update" => {
                require_admin!();
                self.handle_system_check_update().await
            }
            "system.apply_update" => {
                require_admin!();
                self.handle_system_apply_update(params).await
            }

            // ── Logs (manager+) ──────────────────────────────
            "logs.subscribe" => {
                require_manager!();
                self.handle_logs_subscribe(params)
            }
            "logs.unsubscribe" => self.handle_logs_unsubscribe(params),

            // ── Security (admin only) ────────────────────────
            "security.audit_log" => {
                require_admin!();
                self.handle_security_audit_log(params).await
            }
            "audit.unified_log" => {
                require_admin!();
                self.handle_audit_unified_log(params).await
            }
            "audit.evolution_query" => {
                require_admin!();
                self.handle_audit_evolution_query(params).await
            }
            "audit.reliability_summary" => {
                require_admin!();
                self.handle_audit_reliability_summary(params).await
            }
            "security.status" => {
                require_admin!();
                self.handle_security_status().await
            }
            // WP-K: dashboard "credential hygiene" friendly cleanup surface —
            // detect plaintext credentials in config.toml (paths only, never
            // values) and remove ONLY the ones with a confirmed `_enc` twin.
            "security.credential_hygiene" => {
                require_admin!();
                self.handle_security_credential_hygiene().await
            }
            // WP-H1 P1: the structured credential list — every field's
            // `describe()` verdict (source / writable / residue), no values.
            "security.credential_inventory" => {
                require_admin!();
                self.handle_security_credential_inventory().await
            }
            "security.credential_cleanup" => {
                require_admin!();
                self.handle_security_credential_cleanup(ctx).await
            }

            // ── Security Audit (secaudit dashboard, DESIGN-code-security-
            // audit-2026-08 §3.1, manager+) — reads/reviews reports written
            // by `duduclaw secaudit --save`. Read RPCs and the human-review
            // write RPC are all manager+ (same bar as `analytics.*` below,
            // not the admin-only `security.*` credential surfaces above).
            "secaudit.reports" => {
                require_manager!();
                self.handle_secaudit_reports().await
            }
            "secaudit.report" => {
                require_manager!();
                self.handle_secaudit_report(params).await
            }
            "secaudit.finding_status" => {
                require_manager!();
                self.handle_secaudit_finding_status(params, ctx).await
            }

            // ── Analytics (manager+) ────────────────────────
            "analytics.summary" => {
                require_manager!();
                self.handle_analytics_summary(params).await
            }
            "analytics.conversations" => {
                require_manager!();
                self.handle_analytics_conversations().await
            }
            "analytics.cost_savings" => {
                require_manager!();
                self.handle_analytics_cost_savings().await
            }

            // ── Heartbeat (manager+) ─────────────────────────
            "heartbeat.status" => {
                require_manager!();
                self.handle_heartbeat_status().await
            }
            "heartbeat.trigger" => {
                require_manager!();
                self.handle_heartbeat_trigger(params).await
            }

            // ── Evolution (manager+, H3 fix) ─────────────────
            "evolution.status" => {
                require_manager!();
                self.handle_evolution_status().await
            }
            // Evolution v3 dashboard convergence (WP: TODO-evolution-v3-2026-08.md
            // §"dashboard 統一收斂"). Same access bar as the rest of the
            // `evolution.*` family — manager+ only, optional `agent_id` scopes to
            // one agent server-side without an extra ACL round-trip.
            "evolution.stagnation" => {
                require_manager!();
                self.handle_evolution_stagnation(params).await
            }
            "evolution.telemetry" => {
                require_manager!();
                self.handle_evolution_telemetry(params).await
            }
            // ── Forward model / calibration views (manager+) — the v1.53/54
            //    predict-act-verify layer's first dashboard surface. Generic:
            //    reads only the platform store (prediction.db), any agent. ──
            "forward.summary" => {
                require_manager!();
                self.handle_forward_summary(params).await
            }
            "forward.recent" => {
                require_manager!();
                self.handle_forward_recent(params, ctx).await
            }
            "forward.chain" => {
                require_manager!();
                self.handle_forward_chain(params, ctx).await
            }
            "forward.calibration" => {
                require_manager!();
                self.handle_forward_calibration(params).await
            }
            "forward.states" => {
                require_manager!();
                self.handle_forward_states(params).await
            }

            // ── Belief Loop (WP4, design-market-belief-loop-2026-08)
            //    — external-world belief bookkeeping, parallel to
            //    the task forward model above. Same access bar / fail-open
            //    shape as forward.*: manager+ only, reads prediction.db. ──
            "belief.recent" => {
                require_manager!();
                self.handle_belief_recent(params).await
            }
            "belief.summary" => {
                require_manager!();
                self.handle_belief_summary(params).await
            }

            // ── Playbook (agent-scoped gene-shaped experience entries) ──
            // Read surfaces mirror the `memory.*` H2-fix ACL shape (Viewer);
            // `retire` is a destructive-but-recoverable per-entry mutation, so
            // it takes the same Owner bar as `memory.forget`.
            "playbook.list" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_playbook_list(params).await
            }
            "playbook.export" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_playbook_export(params).await
            }
            "playbook.retire" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_playbook_retire(params).await
            }

            // ── Cost / cache-efficiency telemetry (admin only) ──
            "cost.summary" => {
                require_admin!();
                self.handle_cost_summary(params).await
            }
            "cost.agents" => {
                require_admin!();
                self.handle_cost_agents(params).await
            }
            "cost.recent" => {
                require_admin!();
                self.handle_cost_recent(params).await
            }
            // WP-A2: per-model rollup. Same admin gate as its `cost.*`
            // siblings — it exposes the same spend data, sliced by model.
            "cost.by_model" => {
                require_admin!();
                self.handle_cost_by_model(params).await
            }
            "cost.by_role" => {
                require_admin!();
                self.handle_cost_by_role(params).await
            }

            // ── Read-only SQL data sources (admin only, §13.7 WP-D) ──────────
            // One arm, exact method names (never a `starts_with` prefix — see
            // coding convention 2). Everything else lives in `db_sources_rpc`.
            "db_sources.list"
            | "db_sources.test"
            | "db_sources.upsert"
            | "db_sources.remove"
            | "db_sources.tables"
            // WP-A: per-agent `[capabilities] db_sources` grants. Same family,
            // same admin gate — implemented in `db_source_grants`.
            | "db_sources.grants.list"
            | "db_sources.grants.set" => {
                require_admin!();
                crate::db_sources_rpc::dispatch(&self.registry, &self.home_dir, method, params)
                    .await
            }

            // ── Odoo (admin only) ────────────────────────────
            "odoo.status" => {
                require_admin!();
                self.handle_odoo_status().await
            }
            "odoo.config" => {
                require_admin!();
                self.handle_odoo_config().await
            }
            "odoo.configure" => {
                require_admin!();
                self.handle_odoo_configure(params).await
            }
            "odoo.test" => {
                require_admin!();
                self.handle_odoo_test(params).await
            }
            // Schema introspection — enumerate models/fields + write to wiki.
            "odoo.discover_schema" => {
                require_admin!();
                self.handle_odoo_discover_schema(params).await
            }
            // RFC-21 §2: per-agent Odoo credential isolation.
            "odoo.agent_config_get" => {
                require_admin!();
                self.handle_odoo_agent_config_get(params).await
            }
            "odoo.agent_config_set" => {
                require_admin!();
                self.handle_odoo_agent_config_set(params).await
            }
            "odoo.agent_test" => {
                require_admin!();
                self.handle_odoo_agent_test(params).await
            }

            // ── User management (admin only) ─────────────────
            // ── Personal dashboard (WP15) — per-user, no extra gate ──
            "dashboard.widgets.catalog" => self.handle_dashboard_widgets_catalog(ctx).await,
            "dashboard.layout.get" => self.handle_dashboard_layout_get(ctx).await,
            "dashboard.layout.set" => self.handle_dashboard_layout_set(params, ctx).await,

            _ => self.dispatch_org(method, params, ctx, conn).await,
        }
    }
}
