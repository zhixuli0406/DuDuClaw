//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

impl MethodHandler {
    pub(crate) async fn dispatch_core(
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
            "connect.challenge" => self.handle_connect_challenge(params),
            "connect" => self.handle_connect(params),
            "ping" => WsFrame::ok_response("", json!({ "pong": true })),
            "hello-ok" => self.handle_hello_ok(params),
            "tools.catalog" => self.handle_tools_catalog(params),
            "tools.builtin_catalog" => {
                // Non-sensitive static catalog for the capability editor picker;
                // any authenticated user (same tier as `agents.list`) may read it.
                if let Err(e) = acl::require_role(ctx, UserRole::Employee) {
                    return WsFrame::error_response("", &e);
                }
                self.handle_tools_builtin_catalog()
            }

            // ── Agent methods (filtered by binding) ──────────
            "agents.list" => self.handle_agents_list_filtered(ctx, params).await,
            "agents.status" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_status(params).await
            }
            // ── WP-6F P1: agent presets ("職務組合"), read-only —────────
            // no switching UI in P1 (design §9); binding is CLI-only via
            // `duduclaw preset bind`.
            "presets.list" => {
                require_manager!();
                self.handle_presets_list().await
            }
            "presets.status" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_presets_status(params).await
            }
            "agents.create" => {
                require_admin!();
                self.handle_agents_create_as(params, Some(ctx)).await
            }
            "agents.delegate" => {
                // H1 fix: delegate is high-risk — requires operator-level access
                let _ = check_agent!(AccessLevel::Operator);
                self.handle_agents_delegate(params).await
            }
            "agents.pause" => {
                require_manager!();
                self.handle_agents_pause(params).await
            }
            "agents.resume" => {
                require_manager!();
                self.handle_agents_resume(params).await
            }
            "agents.update" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_agents_update_as(params, Some(ctx)).await
            }
            "agents.remove" => {
                require_admin!();
                self.handle_agents_remove(params).await
            }
            // ── WP4 off-boarding lifecycle (all admin-gated) ──
            "agents.archive" => {
                require_admin!();
                self.handle_agents_archive(params).await
            }
            "agents.unarchive" => {
                require_admin!();
                self.handle_agents_unarchive(params).await
            }
            "agents.handoff" => {
                require_admin!();
                self.handle_agents_handoff(params).await
            }
            "agents.set_avatar" => {
                require_admin!();
                self.handle_agents_set_avatar(params).await
            }
            "agents.set_outfit" => {
                require_admin!();
                self.handle_agents_set_outfit(params).await
            }
            "agents.clear_avatar" => {
                require_admin!();
                self.handle_agents_clear_avatar(params).await
            }
            "agents.inspect" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_inspect(params).await
            }
            // E1: cheap avatar-only read (no telemetry / config serialization).
            "agents.avatar" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_avatar(params).await
            }

            // ── Premium team templates (dashboard onboarding staging flow) ──
            "templates.industries" => {
                require_admin!();
                self.handle_templates_industries().await
            }
            "templates.stage" => {
                require_admin!();
                self.handle_templates_stage(params).await
            }
            "templates.roster" => {
                require_admin!();
                self.handle_templates_roster(params).await
            }
            "templates.role" => {
                require_admin!();
                self.handle_templates_role(params).await
            }
            "templates.create_agent" => {
                require_admin!();
                self.handle_templates_create_agent(params).await
            }

            // ── Behavioral contract (per-agent CONTRACT.toml, CON.1–CON.3) ──
            "contract.get" => {
                require_admin!();
                self.handle_contract_get(params).await
            }
            "contract.update" => {
                require_admin!();
                self.handle_contract_update(params).await
            }

            // ── Redaction / privacy (global config.toml [redaction], RED.1–RED.4) ──
            "redaction.get" => {
                require_admin!();
                self.handle_redaction_get().await
            }
            "redaction.update" => {
                require_admin!();
                self.handle_redaction_update(params).await
            }
            // Same gating as `redaction.update`: a dry run writes vault rows
            // and reads the live rule set, so it is an operator action.
            "redaction.dry_run" => {
                require_admin!();
                self.handle_redaction_dry_run(params).await
            }

            // §13.2 「我的規則」 — same operator gate as the rest of
            // `redaction.*`: every one of these writes the rule set that
            // decides what leaves the deployment.
            "redaction.custom_rules.list" => {
                require_admin!();
                self.handle_redaction_custom_rules_list().await
            }
            "redaction.custom_rules.upsert" => {
                require_admin!();
                self.handle_redaction_custom_rules_upsert(params).await
            }
            "redaction.custom_rules.remove" => {
                require_admin!();
                self.handle_redaction_custom_rules_remove(params).await
            }
            "redaction.custom_rules.set_enabled" => {
                require_admin!();
                self.handle_redaction_custom_rules_set_enabled(params).await
            }
            "redaction.suggest_pattern" => {
                require_admin!();
                self.handle_redaction_suggest_pattern(params, &ctx.user_id)
                    .await
            }
            "redaction.profiles.import" => {
                require_admin!();
                self.handle_redaction_profiles_import(params).await
            }
            "redaction.profiles.remove" => {
                require_admin!();
                self.handle_redaction_profiles_remove(params).await
            }

            // §13.4 「AI 智慧偵測」 model lifecycle. Same operator gate as the
            // rest of `redaction.*`: installing a 945 MB artefact and changing
            // what the detector can see are both operator actions.
            "redaction.model.status" => {
                require_admin!();
                self.handle_redaction_model_status().await
            }
            "redaction.model.install" => {
                require_admin!();
                self.handle_redaction_model_install().await
            }
            "redaction.model.cancel" => {
                require_admin!();
                self.handle_redaction_model_cancel().await
            }
            "redaction.model.remove" => {
                require_admin!();
                self.handle_redaction_model_remove().await
            }

            // IDR: identity resolution (RFC-21 §1 dashboard surface)
            "identity.config_get" => {
                require_admin!();
                self.handle_identity_config_get().await
            }
            "identity.config_set" => {
                require_admin!();
                self.handle_identity_config_set(params).await
            }
            "identity.resolve" => {
                require_admin!();
                self.handle_identity_resolve(params).await
            }

            // ── Skill synthesis auto-run (global config.toml [skill_synthesis], W19-P1) ──
            "skill_synthesis.get" => {
                require_admin!();
                self.handle_skill_synthesis_get().await
            }
            "skill_synthesis.update" => {
                require_admin!();
                self.handle_skill_synthesis_update(params).await
            }

            // ── Inference (global ~/.duduclaw/inference.toml, INF.1–INF.5) ──
            "inference.get" => {
                require_admin!();
                self.handle_inference_get().await
            }
            "inference.update" => {
                require_admin!();
                self.handle_inference_update(params).await
            }

            // ── WP-D: appliance local model (llama.cpp server on loopback).
            //    Reads are admin-only like the rest of the `inference.*`
            //    family — this surface exposes the machine's memory profile
            //    and the state root's layout. Every response is computed
            //    from a live scan/probe; nothing is asserted from config
            //    alone. See `inference_local.rs`. ──
            "inference.local.catalog" => {
                require_admin!();
                WsFrame::ok_response("", crate::inference_local::catalog(&self.home_dir).await)
            }
            "inference.local.status" => {
                require_admin!();
                WsFrame::ok_response("", crate::inference_local::status(&self.home_dir).await)
            }
            "inference.local.download" => {
                require_admin!();
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    WsFrame::error_response("", "id is required")
                } else {
                    match crate::inference_local::download(id, &self.home_dir).await {
                        Ok(v) => WsFrame::ok_response("", v),
                        Err(e) => WsFrame::error_response("", &e),
                    }
                }
            }
            "inference.local.serve" => {
                require_admin!();
                let file = params.get("model_file").and_then(|v| v.as_str()).unwrap_or("");
                let ctx = params
                    .get("ctx")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok());
                if file.is_empty() {
                    WsFrame::error_response("", "model_file is required")
                } else {
                    match crate::inference_local::serve(file, ctx, &self.home_dir).await {
                        Ok(v) => WsFrame::ok_response("", v),
                        Err(e) => WsFrame::error_response("", &e),
                    }
                }
            }
            "inference.local.stop" => {
                require_admin!();
                WsFrame::ok_response("", crate::inference_local::stop().await)
            }

            // ── MCP API keys (global config.toml [mcp_keys], MK.1–MK.4) ──
            "mcp_keys.list" => {
                require_admin!();
                self.handle_mcp_keys_list().await
            }
            "mcp_keys.create" => {
                require_admin!();
                self.handle_mcp_keys_create(params).await
            }
            "mcp_keys.revoke" => {
                require_admin!();
                self.handle_mcp_keys_revoke(params).await
            }

            // ── Kill switch (global ~/.duduclaw/KILLSWITCH.toml, KS.1–KS.2) ──
            "killswitch.get" => {
                require_admin!();
                self.handle_killswitch_get().await
            }
            "killswitch.update" => {
                require_admin!();
                self.handle_killswitch_update(params).await
            }

            // ── Wiki namespace scope (.scope.toml, SCP.1) ──
            "wiki_scope.get" => {
                require_admin!();
                self.handle_wiki_scope_get().await
            }
            "wiki_scope.update" => {
                require_admin!();
                self.handle_wiki_scope_update(params).await
            }

            // ── Channel methods (admin only) ─────────────────
            "channels.status" => {
                require_admin!();
                self.handle_channels_status().await
            }
            "channels.add" => {
                require_admin!();
                self.handle_channels_add(params).await
            }
            "channels.test" => {
                require_admin!();
                self.handle_channels_test(params).await
            }
            "channels.remove" => {
                require_admin!();
                self.handle_channels_remove(params).await
            }
            // WP9: mint a one-time Telegram deep-link/QR bind token for a
            // specific AI employee (shared-bot onboarding). Admin only.
            "channels.telegram_bind_token" => {
                require_admin!();
                self.handle_telegram_bind_token(params).await
            }
            // WP1.1 (ecosystem): LINE OA add-friend link for the QR onboarding
            // card / printable poster. Admin only, same gate as its siblings.
            "channels.line_add_friend" => {
                require_admin!();
                self.handle_line_add_friend().await
            }

            // ── W2-2 (E1/E2, D-C1): behavior settings + access control ──
            // Read/write the same `ChannelSettingsManager`/`AccessController`
            // the `channel_config`/`pairing_manage` MCP tools use — one access
            // layer, two front doors. Admin-only, matching every other
            // `channels.*` method (these are security-relevant settings:
            // `admin_users` decides who can press `!STOP` in-channel).
            "channels.config_get" => {
                require_admin!();
                self.handle_channels_config_get(params).await
            }
            "channels.config_set" => {
                require_admin!();
                self.handle_channels_config_set(params, ctx).await
            }
            "channels.access_get" => {
                require_admin!();
                self.handle_channels_access_get(params).await
            }
            "channels.access_set" => {
                require_admin!();
                self.handle_channels_access_set(params, ctx).await
            }
            "channels.pairing_list" => {
                require_admin!();
                self.handle_channels_pairing_list().await
            }
            "channels.pairing_revoke" => {
                require_admin!();
                self.handle_channels_pairing_revoke(params, ctx).await
            }

            // ── Account methods (admin only) ─────────────────
            // ── License (read-only snapshot of the gateway LicenseRuntime) ──
            //
            // `license.status` lets the dashboard render tier + expiry +
            // grace-period warnings without parsing ~/.duduclaw/license.json
            // directly. Manager-level access — the snapshot intentionally
            // omits the raw signature and customer email, so it is safe to
            // show to anyone who can already see operational metrics.
            "license.status" => {
                require_manager!();
                self.handle_license_status().await
            }
            // Dashboard upgrade flow — install/redeem a commercial license
            // without touching the CLI. Admin-only: this changes what the
            // whole install is allowed to do.
            "license.fingerprint" => {
                require_admin!();
                self.handle_license_fingerprint().await
            }
            "license.activate" => {
                require_admin!();
                self.handle_license_activate(params).await
            }
            "license.redeem" => {
                require_admin!();
                self.handle_license_redeem(params).await
            }

            "accounts.list" => {
                require_admin!();
                self.handle_accounts_list().await
            }
            "accounts.budget_summary" => {
                require_manager!();
                self.handle_budget_summary().await
            }

            _ => self.dispatch_knowledge(method, params, ctx, conn).await,
        }
    }
}
