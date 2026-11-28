//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;
use crate::trigger_route::texts::Lang;
use crate::approval::decider_display::decider_display;

impl MethodHandler {
    // ── Autopilot handlers ──────────────────────────────────

    /// List pending approvals, or recent decided approvals for live Admins.
    /// Optional exact agent/kind filters and a bounded result limit.
    pub(crate) async fn handle_approvals_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        self.approvals_read(params, ctx, false).await
    }

    pub(crate) async fn handle_approvals_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if params.get("id").and_then(Value::as_str).is_none_or(|id| id.is_empty()) {
            return WsFrame::error_response("", "id is required");
        }
        self.approvals_read(params, ctx, true).await
    }

    async fn approvals_read(&self, mut params: Value, ctx: &UserContext, detail: bool) -> WsFrame {
        let step_up = super::personal_data_gate::take_step_up(&mut params);
        let status = match params.get("status") {
            None => "pending",
            Some(Value::String(value)) if matches!(value.as_str(), "pending" | "decided") => value.as_str(),
            _ => return WsFrame::error_response("", "invalid status"),
        };
        let requested_limit = match params.get("limit") {
            None => None,
            Some(value) => match value.as_u64().filter(|limit| (1..=100).contains(limit)) {
                Some(limit) => Some(limit as u32),
                None => return WsFrame::error_response("", "invalid limit"),
            },
        };
        let agent_filter = params.get("agent_id").and_then(|v| v.as_str());
        // Optional exact `action_kind` filter (e.g. "knowledge_quarantine" for the
        // D6 curation queue). Matches the raw stored action_kind string.
        let action_kind_filter = params
            .get("action_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        // A-M-3: rows are filtered by the viewer's live role and employee
        // bindings, not by the session's claims. One identity read per request.
        let (fresh, department) = {
            let (home, session) = (self.home_dir.clone(), ctx.clone());
            match tokio::task::spawn_blocking(move || {
                super::approval_visibility::fresh_reader(&home, &session)
            })
            .await
            {
                Ok(Some(reader)) => reader,
                _ => return WsFrame::error_response("", "permission denied"),
            }
        };
        if status == "decided" && !fresh.is_admin() {
            return WsFrame::error_response("", "permission denied");
        }
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let rows = if let Some(id) = params.get("id").and_then(Value::as_str) {
            broker
                .get(&crate::approval::ApprovalId::from(id.to_owned()))
                .await
                .map(|row| {
                    row.into_iter()
                        .filter(|r| agent_filter.is_none_or(|agent| agent == r.agent_id))
                        .collect()
                })
        } else if status == "decided" {
            broker.list_decided(agent_filter, action_kind_filter, requested_limit.unwrap_or(50)).await
        } else {
            broker.list_pending(agent_filter).await.map(|rows| rows.into_iter()
                .filter(|r| action_kind_filter.is_none_or(|kind| r.action_kind == kind))
                .collect::<Vec<_>>()).map(|rows| match requested_limit {
                    Some(limit) => rows.into_iter().take(limit as usize).collect(),
                    None => rows,
                })
        };
        let grants = crate::dept_grants::DeptGrantView::load(self, ctx).await;
        match rows {
            Ok(rows) => {
                // v1.81 P1b: another person's Personal employee → metadata-only
                // card; its payload needs the owner or an audited step-up.
                let by_id = params.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
                let personal = match self.personal_approval_hidden(ctx, step_up, by_id, &rows).await {
                    Ok(hidden) => hidden,
                    Err(frame) => return frame,
                };
                // Resolve names once per request, keeping users.db and config I/O
                // off the async thread. A failed lookup never exposes an id.
                let (users, home) = (self.user_db.read().await.clone(), self.home_dir.clone());
                let (decider_names, lang) = tokio::task::spawn_blocking(move || {
                    let names: std::collections::HashMap<String, String> = users
                        .and_then(|db| db.list_users().ok())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|user| (user.id, user.display_name))
                        .collect();
                    (names, crate::trigger_route::texts::lang_for(&home))
                }).await.unwrap_or_else(|_| (std::collections::HashMap::new(), Lang::ZhTw));
                let mut items: Vec<Value> = Vec::with_capacity(rows.len());
                let mut open_audit_failed = false;
                for r in rows
                    .iter()
                    .filter(|r| action_kind_filter.map_or(true, |k| r.action_kind == k))
                {
                    let (home, record) = (self.home_dir.clone(), r.clone());
                    let reader = fresh.clone();
                    let visibility = match tokio::task::spawn_blocking(move || {
                        super::approval_visibility::ApprovalVisibility::load(&home, &record, &reader)
                    }).await {
                        Ok(visibility) => visibility,
                        Err(_) => continue,
                    };
                    if !visibility.may_read(r, &fresh, department.as_deref()) { continue; }
                    if personal.contains(&r.agent_id) {
                        items.push(super::personal_data_gate::personal_metadata_card(r));
                        continue;
                    }
                    // Binding data and question answers go only to a viewer
                    // who may decide this card.
                    let outbound = crate::approval::is_outbound_data_card(r);
                    let reviewer = outbound && crate::approval::outbound_reviewer(&grants, &fresh, r);
                    let may_decide = (!outbound || reviewer)
                        && (r.binding.is_none()
                            || Box::pin(self.authorize_bound_decider(r, ctx)).await.is_ok());
                    let mut full_outbound = outbound && detail && reviewer;
                    if full_outbound && !fresh.is_admin() {
                        let (home, record, user) = (self.home_dir.clone(), r.clone(), fresh.user_id.clone());
                        if !matches!(tokio::task::spawn_blocking(move || {
                            crate::approval::outbound::audit_opened(&home, &record, &user)
                        }).await, Ok(Ok(()))) {
                            full_outbound = false;
                            open_audit_failed = true;
                        }
                    }
                    let activation = super::approval_decider::is_activation_record(r);
                    let kind = crate::governance::ApprovalKind::parse(&r.action_kind);
                    // E8 reverse handoff: "在 <通道> 中開啟" — the WP20
                    // `notify_channel`/`notify_chat_id` columns already record
                    // where this approval's decision card was actually pushed
                    // (the conversation to jump back to); `decision_message_store`
                    // has the exact message id for it when a card was recorded
                    // there. A goal-kickoff approval (`action_kind ==
                    // "goal_kickoff"`) files its card under the `goal_kickoff`
                    // namespace instead of `approval` (`goal_notify::notify_goal_kickoff`
                    // is self-notifying — see `SELF_NOTIFYING_KINDS` in
                    // `approval.rs`), so the namespace must match. Only the
                    // resolved URL (or nothing) crosses to the frontend — the raw
                    // `chat_id` never does.
                    let namespace = if r.action_kind == "goal_kickoff" {
                        crate::decision_action::DecisionSource::Kickoff.namespace()
                    } else {
                        crate::decision_action::DecisionSource::Approval.namespace()
                    };
                    let channel_link =
                        match (r.notify_channel.as_deref(), r.notify_chat_id.as_deref()) {
                            (Some(channel), Some(chat_id))
                                if !channel.is_empty() && !chat_id.is_empty() =>
                            {
                                let message_id =
                                    crate::decision_message_store::lookup_card_message(
                                        &self.home_dir,
                                        namespace,
                                        r.id.as_str(),
                                        channel,
                                        chat_id,
                                    )
                                    .map(|p| p.message_id);
                                // W2-7: Discord's guild id was snapshotted onto
                                // this card's entry at push time — pass it
                                // through rather than re-resolving it live.
                                let discord_guild_id =
                                    crate::decision_message_store::lookup_card_discord_guild_id(
                                        &self.home_dir,
                                        namespace,
                                        r.id.as_str(),
                                        channel,
                                        chat_id,
                                    );
                                crate::channel_link::resolve_conversation_link(
                                    &self.home_dir,
                                    channel,
                                    chat_id,
                                    message_id.as_deref(),
                                    discord_guild_id.as_deref(),
                                )
                                .await
                            }
                            _ => None,
                        };
                    let activation_facts = if activation && may_decide && (!outbound || full_outbound) {
                        super::approval_decider::activation_facts(&self.home_dir, r)
                    } else {
                        Value::Null
                    };
                    items.push(json!({
                        "id": r.id.as_str(),
                        "digest": if outbound {
                            if full_outbound && may_decide { json!(crate::approval::decision_digest_with_activation(r, &activation_facts)) }
                            else { Value::Null }
                        } else { viewer_digest(r, &activation_facts, fresh.is_admin(), may_decide) },
                        "agent_id": r.agent_id,
                        "kind": kind.as_str(),
                        "summary": if full_outbound { &r.summary } else { crate::approval::outbound::list_summary(r) },
                        "payload": if outbound {
                            if full_outbound { r.payload.clone() } else { json!({"redacted": true}) }
                        } else if r.action_kind == crate::admin_ai::card::ACTION_KIND {
                            crate::admin_ai::card::confirmation_display(r)
                        } else if r.binding.is_some() {json!({
                            "redacted": true,
                            "payload_hash": r.binding.as_ref().map(|b|&b.payload_hash)
                        })}else{dashboard_action_payload(&r.payload, fresh.is_admin())},
                        "execution_result": if !outbound && r.action_kind == crate::admin_ai::card::ACTION_KIND {
                            json!(r.execution_result.as_ref().map(crate::admin_ai::card::execution_result_projection))
                        } else { Value::Null },
                        "decided_at": if may_decide && (!outbound || full_outbound) { json!(r.decided_at) } else { Value::Null },
                        "request_kind":r.request_kind,
                        "status":r.status,
                        "decided_by_display": viewer_decider_display(
                            r.decided_by.as_deref(), may_decide && (!outbound || full_outbound),
                            |user| decider_names.get(user).cloned(), lang,
                        ),
                        "binding": if may_decide && (!outbound || full_outbound) && r.action_kind != crate::admin_ai::card::ACTION_KIND { json!(r.binding) } else { Value::Null },
                        "answer": if may_decide && (!outbound || full_outbound) { json!(r.answer) } else { Value::Null },
                        "may_decide": may_decide,
                        "mail_requester_approval_eligible": crate::approval::mail_requester::eligible(&self.home_dir, r),
                        "decided_in_dashboard_only": crate::approval_notify::is_dashboard_only_record(Some(&self.home_dir), r),
                        "submitter_is_viewer": activation
                            && super::approval_decider::submitter_is_decider(r, &fresh),
                        // A-H-1: what an Admin accepts — each effect and the
                        // one record it may change.
                        "workflow_activation": activation_facts,
                        "invalidated_reason": if outbound && !full_outbound { Value::Null } else { json!(r.invalidated_reason) },
                        "created_at": r.created_at,
                        "ttl_seconds": r.ttl_seconds,
                        // Epoch seconds the approval auto-denies at (TTL
                        // expiry counts as a denial — see approval.rs). Lets
                        // the dashboard render a live countdown without
                        // parsing `created_at` itself. `null` on an
                        // unparseable `created_at` (fail-safe).
                        "expires_at": r.expires_at_epoch(),
                        // D1/D2: the ActionGuard judge's forward-simulation
                        // narrative (world_state_change + risk_points), when the
                        // approval kind ran that judge. `null` for every other
                        // kind (the overwhelming majority) — the dashboard
                        // renders nothing when this is absent, purely additive.
                        "simulation": if outbound && !full_outbound { Value::Null } else { json!(r.simulation) },
                        // E8: platform name (safe to expose — no internal id)
                        // + a fully-resolved "open in channel" URL, or `null`
                        // when nothing could be constructed.
                        "channel": r.notify_channel,
                        "channel_link": channel_link,
                    }));
                }
                let payload = json!({ "approvals": items, "count": items.len() });
                if open_audit_failed {
                    WsFrame::Response {
                        id: String::new(), ok: false, payload: Some(payload),
                        error: Some(json!("無法記錄審核卡開啟紀錄，內容暫不顯示，請稍後重試。")),
                    }
                } else {
                    WsFrame::ok_response("", payload)
                }
            }
            Err(e) => WsFrame::error_response("", &format!("list approvals: {e}")),
        }
    }

    /// D5: list routing overrides + pending reroute proposals. Read-only view of
    /// `routing_overrides.json` (fail-safe: missing / corrupt ⇒ empty lists) so
    /// the dashboard can surface D5 topology-evolution state. Proposals are
    /// human-gated through the ApprovalBroker; approve/deny goes through the
    /// generic `approvals.decide` RPC.
    pub(crate) async fn handle_topology_list(&self) -> WsFrame {
        let doc = crate::topology_evolution::load_file(&self.home_dir);
        let overrides: Vec<Value> = doc
            .overrides
            .iter()
            .map(|o| {
                json!({
                    "task_class": o.task_class,
                    "from_agent": o.from_agent,
                    "to_agent": o.to_agent,
                    "approved_at": o.approved_at,
                    "observe_until": o.observe_until,
                    "status": o.status,
                    "baseline_reject_rate": o.baseline_reject_rate,
                    "extended": o.extended,
                })
            })
            .collect();
        let pending: Vec<Value> = doc
            .proposals
            .iter()
            .filter(|p| p.status == crate::topology_evolution::PROPOSAL_PENDING)
            .map(|p| {
                json!({
                    "id": p.id,
                    "task_class": p.task_class,
                    "from_agent": p.from_agent,
                    "to_agent": p.to_agent,
                    "created_at": p.created_at,
                    "approval_id": p.approval_id,
                    "samples": p.samples,
                    "reject_rate": p.reject_rate,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "overrides": overrides,
                "pending_proposals": pending,
                "override_count": overrides.len(),
                "pending_count": pending.len(),
            }),
        )
    }

    /// W3-1: conversations a human has currently taken over (read-only).
    ///
    /// No write twin by design — see the `takeover.list` dispatch note.
    /// `holder_user_id` is deliberately omitted: the dashboard needs to show
    /// *who*, and the display name does that without exposing a channel
    /// account id to every manager who opens the page.
    pub(crate) fn handle_takeover_list(&self) -> WsFrame {
        let now = chrono::Utc::now();
        let items: Vec<Value> = duduclaw_core::takeover_state::list_active(&self.home_dir)
            .into_iter()
            .map(|r| {
                json!({
                    "conversation": r.conversation,
                    "channel": r.channel,
                    "channel_label": crate::takeover::channel_label(&r.channel),
                    "chat_id": r.chat_id,
                    "agent_id": r.agent_id,
                    "holder_display": r.holder_display,
                    "started_at": r.started_at.to_rfc3339(),
                    "until": r.until.to_rfc3339(),
                    "minutes_left": r.minutes_left(now),
                    "claimed_task_ids": r.claimed_task_ids,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "count": items.len(), "items": items }))
    }
}

// Decision attribution follows the same visibility as binding/answer data.
fn viewer_decider_display(
    marker: Option<&str>,
    visible: bool,
    name_of: impl Fn(&str) -> Option<String>,
    lang: Lang,
) -> Option<String> {
    decider_display(marker.filter(|_| visible)?, name_of, lang)
}


#[cfg(test)]
mod live_event_tests {
    use super::*;
    use crate::handlers::task_privacy_tests::{admin, fixture, ok, payload};

    #[test]
    fn decision_attribution_displays_names_and_localized_sources() {
        let user = "14300000-0000-4000-8000-000000000022";
        for (suffix, expected) in [
            ("dashboard", "王小明（儀表板）"),
            ("telegram:tap", "王小明（Telegram，直接核准）"),
            ("googlechat:web", "王小明（Google Chat，網頁確認）"),
            ("telegram:totp", "王小明（Telegram，驗證碼確認）"),
        ] {
            let marker = if suffix == "dashboard" { format!("dashboard:{user}") }
                else { format!("chat-admin:{user}:{suffix}") };
            let name_of = |id: &str| (id == user).then(|| "王小明".to_owned());
            let display = decider_display(&marker, name_of, Lang::ZhTw).unwrap();
            assert_eq!(display, expected);
            assert!(!display.contains(user));
            assert_eq!(json!(viewer_decider_display(Some(&marker), false, |_| {
                panic!("invisible attribution must not resolve a name")
            }, Lang::ZhTw)), Value::Null);
        }
        for (lang, name, suffix, expected) in [
            (Lang::En, "Wang", "dashboard", "Wang (dashboard)"),
            (Lang::En, "Wang", "telegram:web", "Wang (Telegram, web confirmation)"),
            (Lang::Ja, "王小明", "dashboard", "王小明（ダッシュボード）"),
            (Lang::Ja, "王小明", "telegram:tap", "王小明（Telegram、ワンタップ）"),
            (Lang::Ja, "王小明", "telegram:web", "王小明（Telegram、Web 確認）"),
            (Lang::Ja, "王小明", "googlechat:totp", "王小明（Google Chat、コード確認）"),
        ] {
            let marker = if suffix == "dashboard" { format!("dashboard:{user}") }
                else { format!("chat-admin:{user}:{suffix}") };
            let display = decider_display(&marker, |_| Some(name.into()), lang).unwrap();
            assert_eq!(display, expected);
            assert!(!display.contains(user));
        }
    }

    #[test]
    fn decision_attribution_rejects_invalid_markers_and_unknown_users() {
        for marker in ["dashboard:", "dashboard:a:b", "chat-admin:bad:telegram:tap",
            "channel:telegram:123", "requester:someone",
            "chat-admin:14300000-0000-4000-8000-000000000022:telegram:unknown",
            "chat-admin:14300000-0000-4000-8000-000000000022:unknown:tap",
            "chat-admin:14300000-0000-4000-8000-000000000022:telegram:tap:extra"] {
            assert_eq!(decider_display(marker, |_| {
                panic!("invalid markers must not resolve a name")
            }, Lang::ZhTw), None);
        }
        for marker in ["dashboard:missing",
            "chat-admin:14300000-0000-4000-8000-000000000022:telegram:tap"] {
            assert_eq!(decider_display(marker, |_| None, Lang::ZhTw), None);
        }
        assert_eq!(viewer_decider_display(None, true, |_| Some("王小明".into()), Lang::ZhTw), None);
    }

    #[test]
    fn decision_attribution_sanitizes_and_caps_display_names() {
        assert_eq!(decider_display("dashboard:known", |_| Some("王\n小\u{202e}明".into()), Lang::ZhTw),
            Some("王小明（儀表板）".into()));
        assert_eq!(decider_display("dashboard:known", |_| Some("王".repeat(81)), Lang::ZhTw),
            Some(format!("{}（儀表板）", "王".repeat(80))));
        assert_eq!(decider_display("dashboard:known", |_| Some("\n\u{202e}".into()), Lang::ZhTw), None);
    }

    #[tokio::test]
    async fn list_digest_matches_decide_and_mismatch_leaves_pending() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "mcp_call", "reviewed", json!({"name": "send_message"}), 300)
            .await.unwrap();
        let listed = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &root).await);
        let digest = listed["approvals"][0]["digest"].as_str().unwrap().to_owned();
        let bad = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(!ok(&bad));
        assert!(matches!(bad, WsFrame::Response { error: Some(error), .. }
            if error.as_str().is_some_and(|s| s.contains("digest mismatch"))));
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        assert!(ok(&f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": digest}), &root,
        ).await));
    }

    #[tokio::test]
    async fn malformed_digest_is_rejected_and_absence_preserves_legacy_decision() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "mcp_call", "reviewed", json!({}), 300).await.unwrap();
        for malformed in [json!(null), json!(42), json!("bad")] {
            assert!(!ok(&f.handler.handle_approvals_decide(
                json!({"id": id.as_str(), "approve": false, "expected_digest": malformed}), &root,
            ).await));
        }
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        assert!(ok(&f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": false}), &root,
        ).await));
    }

    #[tokio::test]
    async fn get_requires_an_id_and_returns_the_same_digest_as_list() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "mcp_call", "reviewed", json!({}), 300).await.unwrap();
        let list = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &root).await);
        let get = payload(f.handler.dispatch_org(
            "approvals.get", json!({"id": id.as_str()}), &root,
            crate::power_local::RpcConnInfo::internal(),
        ).await);
        assert_eq!(get["approvals"][0]["digest"], list["approvals"][0]["digest"]);
        for response in [&list, &get] {
            let card = &response["approvals"][0];
            assert!(card.get("decided_by").is_none());
            assert_eq!(card.get("decided_by_display"), Some(&Value::Null));
        }
        assert!(!ok(&f.handler.dispatch_org(
            "approvals.get", json!({}), &root,
            crate::power_local::RpcConnInfo::internal(),
        ).await));
    }

    #[tokio::test]
    async fn hidden_purchase_digest_guesses_are_identically_refused() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "mcp_call", "purchase", json!({
            "action_grant": {"effect": "purchase"}, "args": {"amount": 123}
        }), 300).await.unwrap();
        let listed = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &root).await);
        let correct = listed["approvals"][0]["digest"].as_str().unwrap();
        for digest in [correct.to_owned(), "0".repeat(64)] {
            let result = f.handler.handle_approvals_decide(
                json!({"id": id.as_str(), "approve": false, "expected_digest": digest}), &f.alice,
            ).await;
            assert!(matches!(result, WsFrame::Response { ok: false, error: Some(error), .. }
                if error == json!("expected_digest is not accepted for this approval")));
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        }
        let bad_admin = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": false, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(matches!(bad_admin, WsFrame::Response { error: Some(error), .. }
            if error.as_str().is_some_and(|s| s.contains("digest mismatch"))));
        assert!(ok(&f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": false}), &f.alice,
        ).await));
    }

    #[tokio::test]
    async fn expired_hidden_purchase_never_compares_guesses() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "mcp_call", "purchase", json!({
            "action_grant": {"effect": "purchase"}, "args": {"amount": 123}
        }), 300).await.unwrap();
        rusqlite::Connection::open(f.home.join("approvals.db")).unwrap().execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1", [id.as_str()],
        ).unwrap();
        let rec = broker.get(&id).await.unwrap().unwrap();
        for digest in [crate::approval::decision_digest(&rec), "f".repeat(64)] {
            let result = f.handler.handle_approvals_decide(
                json!({"id": id.as_str(), "approve": true, "expected_digest": digest}), &f.alice,
            ).await;
            assert!(matches!(result, WsFrame::Response { error: Some(error), .. }
                if error == json!("expected_digest is not accepted for this approval")));
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        }
        let expired_admin = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(matches!(expired_admin, WsFrame::Response { error: Some(error), .. }
            if error == json!("approval expired")));
        // Reject a revoked identity before expiry or digest checks.
        f.db.set_user_status(&root.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
        let result = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(matches!(result, WsFrame::Response { error: Some(error), .. }
            if error == json!(PERMISSION_DENIED)));
    }

    #[tokio::test]
    async fn dashboard_expiry_precedes_digest_mismatch() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "responsibility_operator_change", "operator", json!({}), 300).await.unwrap();
        rusqlite::Connection::open(f.home.join("approvals.db")).unwrap().execute(
            "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1", [id.as_str()],
        ).unwrap();
        let result = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(matches!(result, WsFrame::Response { error: Some(error), .. }
            if error == json!(crate::approval_notify::dashboard_only_expired_text("responsibility_operator_change"))));
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Expired);
    }

    #[tokio::test]
    async fn redacted_unbound_purchase_has_no_digest_for_manager() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let full_payload = json!({
            "action_grant": {"effect": "purchase"},
            "args": {"recipient": "private@example.com", "amount": 123}
        });
        let id = broker.request("sales", "mcp_call", "purchase", full_payload.clone(), 300)
            .await.unwrap();
        for method in ["approvals.list", "approvals.get"] {
            let manager = payload(f.handler.dispatch_org(
                method, json!({"id": id.as_str()}), &f.alice,
                crate::power_local::RpcConnInfo::internal(),
            ).await);
            assert_eq!(manager["count"], 1);
            assert_eq!(manager["approvals"][0]["may_decide"], true);
            assert_eq!(manager["approvals"][0]["payload"], json!({"redacted": true}));
            assert_eq!(manager["approvals"][0]["digest"], Value::Null);
            let admin = payload(f.handler.dispatch_org(
                method, json!({"id": id.as_str()}), &root,
                crate::power_local::RpcConnInfo::internal(),
            ).await);
            assert_eq!(admin["approvals"][0]["payload"], full_payload);
            assert!(admin["approvals"][0]["digest"].is_string());
        }
    }

    #[tokio::test]
    async fn dashboard_only_approval_cards_are_admin_only() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "responsibility_operator_change", "operator request", json!({}), 300)
            .await.unwrap();
        let manager = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &f.alice).await);
        assert_eq!(manager["count"], 0);
        let admin = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &root).await);
        assert_eq!(admin["count"], 1);
        assert!(admin["approvals"][0]["digest"].is_string());
    }
}

