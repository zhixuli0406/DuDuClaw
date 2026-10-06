//! Operator review of accepted channel events. No payloads or reply tokens returned.
//! The RPCs share the gateway's one store per home (no migration on every
//! call; review I-MEDIUM-5) and every resolution writes a security audit row.
use super::*;

fn ingress_store(home: &std::path::Path) -> Result<std::sync::Arc<crate::channel_ingress::IngressStore>, String> {
    crate::channel_ingress::IngressStore::shared(home)
}
impl MethodHandler {
    pub(crate) async fn handle_channel_ingress_inspect(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::approval::require_current_dashboard_role_in_home(
            &self.home_dir,
            ctx,
            UserRole::Admin,
        ) {
            return WsFrame::error_response("", &e);
        }
        let Some(id) = params["ingress_id"]
            .as_str()
            .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return WsFrame::error_response("", "有效的 ingress_id required");
        };
        for key in ["before_attempt", "before_authorization"] {
            if !params[key].is_null() && !params[key].as_i64().is_some_and(|n| n > 0) {
                return WsFrame::error_response("", "有效的 pagination cursor required");
            }
        }
        let store = match ingress_store(&self.home_dir) {
            Ok(s) => s,
            Err(_) => return WsFrame::error_response("", "收件紀錄無法讀取"),
        };
        match store
            .inspect(
                id,
                params["before_attempt"].as_i64(),
                params["before_authorization"].as_i64(),
            )
            .await
        {
            Ok(Some(mut result)) => {
                // Two databases cannot commit the human decision and transport
                // receipt atomically. Reconcile only the fixed request/context;
                // never replay the decision or infer tool completion from it.
                if let Some(raw) = result["event"]["decision_binding"].as_str() {
                    if let Ok(binding) = serde_json::from_str::<Value>(raw) {
                        if let (Some(request), Some(expected)) = (
                            binding["request_id"].as_str(),
                            binding["context_hash"].as_str(),
                        ) {
                            if self.home_dir.join("approvals.db").exists() {
                                if let Ok(broker) =
                                    crate::approval::ApprovalBroker::open(&self.home_dir)
                                {
                                    match broker
                                        .get(&crate::approval::ApprovalId::from(request.to_owned()))
                                        .await
                                    {
                                        Ok(Some(record)) => {
                                            if let Some(bound) = record.binding {
                                                let c = bound.decision_context;
                                                if crate::channel_ingress::digest(&[
                                                    &c.channel,
                                                    &c.account_id,
                                                    &c.conversation_id,
                                                    &c.principal_id,
                                                ]) == expected
                                                {
                                                    result["decision_receipt"] = json!({
                                                        "request_id": request,
                                                        "status": record.status.as_str(),
                                                        "decided_by": record.decided_by,
                                                        "decided_at": record.decided_at,
                                                        "transport_receipt_independent": true,
                                                        "action_result_independent": true
                                                    });
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            result["decision_receipt_lookup"] = json!("unavailable")
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                WsFrame::ok_response("", result)
            }
            Ok(None) => WsFrame::error_response("", "找不到這筆收件紀錄"),
            Err(_) => WsFrame::error_response("", "收件紀錄無法讀取"),
        }
    }
    pub(crate) async fn handle_channel_ingress_list(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::approval::require_current_dashboard_role_in_home(
            &self.home_dir,
            ctx,
            UserRole::Admin,
        ) {
            return WsFrame::error_response("", &e);
        }
        let store = match ingress_store(&self.home_dir) {
            Ok(s) => s,
            Err(_) => return WsFrame::error_response("", "收件紀錄無法讀取"),
        };
        let cfg = crate::channel_ingress::config::IngressConfig::load(&self.home_dir).await;
        let settings = json!({
            "line_late_reply": cfg.late_reply.as_str(),
            "line_workers": cfg.line_workers,
            "retention_days": cfg.retention_days,
            "stuck_alert_minutes": cfg.stuck_alert_minutes,
            "capacity_alert_mb": cfg.capacity_alert_mb,
        });
        match store.list_page(params["before_seq"].as_i64()).await {
            Ok(rows) => WsFrame::ok_response(
                "",
                json!({
                    "events": rows,
                    "summary": store.summary().await.ok(),
                    "database_bytes": std::fs::metadata(self.home_dir.join("channel_ingress.db"))
                        .map(|m| m.len())
                        .unwrap_or(0),
                    "wal_bytes": std::fs::metadata(self.home_dir.join("channel_ingress.db-wal"))
                        .map(|m| m.len())
                        .unwrap_or(0),
                    "line_enabled": crate::line::durable_line_enabled(&self.home_dir).await,
                    "settings": settings,
                }),
            ),
            Err(_) => WsFrame::error_response("", "收件紀錄無法讀取"),
        }
    }
    pub(crate) async fn handle_channel_ingress_resolve(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::approval::require_current_dashboard_role_in_home(
            &self.home_dir,
            ctx,
            UserRole::Admin,
        ) {
            return WsFrame::error_response("", &e);
        }
        let Some(id) = params["ingress_id"]
            .as_str()
            .filter(|id| crate::channel_ingress::cli_approval::valid_ingress_id(id))
        else {
            return WsFrame::error_response("", "有效的 ingress_id required");
        };
        let Some(revision) = params["expected_revision"].as_str() else {
            return WsFrame::error_response("", "expected_revision required");
        };
        let Some(expected_attempt) = params["expected_attempt"].as_i64().filter(|n| *n >= 0) else {
            return WsFrame::error_response("", "expected_attempt required");
        };
        let Some(action) = params["action"].as_str() else {
            return WsFrame::error_response("", "action required");
        };
        let Some(note) = params["note"].as_str() else {
            return WsFrame::error_response("", "note required");
        };
        let provider_receipt = match &params["provider_receipt"] {
            Value::Null => None,
            Value::String(s) => Some(s.as_str()),
            _ => return WsFrame::error_response("", "provider_receipt must be a string"),
        };
        let store = match ingress_store(&self.home_dir) {
            Ok(s) => s,
            Err(_) => return WsFrame::error_response("", "收件紀錄無法寫入"),
        };
        let confirm = params["confirm_duplicate_risk"].as_bool().unwrap_or(false);
        let result = store
            .resolve_request(&crate::channel_ingress::ResolveRequest {
                id,
                expected_revision: revision,
                expected_attempt,
                action,
                confirm_duplicate_risk: confirm,
                actor: &ctx.user_id,
                note,
                provider_receipt,
                now: chrono::Utc::now().timestamp(),
                late_reply: crate::channel_ingress::config::IngressConfig::load(&self.home_dir)
                    .await
                    .late_reply,
            })
            .await;
        let event = duduclaw_security::audit::AuditEvent::new(
            "channel_ingress_resolution",
            format!("dashboard:{}", ctx.user_id),
            if result.is_ok() {
                duduclaw_security::audit::Severity::Info
            } else {
                duduclaw_security::audit::Severity::Warning
            },
            json!({
                "ingress_id": id,
                "action": duduclaw_core::truncate_chars(action, 16),
                "confirm_duplicate_risk": confirm,
                "provider_receipt": provider_receipt.is_some(),
                "ok": result.is_ok(),
            }),
        );
        duduclaw_security::audit::append_audit_event(&self.home_dir, &event);
        match result {
            Ok(()) => WsFrame::ok_response("", json!({"ingress_id":id,"action":action})),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn succeeded(f: &WsFrame) -> bool {
        matches!(f, WsFrame::Response { ok: true, .. })
    }
    fn payload(frame: WsFrame) -> Value {
        match frame {
            WsFrame::Response {
                ok: true,
                payload: Some(value),
                ..
            } => value,
            other => panic!("expected successful inspect response: {other:?}"),
        }
    }
    #[tokio::test]
    async fn inspect_pages_event_history_older_than_global_summary_without_payload_or_cross_event_rows()
     {
        let dir = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(dir.path().to_path_buf()).await;
        let admin = UserContext::admin_fallback();
        let mut employee = admin.clone();
        employee.role = UserRole::Employee;
        let store = crate::channel_ingress::IngressStore::open(dir.path()).unwrap();
        for id in ["old-event", "new-event"] {
            store
                .append(
                    &[crate::channel_ingress::AcceptedEvent {
                        decision_fastlane: false,
                        decision_binding: None,
                        event_id: id.into(),
                        account: "account".into(),
                        revision: "r1".into(),
                        authorization_revision: "a1".into(),
                        conversation: id.into(),
                        payload: "secret-payload-marker".into(),
                    }],
                    10,
                )
                .await
                .unwrap();
        }
        let rows = store.list().await.unwrap();
        let old = rows.iter().find(|r| r.event_id == "old-event").unwrap();
        let new = rows.iter().find(|r| r.event_id == "new-event").unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("channel_ingress.db")).unwrap();
        for row in [old, new] {
            for ordinal in 1..=205 {
                let operation = format!("{}-operation-{ordinal}", row.id);
                let authorization = format!("{}-authorization-{ordinal}", row.id);
                conn.execute(
                    "INSERT INTO ingress_attempts VALUES (?1,?2,?3,'uncertain','fixture',?4,?5)",
                    rusqlite::params![
                        operation,
                        row.id,
                        ordinal,
                        if row.id == old.id { 10 } else { 20 },
                        authorization
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO ingress_retry_authorizations VALUES (?1,?2,?3,'operator','fixture',10)",
                    rusqlite::params![authorization, row.id, operation]
                )
                .unwrap();
            }
        }
        assert!(
            !store.summary().await.unwrap()["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["ingress_id"] == old.id)
        );
        let params = json!({"ingress_id":old.id});
        assert!(!succeeded(
            &handler
                .handle("channel_ingress.inspect", params.clone(), &employee)
                .await
        ));
        let first = payload(
            handler
                .handle("channel_ingress.inspect", params, &admin)
                .await,
        );
        assert_eq!(first["attempts"].as_array().unwrap().len(), 200);
        assert_eq!(first["retry_authorizations"].as_array().unwrap().len(), 200);
        assert!(!first.to_string().contains("secret-payload-marker"));
        assert!(
            first["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["operation_id"].as_str().unwrap().starts_with(&old.id))
        );
        let second=payload(handler.handle("channel_ingress.inspect",json!({
            "ingress_id": old.id,
            "before_attempt": first["next_before_attempt"],
            "before_authorization": first["next_before_authorization"]
        }),&admin).await);
        assert_eq!(second["attempts"].as_array().unwrap().len(), 5);
        assert_eq!(second["retry_authorizations"].as_array().unwrap().len(), 5);
        assert!(second["next_before_attempt"].is_null());
        assert!(second["next_before_authorization"].is_null());
        for params in [
            json!({"ingress_id":"client-selected-path"}),
            json!({"ingress_id":"f".repeat(64)}),
            json!({"ingress_id":old.id,"before_attempt":-1}),
        ] {
            assert!(!succeeded(
                &handler
                    .handle("channel_ingress.inspect", params, &admin)
                    .await
            ));
        }
    }
    #[tokio::test]
    async fn channel_ingress_rpc_is_admin_only_and_payload_never_returned() {
        let dir = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(dir.path().to_path_buf()).await;
        let admin = UserContext::admin_fallback();
        let mut employee = admin.clone();
        employee.role = UserRole::Employee;
        for method in [
            "channel_ingress.list",
            "channel_ingress.resolve",
            "channel_ingress.inspect",
        ] {
            assert!(!succeeded(
                &handler.handle(method, json!({}), &employee).await
            ));
        }
        let store = crate::channel_ingress::IngressStore::open(dir.path()).unwrap();
        store
            .append(
                &[crate::channel_ingress::AcceptedEvent {
                    decision_fastlane: false,
                    decision_binding: None,
                    event_id: "event".into(),
                    account: "account".into(),
                    revision: "r1".into(),
                    authorization_revision: "auth1".into(),
                    conversation: "chat".into(),
                    payload: "secret-token".into(),
                }],
                10,
            )
            .await
            .unwrap();
        let row = store.claim(10).await.unwrap().unwrap();
        store
            .transition(&row, "claimed", "quarantined", Some("changed_route"))
            .await
            .unwrap();
        let list = handler
            .handle("channel_ingress.list", json!({}), &admin)
            .await;
        assert!(succeeded(&list));
        assert!(
            !serde_json::to_string(&list)
                .unwrap()
                .contains("secret-token")
        );
        let params = json!({
            "ingress_id": row.id,
            "expected_revision": "r1",
            "expected_attempt": row.attempt,
            "action": "close",
            "note": "verified changed route"
        });
        assert!(!succeeded(
            &handler
                .handle("channel_ingress.resolve", params.clone(), &employee)
                .await
        ));
        assert!(succeeded(
            &handler
                .handle("channel_ingress.resolve", params, &admin)
                .await
        ));
        assert_eq!(store.list().await.unwrap()[0].status, "closed");
    }
}

#[cfg(test)]
mod fresh_authority_tests {
    use super::*;
    #[tokio::test]
    async fn cached_admin_context_cannot_inspect_or_retry_after_revocation_or_db_failure() {
        for revoke in ["role", "suspended", "corrupt"] {
            let home = tempfile::tempdir().unwrap();
            let handler = MethodHandler::new(home.path().to_path_buf()).await;
            let store = crate::channel_ingress::IngressStore::open(home.path()).unwrap();
            store
                .append(
                    &[crate::channel_ingress::AcceptedEvent {
                        decision_fastlane: false,
                        decision_binding: None,
                        event_id: "unknown".into(),
                        account: "account".into(),
                        revision: "route".into(),
                        authorization_revision: "authority".into(),
                        conversation: "chat".into(),
                        payload: "{}".into(),
                    }],
                    chrono::Utc::now().timestamp(),
                )
                .await
                .unwrap();
            let row = store
                .claim(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .unwrap();
            store
                .transition(&row, "claimed", "dispatching", None)
                .await
                .unwrap();
            store
                .transition(&row, "dispatching", "uncertain", Some("unknown"))
                .await
                .unwrap();
            let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
            let user = db
                .create_user(
                    "admin@example.test",
                    "Admin",
                    "fixture-password",
                    UserRole::Admin,
                )
                .unwrap();
            let mut cached = UserContext::admin_fallback();
            cached.user_id = user.id.clone();
            assert!(matches!(
                handler
                    .handle(
                        "channel_ingress.inspect",
                        json!({"ingress_id":row.id}),
                        &cached
                    )
                    .await,
                WsFrame::Response { ok: true, .. }
            ));
            let before = store.inspect(&row.id, None, None).await.unwrap();
            match revoke {
                "role" => db
                    .update_user(&user.id, None, Some(UserRole::Employee), None)
                    .unwrap(),
                "suspended" => db
                    .set_user_status(&user.id, duduclaw_auth::UserStatus::Suspended)
                    .unwrap(),
                _ => {}
            }
            drop(db);
            if revoke == "corrupt" {
                std::fs::write(home.path().join("users.db"), "corrupt identity store").unwrap();
            }
            for method in [
                "channel_ingress.list",
                "channel_ingress.inspect",
                "channel_ingress.resolve",
            ] {
                let params = json!({
                    "ingress_id": row.id,
                    "expected_revision": row.revision,
                    "expected_attempt": row.attempt,
                    "action": "retry",
                    "confirm_duplicate_risk": true,
                    "note": "must refuse"
                });
                assert!(
                    matches!(
                        handler.handle(method, params, &cached).await,
                        WsFrame::Response { ok: false, .. }
                    ),
                    "{method}/{revoke}"
                );
            }
            assert_eq!(
                store.inspect(&row.id, None, None).await.unwrap(),
                before,
                "rejection must not append auth/attempt or change source"
            );
        }
    }
    #[test]
    fn fresh_admin_role_does_not_create_approvals_store_and_preserves_explicit_solo_rule() {
        let home = tempfile::tempdir().unwrap();
        let system = UserContext::admin_fallback();
        assert!(
            crate::approval::require_current_dashboard_role_in_home(
                home.path(),
                &system,
                UserRole::Admin
            )
            .is_ok()
        );
        assert!(!home.path().join("approvals.db").exists());
        let mut fake = system.clone();
        fake.user_id = "claimed-admin".into();
        assert!(
            crate::approval::require_current_dashboard_role_in_home(
                home.path(),
                &fake,
                UserRole::Admin
            )
            .is_err()
        );
        assert!(!home.path().join("approvals.db").exists());
        std::fs::write(home.path().join("users.db"), "corrupt").unwrap();
        assert!(
            crate::approval::require_current_dashboard_role_in_home(
                home.path(),
                &system,
                UserRole::Admin
            )
            .is_err()
        );
    }
}
