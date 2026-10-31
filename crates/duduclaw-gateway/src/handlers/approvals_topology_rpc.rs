//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Autopilot handlers ──────────────────────────────────

    /// WP14-T14.7: list pending approvals for the approval center. Optional
    /// `agent_id` filter. Read-only; opens approvals.db per call.
    pub(crate) async fn handle_approvals_list(&self, params: Value, ctx:&UserContext) -> WsFrame {
        let agent_filter = params.get("agent_id").and_then(|v| v.as_str());
        // Optional exact `action_kind` filter (e.g. "knowledge_quarantine" for the
        // D6 curation queue). Matches the raw stored action_kind string.
        let action_kind_filter = params
            .get("action_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
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
        } else {
            broker.list_pending(agent_filter).await
        };
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
        match rows {
            Ok(rows) => {
                let mut items: Vec<Value> = Vec::with_capacity(rows.len());
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
                    // Binding data and question answers go only to a viewer
                    // who may decide this card.
                    let may_decide = self.authorize_egress_decider(r, &fresh).is_ok()
                        && (r.binding.is_none()
                            || Box::pin(self.authorize_bound_decider(r, &fresh)).await.is_ok());
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
                    let activation_facts = if activation && may_decide {
                        super::approval_decider::activation_facts(&self.home_dir, r)
                    } else {
                        Value::Null
                    };
                    items.push(json!({
                        "id": r.id.as_str(),
                        "digest": viewer_digest(r, &activation_facts, fresh.is_admin(), may_decide),
                        "agent_id": r.agent_id,
                        "kind": kind.as_str(),
                        "summary": r.summary,
                        "payload": if r.binding.is_some()
                            && !may_show_bound_egress(&r.payload, fresh.is_admin(), may_decide) {json!({
                            "redacted": true,
                            "payload_hash": r.binding.as_ref().map(|b|&b.payload_hash)
                        })}else{dashboard_action_payload(&r.payload, fresh.is_admin())},
                        "request_kind":r.request_kind,
                        "status":r.status,
                        "binding": if may_decide { json!(r.binding) } else { Value::Null },
                        "answer": if may_decide { json!(r.answer) } else { Value::Null },
                        "may_decide": may_decide,
                        "decided_in_dashboard_only": r.payload.get("egress_guard").is_some() || crate::approval_notify::is_dashboard_only_kind(&r.action_kind),
                        "submitter_is_viewer": activation
                            && super::approval_decider::submitter_is_decider(r, &fresh),
                        // A-H-1: what an Admin accepts — each effect and the
                        // one record it may change.
                        "workflow_activation": activation_facts,
                        "invalidated_reason":r.invalidated_reason,
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
                        "simulation": r.simulation,
                        // E8: platform name (safe to expose — no internal id)
                        // + a fully-resolved "open in channel" URL, or `null`
                        // when nothing could be constructed.
                        "channel": r.notify_channel,
                        "channel_link": channel_link,
                    }));
                }
                WsFrame::ok_response("", json!({ "approvals": items, "count": items.len() }))
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

#[cfg(test)]
mod live_event_tests {
    use super::*;
    use crate::handlers::task_privacy_tests::{admin, fixture, ok, payload};

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
        // Fresh-context failure also refuses without comparing a candidate.
        f.db.set_user_status(&root.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
        let result = f.handler.handle_approvals_decide(
            json!({"id": id.as_str(), "approve": true, "expected_digest": "0".repeat(64)}), &root,
        ).await;
        assert!(matches!(result, WsFrame::Response { error: Some(error), .. }
            if error == json!("expected_digest is not accepted for this approval")));
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

/// Full outbound content is only delivered to the dashboard administrator.
fn may_show_bound_egress(payload: &Value, admin: bool, may_decide: bool) -> bool {
    admin && may_decide && payload.get("egress_guard").is_some()
}

/// An unbound digest must never become an oracle for redacted payload fields.
pub(super) fn digest_hidden_by_redaction(rec: &crate::approval::ApprovalRecord, admin: bool) -> bool {
    rec.binding.is_none() && dashboard_action_payload(&rec.payload, admin) != rec.payload
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
    let outbound = payload.get("egress_guard").is_some()
        || matches!(payload.get("action_grant").and_then(|g| g.get("effect")).and_then(Value::as_str),
            Some("send" | "publish" | "purchase"));
    if outbound && !admin { json!({"redacted": true}) } else { payload.clone() }
}

#[cfg(test)]
mod egress_payload_tests {
    use super::*;

    #[test]
    fn bound_egress_requires_fresh_decision_authority() {
        let payload = json!({"egress_guard": {"sources": ["memory_read"]}});
        assert!(may_show_bound_egress(&payload, true, true));
        assert!(!may_show_bound_egress(&payload, true, false));
        assert!(!may_show_bound_egress(&payload, false, true));
        assert!(!may_show_bound_egress(&json!({}), true, true));
    }

    #[test]
    fn full_outbound_arguments_are_admin_only() {
        for marker in [json!({"egress_guard": {"sources": ["memory_read"]}}),
            json!({"action_grant": {"effect": "send"}}),
            json!({"action_grant": {"effect": "publish"}}),
            json!({"action_grant": {"effect": "purchase"}})] {
            let mut payload = marker;
            payload["args"] = json!({"content": "complete private content", "to": "recipient@example.com"});
            assert_eq!(dashboard_action_payload(&payload, true), payload);
            assert_eq!(dashboard_action_payload(&payload, false), json!({"redacted": true}));
        }
    }
}
