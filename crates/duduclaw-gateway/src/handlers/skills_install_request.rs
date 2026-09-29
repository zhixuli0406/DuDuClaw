//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Install approval requests (non-admin Skill / MCP install) ───
    //
    // A non-admin who wants to install a Skill/MCP files a request that
    // carries the item's function + security-scan verdict; it flows through a
    // signature chain (employee → manager → admin; manager → admin) before the
    // install runs. See `install_requests.rs` for the store + role logic.

    /// Extract a one-line functional description from SKILL.md frontmatter
    /// (`description:` field, else the first non-frontmatter prose line).
    pub(crate) fn skill_description_from_content(content: &str) -> String {
        if let Some(d) = content
            .lines()
            .find(|l| l.trim_start().starts_with("description:"))
            .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        {
            if !d.is_empty() {
                return duduclaw_core::truncate_chars(&d, 300);
            }
        }
        let prose = content
            .lines()
            .skip_while(|l| l.trim() == "---" || l.contains(':') || l.trim().is_empty())
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        duduclaw_core::truncate_chars(prose, 300)
    }

    pub(crate) fn scan_findings_json(
        scan: &crate::skill_lifecycle::security_scanner::SecurityScanResult,
    ) -> Value {
        json!(
            scan.findings
                .iter()
                .map(|f| json!({
                    "category": format!("{:?}", f.category),
                    "severity": format!("{:?}", f.severity).to_lowercase(),
                    "description": f.description,
                    "pattern": f.matched_pattern,
                }))
                .collect::<Vec<_>>()
        )
    }

    /// File a Skill install request (non-admin). Scans fail-closed: a
    /// risk ≥ High skill is rejected outright (no request is created).
    /// Best-effort: proactively DM the request's current-stage approvers on
    /// their linked channels (Feature C). Spawned detached so channel HTTP
    /// never delays the RPC response; failures are logged inside.
    pub(crate) async fn spawn_install_notify(&self, request_id: String) {
        let db = match self.user_db.read().await.as_ref() {
            Some(d) => d.clone(),
            None => return,
        };
        let home = self.home_dir.clone();
        tokio::spawn(async move {
            if let Ok(store) = crate::install_requests::InstallRequestStore::open(&home) {
                if let Ok(Some(req)) = store.get(&request_id).await {
                    crate::install_notify::notify_install_approvers(&home, &db, &req).await;
                }
            }
        });
    }

    /// Best-effort: tell the requester the FINAL outcome of their request on
    /// their linked channels (approved+installed / install failed / denied).
    /// Spawned detached so channel HTTP never delays the RPC response.
    pub(crate) async fn spawn_requester_notify(&self, request_id: String, text: String) {
        let db = match self.user_db.read().await.as_ref() {
            Some(d) => d.clone(),
            None => return,
        };
        let home = self.home_dir.clone();
        tokio::spawn(async move {
            if let Ok(store) = crate::install_requests::InstallRequestStore::open(&home) {
                if let Ok(Some(req)) = store.get(&request_id).await {
                    crate::install_notify::notify_requester(&home, &db, &req, &text).await;
                }
            }
        });
    }

    /// Look up a user's department from the user DB (`None` if unset / no DB).
    pub(crate) async fn user_department(&self, user_id: &str) -> Option<String> {
        let db = self.user_db.read().await.as_ref()?.clone();
        db.get_user(user_id)
            .ok()
            .flatten()
            .and_then(|u| u.department)
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
    }

    /// Look up a dashboard user's display name (`None` if unset / no DB) —
    /// the decider attribution a dashboard-originated decision passes into
    /// `decision_card::collapse_all` in place of the channel-press
    /// `resolve_decider_name` lookup (there is no channel identity to map
    /// from when the decision was made in the dashboard itself).
    pub(crate) async fn user_display_name(&self, user_id: &str) -> Option<String> {
        let db = self.user_db.read().await.as_ref()?.clone();
        db.get_user(user_id)
            .ok()
            .flatten()
            .map(|u| u.display_name.trim().to_string())
            .filter(|d| !d.is_empty())
    }

    pub(crate) async fn handle_skills_install_request(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let scope = match params.get("scope").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'scope' parameter"),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => return WsFrame::error_response("", "Missing 'content' parameter"),
        };

        // Non-admins scope-install only where they have access. `global` /
        // `department:*` are org-wide and reserved for the eventual admin
        // signer; an employee/manager may only *request* into an agent they can
        // reach. (The admin who signs can still see the requested scope.)
        // Validate agent scope binding for non-global scopes.
        if scope != "global" && !scope.starts_with("department:") {
            if let Err(e) = acl::require_agent_access(ctx, &scope, AccessLevel::Operator) {
                return WsFrame::error_response("", &e);
            }
        }

        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let scan = crate::skill_lifecycle::security_scanner::scan_skill(&content, None);
        if !scan.passed {
            return WsFrame::error_response(
                "",
                &format!(
                    "安全掃描未通過，無法送出申請：風險 {:?}，{} 項問題",
                    scan.risk_level,
                    scan.findings.len()
                ),
            );
        }

        let store = match crate::install_requests::InstallRequestStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open install requests: {e}")),
        };
        let description = Self::skill_description_from_content(&content);
        let payload = json!({ "scope": scope, "content": content, "url": url });
        let scan_json = Self::scan_findings_json(&scan);
        let department = self.user_department(&ctx.user_id).await;
        match store
            .create(
                "skill",
                &skill_name,
                &description,
                &ctx.user_id,
                &ctx.email,
                &ctx.role.to_string(),
                department.as_deref(),
                &format!("{:?}", scan.risk_level),
                &scan_json,
                &payload,
                crate::install_requests::DEFAULT_INSTALL_TTL_SECONDS,
            )
            .await
        {
            Ok(id) => {
                let stage = if ctx.role == UserRole::Employee {
                    "awaiting_manager"
                } else {
                    "awaiting_admin"
                };
                self.spawn_install_notify(id.clone()).await;
                WsFrame::ok_response(
                    "",
                    json!({
                        "request_id": id,
                        "status": "pending",
                        "stage": stage,
                        "scan": scan_json,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("建立申請失敗：{e}")),
        }
    }

    /// File an MCP install request (non-admin). Scans the server definition
    /// fail-closed before creating the request.
    pub(crate) async fn handle_mcp_install_request(&self, params: Value, ctx: &UserContext) -> WsFrame {
        use duduclaw_agent::mcp_template::McpServerDef;

        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        // A requester must have operator access to the target agent.
        if let Err(e) = acl::require_agent_access(ctx, &agent_id, AccessLevel::Operator) {
            return WsFrame::error_response("", &e);
        }
        let server_name = match params.get("server_name").and_then(|v| v.as_str()) {
            Some(s) if crate::mcp_scan::is_valid_mcp_server_name(s) => s.to_string(),
            _ => {
                return WsFrame::error_response(
                    "",
                    "Invalid server_name (allowed: A-Za-z0-9._- max 64)",
                );
            }
        };
        let def: McpServerDef = match params.get("server_def") {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(d) => d,
                Err(e) => return WsFrame::error_response("", &format!("Invalid server_def: {e}")),
            },
            None => return WsFrame::error_response("", "Missing 'server_def' parameter"),
        };

        let scan = crate::mcp_scan::scan_mcp_server_def(&server_name, &def);
        if !scan.passed {
            return WsFrame::error_response(
                "",
                &format!(
                    "安全掃描未通過，無法送出申請：風險 {:?}，{} 項問題",
                    scan.risk_level,
                    scan.findings.len()
                ),
            );
        }

        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .map(|d| duduclaw_core::truncate_chars(d, 300))
            .unwrap_or_else(|| format!("MCP server: {} {}", def.command, def.args.join(" ")));
        let store = match crate::install_requests::InstallRequestStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open install requests: {e}")),
        };
        let payload = json!({
            "agent_id": agent_id,
            "server_name": server_name,
            "server_def": def,
            "add_to_catalog": params.get("add_to_catalog").and_then(|v| v.as_bool()).unwrap_or(false),
            "description": description,
            "source_url": params.get("source_url").and_then(|v| v.as_str()).unwrap_or(""),
        });
        let scan_json = Self::mcp_scan_to_json(&scan)["findings"].clone();
        let title = format!("{server_name} → {agent_id}");
        let department = self.user_department(&ctx.user_id).await;
        match store
            .create(
                "mcp",
                &title,
                &description,
                &ctx.user_id,
                &ctx.email,
                &ctx.role.to_string(),
                department.as_deref(),
                &format!("{:?}", scan.risk_level),
                &scan_json,
                &payload,
                crate::install_requests::DEFAULT_INSTALL_TTL_SECONDS,
            )
            .await
        {
            Ok(id) => {
                let stage = if ctx.role == UserRole::Employee {
                    "awaiting_manager"
                } else {
                    "awaiting_admin"
                };
                self.spawn_install_notify(id.clone()).await;
                WsFrame::ok_response(
                    "",
                    json!({
                        "request_id": id,
                        "status": "pending",
                        "stage": stage,
                        "scan": scan_json,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("建立申請失敗：{e}")),
        }
    }

    /// List install requests the caller (manager+) can currently act on.
    /// Admin sees all pending; a manager sees only employee requests still
    /// awaiting the manager gate AND belonging to the manager's own department
    /// (a request with no department falls back to any manager).
    pub(crate) async fn handle_install_requests_list(&self, ctx: &UserContext) -> WsFrame {
        let store = match crate::install_requests::InstallRequestStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open install requests: {e}")),
        };
        let pending = match store.list_pending().await {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("list: {e}")),
        };
        let is_admin = ctx.role == UserRole::Admin;
        let mgr_dept = if is_admin {
            None
        } else {
            self.user_department(&ctx.user_id).await
        };
        let actionable: Vec<_> = pending
            .into_iter()
            .filter(|r| {
                if is_admin {
                    true
                } else {
                    // manager: employee requests still needing the manager gate,
                    // routed to THIS manager's department.
                    r.requester_role == "employee"
                        && r.manager_by.is_none()
                        && r.manager_may_sign(mgr_dept.as_deref())
                }
            })
            .collect();
        let items = self.install_requests_with_channel_link(&actionable).await;
        WsFrame::ok_response("", json!({ "requests": items, "count": items.len() }))
    }

    /// The caller's own install requests (any authenticated user).
    pub(crate) async fn handle_install_requests_mine(&self, ctx: &UserContext) -> WsFrame {
        let store = match crate::install_requests::InstallRequestStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open install requests: {e}")),
        };
        match store.list_for_requester(&ctx.user_id).await {
            Ok(rows) => {
                let items = self.install_requests_with_channel_link(&rows).await;
                WsFrame::ok_response("", json!({ "requests": items, "count": items.len() }))
            }
            Err(e) => WsFrame::error_response("", &format!("list mine: {e}")),
        }
    }

    /// E8 extension: enrich each install request's JSON with the same "open in
    /// `channel`" reverse-handoff pair (`channel` / `channel_link`) tasks and
    /// approvals already carry (E8). Resolution goes through
    /// `install_notify::resolve_channel_target` (recorded decision card, else
    /// the current stage's first reachable approver — see that function's
    /// docs) and then `channel_link::resolve_conversation_link` to turn
    /// `(channel, chat_id, message_id)` into an actual URL. Best-effort: no
    /// user DB, no resolvable target, or no constructible link for the
    /// platform all degrade to `null` — never a raw chat/message id crosses
    /// to the frontend (project convention: internal identifiers don't leak
    /// to the UI), and a resolution failure never blocks the list itself.
    pub(crate) async fn install_requests_with_channel_link(
        &self,
        rows: &[crate::install_requests::InstallRequest],
    ) -> Vec<Value> {
        let db = self.user_db.read().await.as_ref().cloned();
        let mut items = Vec::with_capacity(rows.len());
        for r in rows {
            let mut v = r.to_json();
            let resolved = match &db {
                Some(db) => {
                    crate::install_notify::resolve_channel_target(&self.home_dir, db, r).await
                }
                None => None,
            };
            let channel_link = match resolved {
                Some((channel, chat_id, message_id)) => {
                    // W2-7: install-request targets don't snapshot a Discord
                    // guild id anywhere today, so this passes `None` — same
                    // honest gap as before this parameter existed, not a
                    // regression (see `channel_link.rs` module docs).
                    crate::channel_link::resolve_conversation_link(
                        &self.home_dir,
                        &channel,
                        &chat_id,
                        message_id.as_deref(),
                        None,
                    )
                    .await
                    .map(|link| (channel, link))
                }
                None => None,
            };
            v["channel"] = json!(channel_link.as_ref().map(|(c, _)| c.clone()));
            v["channel_link"] = json!(channel_link.as_ref().map(|(_, l)| l.clone()));
            items.push(v);
        }
        items
    }

    /// Decide an install request (manager+). On final approval the install is
    /// executed server-side with a fail-closed re-scan.
    pub(crate) async fn handle_install_requests_decide(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "id is required"),
        };
        let approve = params
            .get("approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let store = match crate::install_requests::InstallRequestStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open install requests: {e}")),
        };
        let decider = format!("{}:{}", ctx.role, ctx.user_id);
        let decider_dept = self.user_department(&ctx.user_id).await;
        let outcome = match store
            .decide(
                &id,
                &decider,
                &ctx.role.to_string(),
                decider_dept.as_deref(),
                approve,
                &reason,
            )
            .await
        {
            Ok(o) => o,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // H1 (unified decision hand-off, 07-unified-decision-design.md §6):
        // retire the channel card(s) this request fanned out to, the same
        // way a channel button press does. `approve == false` always denies
        // (softer "已婉拒" verb); `approve == true` covers both the
        // manager-stage and final-stage outcomes below, which read
        // identically as "已同意" — same mapping the channel path uses.
        // Fire-and-forget: cosmetic, must never delay or fail a decision
        // already durable in `install_requests.db`.
        {
            let decider_name = self.user_display_name(&ctx.user_id).await;
            let verb = if approve {
                crate::decision_card::DecisionVerb::Approved
            } else {
                crate::decision_card::DecisionVerb::DeclinedInstall
            };
            crate::install_notify::spawn_dashboard_collapse(
                self.home_dir.clone(),
                id.clone(),
                decider_name,
                verb,
            );
        }

        match outcome {
            crate::install_requests::DecideOutcome::Denied => {
                self.spawn_requester_notify(
                    id.clone(),
                    "❌ 您的安裝申請已被退回。詳情請見儀表板。".to_string(),
                )
                .await;
                WsFrame::ok_response("", json!({ "status": "denied" }))
            }
            crate::install_requests::DecideOutcome::AdvancedToAdmin => {
                // Manager cleared stage 1 → notify the admins (stage 2).
                self.spawn_install_notify(id.clone()).await;
                WsFrame::ok_response(
                    "",
                    json!({ "status": "pending", "stage": "awaiting_admin" }),
                )
            }
            crate::install_requests::DecideOutcome::ReadyToExecute => {
                let req = match store.get(&id).await {
                    Ok(Some(r)) => r,
                    _ => {
                        return WsFrame::error_response(
                            "",
                            "approved, but the request vanished before execution",
                        );
                    }
                };
                let exec = self.execute_approved_install(&req).await;
                let ok = exec.is_ok();
                let _ = store
                    .mark_executed(&id, ok, exec.as_ref().err().map(|s| s.as_str()))
                    .await;
                let requester_text = if ok {
                    format!("✅ 您的安裝申請「{}」已核准並完成安裝。", req.title)
                } else {
                    format!(
                        "⚠️ 您的安裝申請「{}」已核准，但安裝執行失敗。詳情請見儀表板。",
                        req.title
                    )
                };
                self.spawn_requester_notify(id.clone(), requester_text)
                    .await;
                match exec {
                    Ok(detail) => WsFrame::ok_response(
                        "",
                        json!({
                            "status": "approved",
                            "executed": true,
                            "detail": detail,
                        }),
                    ),
                    Err(e) => WsFrame::ok_response(
                        "",
                        json!({
                            "status": "approved",
                            "executed": false,
                            // Honest partial: the signatures landed, execution failed.
                            "warning": format!("已完成簽核，但安裝執行失敗：{e}"),
                        }),
                    ),
                }
            }
        }
    }

    /// Run the actual install for a fully-approved request. Re-scans
    /// fail-closed so a request whose target changed risk since filing (or a
    /// tampered payload) is still blocked at execution time.
    pub(crate) async fn execute_approved_install(
        &self,
        req: &crate::install_requests::InstallRequest,
    ) -> Result<Value, String> {
        match req.kind.as_str() {
            "skill" => {
                let scope = req
                    .payload
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = req
                    .payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if scope.is_empty() || content.is_empty() {
                    return Err("request payload missing scope/content".into());
                }
                let scan = crate::skill_lifecycle::security_scanner::scan_skill(content, None);
                if !scan.passed {
                    return Err(format!(
                        "re-scan rejected skill: risk {:?}",
                        scan.risk_level
                    ));
                }
                let skill_name = content
                    .lines()
                    .find(|l| l.starts_with("name:"))
                    .and_then(|l| l.strip_prefix("name:"))
                    .map(|n| n.trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let installed = self.run_skill_install(scope, content, &skill_name).await?;
                // Persist the scan verdict so the installed skill's "security"
                // column reflects that it passed vetting instead of showing
                // "Not scanned" (Bug#9). Best-effort — a write failure must not
                // fail an otherwise-successful install.
                self.record_skill_scan_verdict(&installed, scan.risk_level);
                Ok(json!({ "skill_name": installed, "scope": scope }))
            }
            "mcp" => {
                use duduclaw_agent::mcp_template::{McpServerDef, add_server_to_config};
                let agent_id = req
                    .payload
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let server_name = req
                    .payload
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let def: McpServerDef = req
                    .payload
                    .get("server_def")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .ok_or_else(|| "request payload missing server_def".to_string())?;
                if !is_valid_agent_id(agent_id)
                    || !crate::mcp_scan::is_valid_mcp_server_name(server_name)
                {
                    return Err("invalid agent_id/server_name in payload".into());
                }
                let scan = crate::mcp_scan::scan_mcp_server_def(server_name, &def);
                if !scan.passed {
                    return Err(format!(
                        "re-scan rejected MCP server: risk {:?}",
                        scan.risk_level
                    ));
                }
                let agent_dir = self.home_dir.join("agents").join(agent_id);
                if !agent_dir.is_dir() {
                    return Err(format!("agent '{agent_id}' not found"));
                }
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                let d = def.clone();
                tokio::task::spawn_blocking(move || add_server_to_config(&ad, &sn, &d))
                    .await
                    .map_err(|e| format!("join: {e}"))??;
                if req
                    .payload
                    .get("add_to_catalog")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let description = req
                        .payload
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let source_url = req
                        .payload
                        .get("source_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let _ = self
                        .append_to_user_marketplace(server_name, &def, description, source_url)
                        .await;
                }
                Ok(json!({ "server_name": server_name, "agent_id": agent_id }))
            }
            other => Err(format!("unknown request kind: {other}")),
        }
    }

    /// Sidecar path mapping installed skill name → security-scan verdict
    /// (`pass` / `warn` / `fail`). Lets `skills.list` surface a real verdict on
    /// approved installs instead of "Not scanned" (Bug#9).
    pub(crate) fn skill_scan_verdicts_path(&self) -> std::path::PathBuf {
        self.home_dir.join("skill_scan_verdicts.json")
    }

    /// Map a scan risk level to the 3-state frontend verdict.
    pub(crate) fn risk_to_verdict(risk: crate::skill_lifecycle::security_scanner::RiskLevel) -> &'static str {
        use crate::skill_lifecycle::security_scanner::RiskLevel;
        match risk {
            RiskLevel::Clean | RiskLevel::Low => "pass",
            RiskLevel::Medium => "warn",
            RiskLevel::High | RiskLevel::Critical => "fail",
        }
    }

    /// Record a skill's scan verdict in the sidecar map (cross-process safe via
    /// an advisory file lock — coding convention #3). Best-effort: any error is
    /// swallowed so it never blocks an install.
    pub(crate) fn record_skill_scan_verdict(
        &self,
        skill_name: &str,
        risk: crate::skill_lifecycle::security_scanner::RiskLevel,
    ) {
        let path = self.skill_scan_verdicts_path();
        let name = skill_name.to_string();
        let verdict = Self::risk_to_verdict(risk).to_string();
        let _ = duduclaw_core::with_file_lock(&path, || {
            let mut map: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            map.insert(name.clone(), Value::String(verdict.clone()));
            if let Ok(serialized) = serde_json::to_string(&Value::Object(map)) {
                let _ = std::fs::write(&path, serialized);
            }
            Ok::<(), std::io::Error>(())
        });
    }

    /// Load the skill scan-verdict map (name → `pass`/`warn`/`fail`). Missing or
    /// malformed file ⇒ empty map (no verdict shown, same as before).
    pub(crate) fn load_skill_scan_verdicts(&self) -> serde_json::Map<String, Value> {
        std::fs::read_to_string(self.skill_scan_verdicts_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}
