//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

impl MethodHandler {
    pub(crate) async fn dispatch_ops(
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
        // WP-B: fail-closed gate for the appliance-only `device.*` RPC
        // surface — a non-appliance install (the overwhelming majority of
        // installs) refuses every one of these methods with a structured
        // `not_appliance` error, never a confusing "command not found"
        // deep inside a shell-out. `is_appliance()` is the single authority
        // (`duduclaw-core/src/appliance.rs`) — this macro never re-reads
        // the env var itself.
        macro_rules! require_appliance {
            () => {
                if !duduclaw_core::is_appliance() {
                    return device_not_appliance_frame();
                }
            };
        }
        // WP-B: the three destructive `device.*` ops (factory reset,
        // update rollback, restart/shutdown) refuse to run without an
        // explicit `"confirm": true` param — no default-yes, no inferring
        // intent from anything else in the payload.
        macro_rules! require_confirm {
            () => {
                if params.get("confirm").and_then(Value::as_bool) != Some(true) {
                    return device_confirm_required_frame();
                }
            };
        }

        match method {
            "autopilot.create" => {
                require_admin!();
                self.handle_autopilot_create(params).await
            }
            "autopilot.update" => {
                require_admin!();
                self.handle_autopilot_update(params).await
            }
            "autopilot.remove" => {
                require_admin!();
                self.handle_autopilot_remove(params).await
            }
            "autopilot.history" => {
                require_admin!();
                self.handle_autopilot_history(params).await
            }

            // ── Resident sensing observability (WP4, admin only) ──
            "ticks.sources" => {
                require_admin!();
                self.handle_ticks_sources().await
            }
            "ticks.recent" => {
                require_admin!();
                self.handle_ticks_recent(params).await
            }
            // ── v1.68: [[tick.sources]] CRUD (admin; hot-respawns the tasks) ──
            "channel_ingress.list" => {
                require_admin!();
                self.handle_channel_ingress_list(params, ctx).await
            }
            "channel_ingress.resolve" => {
                require_admin!();
                self.handle_channel_ingress_resolve(params, ctx).await
            }
            "channel_ingress.inspect" => {
                require_admin!();
                self.handle_channel_ingress_inspect(params, ctx).await
            }
            "computer_workspaces.list"
            | "computer_workspaces.fence"
            | "computer_workspaces.revoke"
            | "computer_workspaces.regrant"
            | "computer_workspaces.renew"
            | "computer_workspaces.rebind_runner"
            | "computer_workspaces.delete" => {
                require_admin!();
                self.handle_computer_workspaces(method, params, ctx).await
            }
            "tick.sources.list" => {
                require_admin!();
                self.handle_tick_sources_list().await
            }
            "tick.sources.upsert" => {
                require_admin!();
                self.handle_tick_sources_upsert(params, ctx).await
            }
            "tick.sources.remove" => {
                require_admin!();
                self.handle_tick_sources_remove(params, ctx).await
            }

            // ── OS-native page (P4-3, admin management surface) ─
            "os.status" => {
                require_admin!();
                self.handle_os_status().await
            }
            "os.settings.update" => {
                require_admin!();
                self.handle_os_settings_update(params, Some(ctx)).await
            }
            "os.gate.recent" => {
                require_admin!();
                self.handle_os_gate_recent(params).await
            }
            "os.events.recent" => {
                require_admin!();
                self.handle_os_events_recent(params).await
            }
            "os.doctor.run" => {
                require_admin!();
                self.handle_os_doctor_run().await
            }
            // P4-3+: opt this WebSocket connection into the live os_file/
            // os_frontmost tail. `require_admin!` here is the ONLY authority
            // check — `server.rs`'s WS loop only flips the per-connection
            // forwarding flag when this call's response is `ok: true`, so a
            // denied subscribe (non-admin) never starts the tail. Consistent
            // with every other `os.*` RPC on this page.
            "os.events.subscribe" => {
                require_admin!();
                self.handle_os_events_subscribe(params)
            }
            "os.events.unsubscribe" => {
                require_admin!();
                self.handle_os_events_unsubscribe(params)
            }

            // ── Redaction (RFC-23, manager-only) ──────────────
            "redaction.stats" => {
                require_manager!();
                self.handle_redaction_stats().await
            }
            "redaction.recent_audit" => {
                require_manager!();
                self.handle_redaction_recent_audit(params).await
            }
            "redaction.override_status" => {
                require_manager!();
                self.handle_redaction_override_status().await
            }
            "redaction.policy_status" => {
                require_manager!();
                self.handle_redaction_policy_status().await
            }

            // ── Shared Skills (open to all authenticated) ───
            "skills.leaderboard" => self.handle_skills_leaderboard(params).await,
            "skills.shared" => self.handle_skills_shared_list().await,
            "skills.share" => self.handle_skills_share(params).await,
            "skills.adopt" => self.handle_skills_adopt(params).await,

            // ── Custom skills (human × agent authored; V13-T13.0) ───
            // create/update/submit/list are open to any logged-in user (the
            // record carries `created_by`); retire is gated to creator/admin
            // inside the handler.
            "skills.custom_create" => self.handle_skills_custom_create(params, ctx).await,
            "skills.custom_generate" => self.handle_skills_custom_generate(params, ctx).await,
            "skills.custom_update" => self.handle_skills_custom_update(params, ctx).await,
            "skills.custom_submit" => self.handle_skills_custom_submit(params, ctx).await,
            "skills.custom_list" => self.handle_skills_custom_list(ctx).await,
            "skills.custom_retire" => self.handle_skills_custom_retire(params, ctx).await,

            // ── Growth / gamification (login-readable; V10-T10.0) ───
            "growth.snapshot" => self.handle_growth_snapshot().await,
            "growth.daily_report" => self.handle_growth_daily_report(params).await,

            // ── Partner Portal ──────────────────────────────
            "partner.profile" => self.handle_partner_profile().await,
            "partner.stats" => self.handle_partner_stats().await,
            "partner.customers" => self.handle_partner_customers(params).await,
            "partner.profile.update" => {
                require_admin!();
                self.handle_partner_profile_update(params).await
            }
            "partner.customer.add" => {
                require_admin!();
                self.handle_partner_customer_add(params).await
            }
            "partner.customer.update" => {
                require_admin!();
                self.handle_partner_customer_update(params).await
            }
            "partner.customer.delete" => {
                require_admin!();
                self.handle_partner_customer_delete(params).await
            }

            // ── White-label branding + About ─────────────────
            // `branding.get` / `about.get` are readable by any logged-in user
            // (the dashboard needs the product name on every page). Writes go
            // through require_admin! AND the white_label feature gate.
            "branding.get" => self.handle_branding_get().await,
            "about.get" => self.handle_about_get().await,
            "branding.set" => {
                require_admin!();
                self.handle_branding_set(params).await
            }
            "branding.reset" => {
                require_admin!();
                self.handle_branding_reset().await
            }
            "branding.preview" => {
                require_admin!();
                self.handle_branding_preview(params).await
            }
            "branding.bundle.create" => {
                require_admin!();
                self.handle_branding_bundle_create().await
            }

            // ── Distributor management (owner instance) ──────
            "distributor.status" => {
                require_admin!();
                self.handle_distributor_status().await
            }
            "distributor.list" => {
                require_admin!();
                self.handle_distributor_list().await
            }
            "distributor.add" => {
                require_admin!();
                self.handle_distributor_add(params).await
            }
            "distributor.update" => {
                require_admin!();
                self.handle_distributor_update(params).await
            }
            "distributor.remove" => {
                require_admin!();
                self.handle_distributor_remove(params).await
            }
            "distributor.issue" => {
                require_admin!();
                self.handle_distributor_issue(params).await
            }
            "distributor.revoke" => {
                require_admin!();
                self.handle_distributor_revoke(params).await
            }
            "distributor.upgrade" => {
                require_admin!();
                self.handle_distributor_upgrade(params).await
            }
            "distributor.bundle.sign" => {
                require_admin!();
                self.handle_distributor_bundle_sign(params).await
            }

            // ── Billing ──────────────────────────────────────
            "billing.usage" => self.handle_billing_usage().await,
            "billing.history" | "billing.plan" => WsFrame::error_response(
                "",
                "Billing features are not available in the current edition",
            ),
            "browser.audit_log"
            | "browser.emergency_stop"
            | "browser.tool_approve"
            | "browser.browserbase_sessions"
            | "browser.browserbase_cost" => {
                WsFrame::error_response("", "Browser automation features require the Pro edition")
            }
            "marketplace.list" => self.handle_marketplace_list().await,
            "marketplace.install" => {
                require_admin!();
                self.handle_marketplace_install(params).await
            }

            // ── WP-B: appliance device management ────────────────
            // Every `device.*` method is admin-only AND fail-closed off the
            // appliance image (`require_appliance!()` — see its doc comment
            // next to `require_admin!` above). Three destructive ops also
            // require an explicit `"confirm": true` param.
            "device.status" => {
                require_admin!();
                require_appliance!();
                self.handle_device_status().await
            }
            "device.network" => {
                require_admin!();
                require_appliance!();
                self.handle_device_network(params).await
            }
            "device.update_status" => {
                require_admin!();
                require_appliance!();
                self.handle_device_update_status().await
            }
            "device.update_check" => {
                require_admin!();
                require_appliance!();
                self.handle_device_update_check().await
            }
            "device.update_apply" => {
                require_admin!();
                require_appliance!();
                self.handle_device_update_apply().await
            }
            "device.update_rollback" => {
                require_admin!();
                require_appliance!();
                require_confirm!();
                self.handle_device_update_rollback().await
            }
            "device.boot_assessment" => {
                require_admin!();
                require_appliance!();
                self.handle_device_boot_assessment().await
            }
            "device.backup_create" => {
                require_admin!();
                require_appliance!();
                self.handle_device_backup_create().await
            }
            // ── WP-G1: scheduled backups + device-migration restore ──
            "device.backup_schedule_get" => {
                require_admin!();
                require_appliance!();
                self.handle_device_backup_schedule_get().await
            }
            "device.backup_schedule_set" => {
                require_admin!();
                require_appliance!();
                self.handle_device_backup_schedule_set(params).await
            }
            "device.backup_list" => {
                require_admin!();
                require_appliance!();
                self.handle_device_backup_list().await
            }
            "device.backup_delete" => {
                require_admin!();
                require_appliance!();
                self.handle_device_backup_delete(params).await
            }
            "device.backup_restore" => {
                require_admin!();
                require_appliance!();
                require_confirm!();
                self.handle_device_backup_restore(params).await
            }
            "device.factory_reset" => {
                require_admin!();
                require_appliance!();
                require_confirm!();
                self.handle_device_factory_reset(params).await
            }
            "device.power" => {
                require_admin!();
                require_appliance!();
                require_confirm!();
                self.handle_device_power(params).await
            }
            // ── IMPL-POWER: the lock screen's login-free power surface ──
            // Deliberately carries NONE of the three macros above:
            //   * no `require_admin!()` — this method exists precisely to be
            //     reachable before anyone has logged in (see
            //     `power_local`'s module header on why every desktop OS puts
            //     power controls on its lock screen);
            //   * no `require_appliance!()` / `require_confirm!()` — the
            //     appliance fence is not skipped, it moves INTO
            //     `power_local::evaluate` together with the loopback and
            //     action fences, so the whole gate is one pure, ordered,
            //     exhaustively unit-tested function instead of three macro
            //     expansions plus a stray `match` on `action`. Confirmation
            //     is a lock-screen UI affordance (the menu itself is the
            //     confirm step), not a param the caller asserts about itself.
            // What replaces them is stricter, not looser: appliance AND
            // loopback-peer AND a closed two-value action AND a rate limit.
            "device.power_local" => self.handle_device_power_local(params, conn).await,

            // ── D4a: network settings (Wi-Fi over iwd D-Bus) ──────────
            // Same admin + appliance gate as the `device.*` family above
            // (design `DESIGN-network-settings-2026-08.md` §3.2: the
            // authorization decision lives at the RPC front door, not just
            // in the iwd D-Bus policy's `netdev` group membership). The
            // OOBE pre-auth twin of these lives in `server.rs` as
            // `/api/first-run/network/*` — see that module for why the
            // WS-RPC path can't be used before an account exists.
            "network.wifi_scan" => {
                require_admin!();
                require_appliance!();
                self.handle_network_wifi_scan(params).await
            }
            "network.wifi_connect" => {
                require_admin!();
                require_appliance!();
                self.handle_network_wifi_connect(params).await
            }
            "network.wifi_forget" => {
                require_admin!();
                require_appliance!();
                self.handle_network_wifi_forget(params).await
            }
            "network.status" => {
                require_admin!();
                require_appliance!();
                self.handle_network_status().await
            }

            // ── System-settings app: device.about / device.timedate* /
            // network.wired_* — same admin + appliance gate as the rest of
            // the `device.*`/`network.*` family above. See `device_about.rs`
            // (device.about, device.timedate/timedate_set) and
            // `network/wired.rs` (network.wired_status/wired_config) for the
            // data/orchestration half; these five arms are dispatch glue
            // only.
            "device.about" => {
                require_admin!();
                require_appliance!();
                self.handle_device_about().await
            }
            "device.timedate" => {
                require_admin!();
                require_appliance!();
                self.handle_device_timedate().await
            }
            "device.timedate_set" => {
                require_admin!();
                require_appliance!();
                self.handle_device_timedate_set(params).await
            }
            "network.wired_status" => {
                require_admin!();
                require_appliance!();
                self.handle_network_wired_status().await
            }
            "network.wired_config" => {
                require_admin!();
                require_appliance!();
                self.handle_network_wired_config(params).await
            }

            // ── Maintenance Mode — Entry A ────────────────────
            // `commercial/docs/DESIGN-maintenance-mode-2026-08.md` §2. Same
            // gate as the rest of the `device.*`/`network.*` appliance-only
            // family (Admin-only + appliance-only): the whole point of this
            // feature is to widen what an appliance's dashboard operator can
            // see/reach, so it is meaningless (and, per §1, out of scope) on
            // a desktop/dev install that already has a full shell.
            "maintenance.enable" => {
                require_admin!();
                require_appliance!();
                self.handle_maintenance_enable(params, ctx).await
            }
            "maintenance.disable" => {
                require_admin!();
                require_appliance!();
                self.handle_maintenance_disable(params, ctx).await
            }
            "maintenance.status" => {
                require_admin!();
                require_appliance!();
                self.handle_maintenance_status().await
            }
            "maintenance.history" => {
                require_admin!();
                require_appliance!();
                self.handle_maintenance_history(params)
            }
            "maintenance.log_access" => {
                require_admin!();
                require_appliance!();
                self.handle_maintenance_log_access(params, ctx).await
            }

            unknown => WsFrame::error_response("", &format!("Unknown method: {unknown}")),
        }
    }
}
