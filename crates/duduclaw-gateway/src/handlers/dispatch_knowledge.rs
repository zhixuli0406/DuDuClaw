//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

impl MethodHandler {
    pub(crate) async fn dispatch_knowledge(
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
        // Helper for list/filter RPCs that accept an OPTIONAL `agent_id` filter.
        // Admins may list across all agents; non-admins must scope the query to
        // an agent they are bound to (otherwise they could enumerate other
        // teams' tasks/activity). Mirrors the agent-binding intent of memory.*.
        macro_rules! check_agent_filter {
            ($min_level:expr) => {
                if !ctx.is_admin() {
                    match params.get("agent_id").and_then(|v| v.as_str()) {
                        Some(id) if !id.is_empty() => {
                            if let Err(e) = acl::require_agent_access(ctx, id, $min_level) {
                                return WsFrame::error_response("", &e);
                            }
                        }
                        _ => {
                            return WsFrame::error_response("", "agent_id parameter is required");
                        }
                    }
                }
            };
        }

        match method {
            // CLI-store credentials (grok/codex/gemini) — presence-only view
            // of each CLI's own credential file, so subscription logins that
            // never become rotator accounts still show on the accounts page.
            "accounts.cli_credentials" => {
                require_manager!();
                self.handle_accounts_cli_credentials().await
            }
            "accounts.rotate" => {
                require_admin!();
                self.handle_accounts_rotate(params).await
            }
            "accounts.health" => {
                require_admin!();
                self.handle_accounts_health().await
            }
            "accounts.add" => {
                require_admin!();
                self.handle_accounts_add(params).await
            }
            "accounts.update_budget" => {
                require_admin!();
                self.handle_accounts_update_budget(params).await
            }
            "accounts.update" => {
                require_admin!();
                self.handle_accounts_update(params).await
            }
            // Interactive CLI login ("Dashboard 一鍵登入") — drives the CLI's
            // native login in a PTY and streams it to the dashboard.
            "auth.cli_login.start" => {
                require_admin!();
                self.handle_cli_login_start(params).await
            }
            "auth.cli_login.input" => {
                require_admin!();
                self.handle_cli_login_input(params).await
            }
            "auth.cli_login.status" => {
                require_admin!();
                self.handle_cli_login_status(params).await
            }
            "auth.cli_login.cancel" => {
                require_admin!();
                self.handle_cli_login_cancel(params).await
            }
            "auth.cli_login.finalize" => {
                require_admin!();
                self.handle_cli_login_finalize(params).await
            }
            // WP-D: "訂閱帳號" device-code-style setup wizard — a guided,
            // single-flight, pre-validated specialization of the generic
            // `auth.cli_login.*` flow above, scoped to Claude subscription
            // accounts on headless boxes. No appliance gate: useful on
            // desktop too. See `setup_token_wizard.rs`.
            "accounts.setup_token_start" => {
                require_admin!();
                self.handle_accounts_setup_token_start().await
            }
            "accounts.setup_token_submit" => {
                require_admin!();
                self.handle_accounts_setup_token_submit(params).await
            }
            "accounts.setup_token_status" => {
                require_admin!();
                self.handle_accounts_setup_token_status(params).await
            }
            "accounts.setup_token_cancel" => {
                require_admin!();
                self.handle_accounts_setup_token_cancel(params).await
            }

            // ── Memory (agent-scoped, H2 fix) ────────────────
            "memory.search" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_search(params).await
            }
            "memory.browse" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_browse(params).await
            }
            "memory.key_facts" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_key_facts(params).await
            }
            // Freshness histogram + top-N + accumulation trend for the memory
            // page's decay visualisation. Read-only aggregate over the same
            // rows `memory.browse` lists.
            "memory.decay_overview" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_decay_overview(params).await
            }
            // F1 Temporal Memory (v1.19.0): supersession chain + point-in-time.
            "memory.history" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_history(params).await
            }
            "memory.at" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_at(params).await
            }
            // D6 alias — the D1 point-in-time lookup is also exposed under the
            // engine method name so the curation UI reads consistently.
            "memory.get_at" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_at(params).await
            }
            // D6 HITL knowledge-graph curation (2026-07): SPO graph export for
            // the force-directed viewer + a destructive by-origin rollback.
            "memory.graph" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_graph(params).await
            }
            // Destructive: expire every currently-valid fact from one source.
            // Owner access to the agent + Manager role (dashboard-local only).
            "memory.invalidate_origin" => {
                require_manager!();
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_memory_invalidate_origin(params).await
            }
            // Destructive but recoverable: forget one entry (archived, not
            // dropped). Owner access mirrors `memory.invalidate_origin`; the
            // role bar is Employee-and-up because forgetting a single wrong
            // memory is routine hygiene for whoever owns the AI staff member,
            // not an admin-only rollback.
            "memory.forget" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_memory_forget(params).await
            }

            // ── Wiki (agent-scoped — HS4 fix, mirrors memory.*) ───────
            // Each arm reads `agent_id` from params; an Employee bound only to
            // agent A must not be able to read agent B's private wiki/SOPs.
            "wiki.pages" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_pages(params).await
            }
            "wiki.read" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_read(params).await
            }
            // WP5c curation station — the auto-filing audit surface.
            // Listing is read-only (Viewer); the three state-changing actions
            // need Owner access to the agent, mirroring `memory.forget`:
            // promoting / removing / sharing an AI staff member's knowledge is
            // routine hygiene for whoever owns them, not an admin-only lever.
            "wiki.auto_pages" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_auto_pages(params).await
            }
            "wiki.promote" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_wiki_promote(params).await
            }
            "wiki.archive" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_wiki_archive(params).await
            }
            "wiki.share" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_wiki_share(params).await
            }
            "wiki.search" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_search(params).await
            }
            "wiki.lint" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_lint(params).await
            }
            "wiki.stats" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_stats(params).await
            }
            // Phase 4: trust feedback inspection + manual override.
            // Trust state exposes per-conversation citation history that
            // can correlate with user activity → manager+ only (review H1).
            "wiki.trust_audit" => {
                require_manager!();
                self.handle_wiki_trust_audit(params).await
            }
            "wiki.trust_override" => {
                require_admin!();
                self.handle_wiki_trust_override(params).await
            }
            "wiki.trust_history" => {
                require_manager!();
                self.handle_wiki_trust_history(params).await
            }

            // ── Shared Wiki ─────────────────────────────────
            "shared_wiki.pages" => self.handle_shared_wiki_pages().await,
            "shared_wiki.read" => self.handle_shared_wiki_read(params).await,
            "shared_wiki.search" => self.handle_shared_wiki_search(params).await,
            "shared_wiki.stats" => self.handle_shared_wiki_stats().await,

            // ── I-5: cross-source content search (⌘K backend) ──
            // Aggregates conversations / artifacts / memory / wiki behind one
            // bounded query. Admins may search across every agent;
            // non-admins must scope to a bound agent (mirrors
            // `chat.sessions.list` / `memory.*` — no enumerating other
            // teams' conversations or knowledge).
            "search.query" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_search_query(params, ctx).await
            }

            // ── Skills (open to all) ─────────────────────────
            "skills.list" => self.handle_skills_list(params).await,
            "skills.search" => self.handle_skills_search(params).await,
            "skills.content" => {
                // HS4: skill content is read from a specific agent's registry —
                // scope to an agent the caller is bound to.
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_skills_content(params).await
            }
            // Read-only preview (fetch + security scan, no mutation): any
            // authenticated user may scan a skill before requesting install.
            "skills.vet" => self.handle_skills_vet(params).await,
            // Direct install stays admin-only; non-admins file an install request.
            "skills.install" => {
                require_admin!();
                self.handle_skills_install(params).await
            }
            "skills.install_request" => self.handle_skills_install_request(params, ctx).await,

            // ── Expert packs (專家包, admin only) ────────────────
            "experts.list" => {
                require_admin!();
                self.handle_experts_list().await
            }
            "experts.install" => {
                require_admin!();
                self.handle_experts_install(params).await
            }
            "experts.remove" => {
                require_admin!();
                self.handle_experts_remove(params).await
            }
            "experts.hooks_apply" => {
                require_admin!();
                self.handle_experts_hooks_apply(params).await
            }
            "experts.catalog" => {
                require_admin!();
                self.handle_experts_catalog().await
            }
            "experts.install_builtin" => {
                require_admin!();
                self.handle_experts_install_builtin(params).await
            }
            "experts.generate" => {
                require_admin!();
                self.handle_experts_generate(params).await
            }
            "experts.generate_revise" => {
                require_admin!();
                self.handle_experts_generate_revise(params).await
            }
            "experts.install_draft" => {
                require_admin!();
                self.handle_experts_install_draft(params).await
            }

            // ── Inspiration gallery (P2-b, curated-only, admin only —
            //    mirrors experts.catalog's license/premium-tree gate) ──
            "gallery.list" => {
                require_admin!();
                self.handle_gallery_list().await
            }

            // ── Cron (admin only) ────────────────────────────
            "cron.list" => {
                require_admin!();
                self.handle_cron_list().await
            }
            "cron.add" => {
                require_admin!();
                self.handle_cron_add(params).await
            }
            "cron.update" => {
                require_admin!();
                self.handle_cron_update(params).await
            }
            "cron.pause" => {
                require_admin!();
                self.handle_cron_set_enabled(params, false).await
            }
            "cron.resume" => {
                require_admin!();
                self.handle_cron_set_enabled(params, true).await
            }
            "cron.remove" => {
                require_admin!();
                self.handle_cron_remove(params).await
            }
            "cron.run_now" => {
                require_admin!();
                self.handle_cron_run_now(params).await
            }
            "cron.templates" => {
                require_admin!();
                self.handle_cron_templates().await
            }

            // ── System (admin only for config changes) ───────
            "system.status" => self.handle_system_status().await,
            "system.doctor" => {
                require_admin!();
                self.handle_system_doctor().await
            }
            "system.doctor_repair" => {
                require_admin!();
                self.handle_system_doctor_repair().await
            }
            "models.list" => self.handle_models_list().await,
            "models.refresh" => self.handle_models_refresh().await,

            // ── Local-model marketplace (design doc: DESIGN-local-model-
            //    marketplace-2026-08-13). Reads for any logged-in user;
            //    mutations (install/cancel/remove) manager+. ──
            "localmodels.search" => {
                let intent = params.get("intent").and_then(|v| v.as_str()).unwrap_or("chat");
                match crate::local_models::search(intent, &self.home_dir).await {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            "localmodels.quants" => {
                let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                match crate::local_models::quants(repo, &self.home_dir).await {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            "localmodels.installed" => {
                WsFrame::ok_response("", crate::local_models::installed(&self.home_dir).await)
            }
            "localmodels.install_status" => {
                WsFrame::ok_response("", crate::local_models::install_status())
            }
            "localmodels.install" => {
                require_manager!();
                let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                let filename = params.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                let shards: Vec<String> = params
                    .get("shards")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let total = params.get("total_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                if filename.is_empty() {
                    WsFrame::error_response("", "filename is required")
                } else {
                    match crate::local_models::install(
                        repo, filename, shards, total, &self.home_dir,
                    )
                    .await
                    {
                        Ok(id) => WsFrame::ok_response("", json!({ "job_id": id })),
                        Err(e) => WsFrame::error_response("", &e),
                    }
                }
            }
            "localmodels.cancel" => {
                require_manager!();
                let id = params.get("job_id").and_then(|v| v.as_u64()).unwrap_or(0);
                match crate::local_models::cancel(id) {
                    Ok(()) => WsFrame::ok_response("", json!({ "cancelled": true })),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            "localmodels.remove" => {
                require_manager!();
                let filename = params.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                match crate::local_models::remove(filename, &self.home_dir).await {
                    Ok(()) => WsFrame::ok_response("", json!({ "removed": true })),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            // ── Fine-tuning / post-training (WP-E, TODO-ai-runtimes-2026-09
            //    decision 4C). Admin-only across the board: dataset building
            //    reads every stored conversation, task result and approval on
            //    the box, and job creation can ship that corpus to a third
            //    party. That is an owner-level decision, not a manager one.
            //
            //    Nothing here trains locally — the appliance's iGPU cannot.
            //    See `finetune/mod.rs` for the "curate here, train elsewhere,
            //    deploy here" framing and the privacy gate. ──
            "finetune.datasets.list" => {
                require_admin!();
                finetune_frame(
                    crate::finetune::dataset::list(&self.home_dir)
                        .map(|d| json!({ "datasets": d })),
                )
            }
            "finetune.datasets.create" => {
                require_admin!();
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("sharegpt");
                finetune_frame(
                    crate::finetune::dataset::create(&self.home_dir, name, format)
                        .map(|d| json!({ "dataset": d })),
                )
            }

            _ => self.dispatch_system(method, params, ctx, conn).await,
        }
    }
}
