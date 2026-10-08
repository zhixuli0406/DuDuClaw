//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

impl MethodHandler {
    pub(crate) async fn dispatch_org(
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
        // Helper: check access to a specifically-named agent (used when the
        // binding key is not the literal `agent_id` param — e.g. `assigned_to`).
        macro_rules! check_agent_named {
            ($agent:expr, $min_level:expr) => {
                if let Err(e) = acl::require_agent_access(ctx, $agent, $min_level) {
                    return WsFrame::error_response("", &e);
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
            "discovery.catalog" | "discovery.list" | "discovery.tree" | "discovery.artifact"
            | "discovery.cancel" => self.handle_discovery_rpc(method, params, ctx).await,
            "dashboard.layout.view" => {
                require_manager!();
                self.handle_dashboard_layout_view(params, ctx).await
            }
            // ── Custom widgets (sandboxed-iframe HTML cards; 2026-07-16) ──
            "widgets.custom.list" => self.handle_widgets_custom_list(ctx).await,
            "widgets.custom.get" => self.handle_widgets_custom_get(params, ctx).await,
            "widgets.custom.create" => self.handle_widgets_custom_create(params, ctx).await,
            "widgets.custom.update" => self.handle_widgets_custom_update(params, ctx).await,
            "widgets.custom.remove" => self.handle_widgets_custom_remove(params, ctx).await,
            "widgets.custom.share" => self.handle_widgets_custom_share(params, ctx).await,
            "widgets.custom.generate" => self.handle_widgets_custom_generate(params).await,
            "users.subordinates" => {
                require_manager!();
                self.handle_users_subordinates(ctx).await
            }
            // ── Departments (org structure for agent create/edit) ──
            "departments.list" => {
                require_manager!();
                self.handle_departments_list().await
            }
            "departments.create" => {
                require_admin!();
                self.handle_departments_create(params).await
            }
            "departments.remove" => {
                require_admin!();
                self.handle_departments_remove(params).await
            }
            "users.list" => {
                require_admin!();
                self.handle_users_list().await
            }
            "users.create" => {
                require_admin!();
                self.handle_users_create(params, ctx).await
            }
            "users.update" => {
                require_admin!();
                self.handle_users_update(params, ctx).await
            }
            "users.remove" => {
                require_admin!();
                self.handle_users_remove(params, ctx).await
            }
            "users.bind_agent" => {
                require_admin!();
                self.handle_users_bind_agent(params, ctx).await
            }
            "users.unbind_agent" => {
                require_admin!();
                self.handle_users_unbind_agent(params, ctx).await
            }
            "users.offboard" => {
                require_admin!();
                self.handle_users_offboard(params, ctx).await
            }
            "users.me" => self.handle_users_me(ctx).await,
            // Self-service: any logged-in user changes their OWN password. Not
            // admin-gated on purpose — it only ever mutates the caller's account,
            // and it's the sole password path in the single-owner edition (the
            // multi-user Users page is hidden there).
            "users.change_password" => self.handle_users_change_password(params, ctx).await,
            "users.audit_log" => {
                require_admin!();
                self.handle_users_audit_log(params).await
            }

            "mcp.list" => {
                require_admin!();
                self.handle_mcp_list().await
            }
            "mcp.update" => {
                require_admin!();
                self.handle_mcp_update(&params).await
            }
            // Read-only preview (fetch + scan): any authenticated user may
            // browse and scan candidates before requesting install.
            "mcp.import.fetch" => self.handle_mcp_import_fetch(params).await,
            // Direct install stays admin-only; non-admins file an install request.
            "mcp.import.install" => {
                require_admin!();
                self.handle_mcp_import_install(params).await
            }
            "mcp.install_request" => self.handle_mcp_install_request(params, ctx).await,
            // MCP Registry: search is read-only (like mcp.import.fetch);
            // install routes Admins to the direct install and everyone else
            // to an install request inside the handler.
            "mcp.registry_search" => self.handle_mcp_registry_search(params).await,
            "mcp.registry_install" => self.handle_mcp_registry_install(params, ctx).await,
            // Native remote MCP connections hold credentials: Admin only.
            "mcp.remote_connect" => {
                require_admin!();
                self.handle_mcp_remote_connect(params, ctx).await
            }
            "mcp.remote_complete" => {
                require_admin!();
                self.handle_mcp_remote_complete(params, ctx).await
            }
            "mcp.remote_status" => {
                require_admin!();
                self.handle_mcp_remote_status(params).await
            }
            "mcp.remote_disconnect" => {
                require_admin!();
                self.handle_mcp_remote_disconnect(params, ctx).await
            }
            // Third-party tools with their derived effect class (2026-10-08).
            "mcp.tool_effects" => {
                require_admin!();
                self.handle_mcp_tool_effects(params).await
            }
            // MCP Events subscriptions hold signing secrets: Admin only.
            "mcp.events_subscribe" => {
                require_admin!();
                self.handle_mcp_events_subscribe(params).await
            }
            "mcp.events_discover" => {
                require_admin!();
                self.handle_mcp_events_discover(params).await
            }
            "mcp.events_list" => {
                require_admin!();
                self.handle_mcp_events_list(params).await
            }
            "mcp.events_unsubscribe" => {
                require_admin!();
                self.handle_mcp_events_unsubscribe(params).await
            }
            "mcp.events_rotate" => {
                require_admin!();
                self.handle_mcp_events_rotate(params).await
            }

            // ── MCP OAuth (admin only) ──────────────────────────
            "mcp.oauth.providers" => {
                require_admin!();
                self.handle_mcp_oauth_providers().await
            }
            "mcp.oauth.start" => {
                require_admin!();
                self.handle_mcp_oauth_start(params).await
            }
            "mcp.oauth.status" => {
                require_admin!();
                self.handle_mcp_oauth_status(params).await
            }
            "mcp.oauth.revoke" => {
                require_admin!();
                self.handle_mcp_oauth_revoke(params).await
            }

            // ── Google credential paths (admin only) ────────────
            // Service-account delegation / Apps Script bridge configuration.
            // Admin-gated like every other credential surface; `get` never
            // returns the stored bridge secret.
            "google.credentials.get" => {
                require_admin!();
                self.handle_google_credentials_get().await
            }
            "google.credentials.set" => {
                require_admin!();
                self.handle_google_credentials_set(params).await
            }
            "google.credentials.test" => {
                require_admin!();
                self.handle_google_credentials_test().await
            }
            // G6 (2026-09 feature audit): the master gate as a first-class
            // control. The page used to be unconditionally visible while
            // `[integrations] google_workspace` defaulted to false, so an
            // operator could open it, follow every step, and still have every
            // agent's Google tool call refused. Now the page reads the flag
            // and offers this switch instead.
            "google.integration.set" => {
                require_admin!();
                self.handle_google_integration_set(params).await
            }

            // P9 employee digest + feedback on finished work (per-call ACL
            // inside: Viewer to read, Operator + task audience to rate).
            "digest.latest" | "digest.feedback" => self.handle_digest_rpc(method, params, ctx).await,

            // ── Task Board (agent-scoped — HS4 fix) ────
            "responsibilities.create"
            | "responsibilities.status"
            | "responsibilities.list"
            | "responsibilities.get"
            | "responsibilities.occurrences"
            | "responsibilities.fires"
            | "responsibilities.update_contract"
            | "responsibilities.pause"
            | "responsibilities.resume"
            | "responsibilities.disable"
            | "responsibilities.enable"
            | "responsibilities.clear_failures"
            | "tasks.steer"
            | "tasks.steering"
            | "tasks.stop"
            | "tasks.stop_status" => self.handle_responsibilities_rpc(method, params, ctx).await,
            "tasks.review_snapshot" => super::workflow_errors::with_workflow_error_code(method, self.handle_tasks_review_snapshot(params,ctx).await),
            "tasks.review_accept" => super::workflow_errors::with_workflow_error_code(method, self.handle_tasks_review_accept(params,ctx).await),
            "workflow_drafts.create" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_create(params,ctx).await),
            "workflow_drafts.get" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_get(params,ctx).await),
            "workflow_drafts.list" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_list(params,ctx).await),
            "workflow_drafts.run_fixture" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_run_fixture(params,ctx).await),
            "workflow_drafts.request_activation" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_request_activation(params,ctx).await),
            "workflow_drafts.commit_activation" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_commit_activation(params,ctx).await),
            "workflow_drafts.revoke_activation" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_drafts_revoke_activation(params,ctx).await),
            "workflow_runs.get" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_runs_get(params,ctx).await),
            "workflow_runs.list" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_runs_list(params,ctx).await),
            "workflow_runs.cancel" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_runs_cancel(params,ctx).await),
            "workflow_runs.reset_failures" => super::workflow_errors::with_workflow_error_code(method, self.handle_workflow_runs_reset_failures(params,ctx).await),
            "tasks.list" => {
                // Non-admins must scope the listing to a bound agent.
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_tasks_list(params, ctx).await
            }
            "tasks.create" => {
                // The target agent is `assigned_to`; creating work for an agent
                // is a side-effecting operation → Operator level.
                let assigned_to = params
                    .get("assigned_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if assigned_to.is_empty() {
                    return WsFrame::error_response("", "assigned_to is required");
                }
                check_agent_named!(assigned_to, AccessLevel::Operator);
                self.handle_tasks_create(params, ctx).await
            }
            "tasks.update" => self.handle_tasks_update(params, ctx).await,
            "tasks.remove" => self.handle_tasks_remove(params, ctx).await,
            "tasks.assign" => self.handle_tasks_assign(params, ctx).await,
            // I-3b task list operations (dashboard-ux-workbuddy 2026-08):
            // archive/pin/rename, thin wrappers over `handle_tasks_update`
            // (same delegation pattern as `tasks.assign` above) so HS4
            // agent-binding authorization is enforced exactly once.
            "tasks.archive" => self.handle_tasks_archive(params, ctx).await,
            "tasks.unarchive" => self.handle_tasks_unarchive(params, ctx).await,
            "tasks.pin" => self.handle_tasks_pin(params, ctx).await,
            "tasks.unpin" => self.handle_tasks_unpin(params, ctx).await,
            "tasks.rename" => self.handle_tasks_rename(params, ctx).await,
            // I-3b: paginated task listing (with total count) — replaces the
            // client-side `.slice(0, 20)` hard cut the `/goals` page used to
            // apply to finished tasks. Same Viewer/agent-filter gate as
            // `tasks.list`.
            "tasks.list_page" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_tasks_list_page(params, ctx).await
            }
            // L2: task comments. Access is gated inside the handler by the
            // task's owning agent (Viewer) — anyone who can see the task may
            // read/post; unknown task fails closed for non-admins.
            "tasks.comment" => self.handle_tasks_comment(params, ctx).await,
            "tasks.comments" => self.handle_tasks_comments(params, ctx).await,
            // Iterative Kanban: per-task revision timeline + per-agent flow
            // metrics. Both are read-only board analytics (Viewer). The
            // per-task call gates on the task's owning agent inside the handler
            // (like tasks.comments); the aggregate forces non-admins to scope to
            // a bound agent (check_agent_filter) and filters the result to it.
            "tasks.iterations" => self.handle_tasks_iterations(params, ctx).await,
            "tasks.timeline" => self.handle_tasks_timeline(params, ctx).await,
            // WP-F (P2-c): per-task file-change evidence for the needs_human
            // 「變更」tab. Same read-only, task-scoped gate as tasks.comments.
            "tasks.changes" => self.handle_tasks_changes(params, ctx).await,
            "tasks.artifacts" => self.handle_tasks_artifacts(params, ctx).await,
            "tasks.role_turns" => self.handle_tasks_role_turns(params, ctx).await,
            "tasks.goal_decide" => self.handle_tasks_goal_decide(params, ctx).await,
            "tasks.goal_create" => self.handle_tasks_goal_create(params, ctx).await,
            "tasks.flow_metrics" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_tasks_flow_metrics(params).await
            }

            // ── Co-edited plans (U4, Cocoa arXiv:2412.10999) ──
            // Same gate pattern as tasks.*: listing takes the optional
            // agent-filter gate (Viewer); per-plan methods resolve the plan's
            // owning agent inside the handler and fail closed (HS4).
            "plans.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_plans_list(params).await
            }
            "plans.get" => self.handle_plans_get(params, ctx).await,
            "plans.create" => {
                // The owning agent is `agent_id`; creating a shared plan for
                // an agent is side-effecting → Operator binding required.
                let agent_id = params
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if agent_id.is_empty() {
                    return WsFrame::error_response("", "agent_id is required");
                }
                check_agent_named!(agent_id, AccessLevel::Operator);
                self.handle_plans_create(params, ctx).await
            }
            "plans.update" => self.handle_plans_update(params, ctx).await,
            "plans.remove" => self.handle_plans_remove(params, ctx).await,
            "plans.add_step" => self.handle_plans_add_step(params, ctx).await,
            "plans.update_step" => self.handle_plans_update_step(params, ctx).await,
            "plans.remove_step" => self.handle_plans_remove_step(params, ctx).await,

            // ── Activity Feed (agent-scoped — HS4 fix) ───
            "activity.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_activity_list(params, ctx).await
            }
            // Per-topic filtering is NOT implemented: BroadcastLayer fans out every
            // activity event to every authenticated WS client unconditionally. This
            // RPC exists purely as a client-intent signal and future-compat hook so
            // callers can declare interest without guessing at server state.
            "activity.subscribe" => WsFrame::ok_response(
                "",
                json!({
                    "subscribed": true,
                    "broadcast_mode": "all_events",
                    "note": "All authenticated WS clients receive activity events automatically; no per-client filter is in effect.",
                }),
            ),

            // ── Work Timeline (G11) — company Gantt view. Same gate
            //    as activity.list: viewing is read-only, agent-scoped.
            "timeline.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_timeline_list(params, ctx).await
            }

            // ── Run inspector (G12) — per-run transcript derived from
            //    sessions.db turns + the MCP tool audit trail. Same gate as
            //    activity.list: read-only, agent-scoped, fail-closed.
            "runs.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_runs_list(params, ctx).await
            }
            // Gated inside the handler by the run's owning agent (Viewer) —
            // the agent id is only known after resolving the run's session;
            // unknown run fails closed for non-admins.
            "runs.get" => self.handle_runs_get(params, ctx).await,

            // ── WebChat session history + resume (WP3) ──────
            // Same gate as runs.list: read-only, agent-scoped, fail-closed.
            // Admins may list across all agents (agent_id optional); non-admins
            // MUST scope to a bound agent so they cannot enumerate other teams'
            // conversations.
            "chat.sessions.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_chat_sessions_list(params).await
            }
            // Gated inside the handler by the session's owning agent (Viewer) —
            // the agent id is resolved from the session; an unknown session id
            // fails closed ("session not found") before any turns are exposed.
            "chat.sessions.history" => self.handle_chat_sessions_history(params, ctx).await,

            // ── Decision Continuity (RFC-24, agent-scoped) ──
            "decisions.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_decisions_list(params).await
            }
            "decisions.dismiss" => {
                // Marking a captured decision as a false positive mutates state.
                check_agent_filter!(AccessLevel::Operator);
                self.handle_decisions_dismiss(params).await
            }

            // ── Live Canvas (G15) — agent-pushed HTML workspace. Same gate
            //    as activity.list: viewing is read-only, agent-scoped,
            //    fail-closed (non-admins must name an agent they can view).
            //    Mutations happen only via the `canvas_push` / `canvas_clear`
            //    MCP tools; content is ammonia-sanitized at write time and the
            //    dashboard renders it inside `<iframe sandbox="">`.
            "canvas.get" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_canvas_get(params).await
            }

            // ── Notification governance (W2-4) ──────────────
            // Read-only telemetry about the notification system itself: how
            // many of each type went out, and how many led a person to
            // actually decide something (P4-5 / C12). Manager+ because it
            // aggregates across every agent in the deployment.
            "notify.stats" => {
                require_manager!();
                self.handle_notify_stats(params)
            }

            // ── Live Run Forking (RFC-26) ───────────────────
            // F5-D: branch outputs are an employee's work; Manager role plus a
            // live binding on the fork's employee (checked in the handler).
            "fork.list" => {
                require_manager!();
                self.handle_fork_list(params, ctx)
            }
            "fork.inspect" => {
                require_manager!();
                self.handle_fork_inspect(params, ctx)
            }
            // Resolving a fork promotes a winner's workspace → side-effecting.
            "fork.resolve" => {
                require_manager!();
                self.handle_fork_resolve(params)
            }

            // ── Migrate-from (spawns the CLI's `migrate from --json`) ──
            // scan = dry-run plan; apply = actual writes. Both shell out to
            // this same binary (`current_exe`) so the gateway never has to
            // depend on the duduclaw-cli crate.
            "migrate.scan" => {
                require_manager!();
                self.handle_migrate_scan(params).await
            }
            "migrate.apply" => {
                require_manager!();
                self.handle_migrate_apply(params).await
            }

            // ── Approvals (WP14-T14.7 approval center) ──────
            "approvals.operations" => {
                require_admin!();
                self.handle_approval_operations(params, ctx, false).await
            }
            "approvals.resolve_uncertain" => {
                require_admin!();
                self.handle_approval_operations(params, ctx, true).await
            }
            "approvals.list" => {
                require_manager!();
                self.handle_approvals_list(params,ctx).await
            }
            "approvals.decide" => {
                require_manager!();
                self.handle_approvals_decide(params, ctx).await
            }

            // ── Agent Mail (P2-d) — 信箱 ────────────────────
            // Same gate as the approval centre: mail is real customer
            // correspondence and confirming a send is a real-world act.
            "mail.status" => {
                require_manager!();
                self.handle_mail_status().await
            }
            "mail.list" => {
                require_manager!();
                self.handle_mail_list(params).await
            }
            "mail.read" => {
                require_manager!();
                self.handle_mail_read(params, ctx).await
            }
            "mail.archive" => {
                require_manager!();
                self.handle_mail_archive(params, ctx).await
            }
            "mail.outbox" => {
                require_manager!();
                self.handle_mail_outbox(params).await
            }
            "mail.decide" => {
                require_manager!();
                self.handle_mail_decide(params, ctx).await
            }

            // ── D5 topology evolution: routing overrides + pending reroute proposals ──
            "topology.list" => {
                require_manager!();
                self.handle_topology_list().await
            }

            // ── W3-1 human takeover: READ-ONLY surface ──────
            // Deliberately the only takeover RPC. Starting / extending /
            // ending a takeover is a channel-side act (speaking, or
            // `/takeover` in the conversation) — mirroring it as a dashboard
            // write would create a second authorization model for the same
            // state and let somebody "take over" a conversation they are not
            // actually in. The dashboard's job here is to show that a human
            // is on it; a UI consumer is a follow-up.
            "takeover.list" => {
                require_manager!();
                self.handle_takeover_list()
            }

            // Install approval requests (Skill / MCP two-stage signature chain)
            "install_requests.list" => {
                require_manager!();
                self.handle_install_requests_list(ctx).await
            }
            "install_requests.mine" => self.handle_install_requests_mine(ctx).await,
            "install_requests.decide" => {
                require_manager!();
                self.handle_install_requests_decide(params, ctx).await
            }

            // ── Budget incidents (WP14-T14.6) ───────────────
            "budget.incidents" => {
                require_manager!();
                self.handle_budget_incidents(params).await
            }

            // ── Autopilot (admin only) ──────────────────────
            "autopilot.list" => {
                require_admin!();
                self.handle_autopilot_list().await
            }

            _ => self.dispatch_ops(method, params, ctx, conn).await,
        }
    }
}