/// An unbound digest must never become an oracle for redacted payload fields.
pub(super) fn digest_hidden_by_redaction(rec: &crate::approval::ApprovalRecord, admin: bool) -> bool {
    rec.binding.is_none() && ((crate::approval::is_outbound_data_card(rec) && !admin)
        || dashboard_action_payload(&rec.payload, admin) != rec.payload)
}

/// Only disclose a digest when the viewer can see every field it hashes.
fn viewer_digest(
    rec: &crate::approval::ApprovalRecord,
    activation_facts: &Value,
    admin: bool,
    may_decide: bool,
) -> Value {
    if !may_decide
        || digest_hidden_by_redaction(rec, admin)
    {
        Value::Null
    } else {
        json!(crate::approval::decision_digest_with_activation(rec, activation_facts))
    }
}

fn dashboard_action_payload(payload: &Value, admin: bool) -> Value {
    let outbound = matches!(payload.get("action_grant").and_then(|g| g.get("effect")).and_then(Value::as_str),
            Some("send" | "publish" | "purchase"));
    if outbound && !admin { json!({"redacted": true}) } else { payload.clone() }
}

#[cfg(test)]
mod egress_payload_tests {
    use super::*;

    #[test]
    fn full_outbound_arguments_are_admin_only() {
        for marker in [json!({"action_grant": {"effect": "send"}}),
            json!({"action_grant": {"effect": "publish"}}),
            json!({"action_grant": {"effect": "purchase"}})] {
            let mut payload = marker;
            payload["args"] = json!({"content": "complete private content", "to": "recipient@example.com"});
            assert_eq!(dashboard_action_payload(&payload, true), payload);
            assert_eq!(dashboard_action_payload(&payload, false), json!({"redacted": true}));
        }
    }
}

#[cfg(test)]
mod decided_list_tests {
    use super::*;
    use crate::handlers::task_privacy_tests::{fixture, admin, payload};

    #[tokio::test]
    async fn decided_list_is_bounded_filtered_and_live_admin_only() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let old = broker.request("sales", "mcp_call", "old", json!({}), 300).await.unwrap();
        let newest = broker.request("sales", "mcp_call", "new", json!({}), 300).await.unwrap();
        let other = broker.request("sales", "bus_task", "other", json!({}), 300).await.unwrap();
        let pending = broker.request("sales", "mcp_call", "pending", json!({}), 300).await.unwrap();
        let conn = rusqlite::Connection::open(f.home.join("approvals.db")).unwrap();
        for (id, time, status) in [(&old, "2026-01-01T00:00:00Z", "denied"),
            (&newest, "2026-01-02T00:00:00Z", "expired"),
            (&other, "2026-01-03T00:00:00Z", "approved")] {
            conn.execute("UPDATE approvals SET status=?1, decided_at=?2 WHERE id=?3",
                rusqlite::params![status, time, id.as_str()]).unwrap();
        }
        let params = json!({"status":"decided", "limit":1, "action_kind":"mcp_call", "agent_id":"sales"});
        let view = payload(f.handler.handle_approvals_list(params.clone(), &root).await);
        assert_eq!(view["count"], 1);
        assert_eq!(view["approvals"][0]["id"], newest.as_str());
        assert_eq!(view["approvals"][0]["decided_at"], "2026-01-02T00:00:00Z");
        assert_eq!(view["approvals"][0]["execution_result"], Value::Null);
        let all = payload(f.handler.handle_approvals_list(json!({"status":"decided"}), &root).await);
        assert_eq!(all["count"], 3);
        assert_eq!(all["approvals"][0]["id"], other.as_str());
        let pending_view = payload(f.handler.handle_approvals_list(json!({}), &root).await);
        assert_eq!(pending_view["count"], 1);
        assert_eq!(pending_view["approvals"][0]["id"], pending.as_str());
        for role in [UserRole::Manager, UserRole::Employee] {
            f.db.update_user(&f.alice.user_id, None, Some(role), None).unwrap();
            let ctx = f.db.live_user_context(&f.alice.user_id).unwrap().unwrap();
            let frame = f.handler.handle_approvals_list(params.clone(), &ctx).await;
            assert!(matches!(frame, WsFrame::Response { ok:false, error:Some(e), payload:None, .. }
                if e == json!("permission denied")));
        }
        f.db.update_user(&root.user_id, None, Some(UserRole::Manager), None).unwrap();
        assert!(matches!(f.handler.handle_approvals_list(params, &root).await,
            WsFrame::Response { ok:false, error:Some(e), .. } if e == json!("permission denied")));
    }

    #[tokio::test]
    async fn approvals_list_rejects_invalid_status_and_limit() {
        let f = fixture().await;
        let root = admin(&f);
        for params in [json!({"status":"approved"}), json!({"status":null}), json!({"status":1}),
            json!({"limit":0}), json!({"limit":101}), json!({"limit":-1}),
            json!({"limit":1.5}), json!({"limit":"1"}), json!({"limit":null})] {
            assert!(matches!(f.handler.handle_approvals_list(params, &root).await,
                WsFrame::Response { ok:false, payload:None, .. }));
        }
        for limit in [1, 100] {
            assert!(matches!(f.handler.handle_approvals_list(json!({"status":"decided", "limit":limit}), &root).await,
                WsFrame::Response { ok:true, .. }));
        }
    }

    #[tokio::test]
    async fn pending_list_is_unbounded_by_default_and_honors_explicit_limit() {
        let f = fixture().await;
        let root = admin(&f);
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        for index in 0..51 {
            broker.request("sales", "mcp_call", &format!("pending-{index}"), json!({}), 300)
                .await.unwrap();
        }

        let all = payload(f.handler.handle_approvals_list(json!({}), &root).await);
        assert_eq!(all["count"], 51);
        assert_eq!(all["approvals"].as_array().unwrap().len(), 51);

        let limited = payload(f.handler.handle_approvals_list(json!({"limit":2}), &root).await);
        assert_eq!(limited["count"], 2);
        assert_eq!(limited["approvals"].as_array().unwrap().len(), 2);
    }
}
