//! Admin-only dashboard credentials. Values never enter audit or RPC errors.
use super::*;
use crate::website_credentials::{self as credentials, Upsert};
use duduclaw_security::audit::{AuditEvent, Severity};

pub(super) fn audit(home: &Path, method: &str, actor: &str, id: Option<&str>, status: &str) {
    crate::security_autopilot::audit_and_emit(home, &AuditEvent::new(
        method, "website-credentials", Severity::Info,
        json!({"actor_user_id": actor, "credential_id": id, "status": status}),
    ));
}

impl MethodHandler {
    pub(crate) async fn handle_website_credentials(
        &self, method: &str, mut params: Value, ctx: &UserContext,
    ) -> WsFrame {
        // Re-read identity here as well: direct handler callers must not be
        // able to bypass the dispatcher or reuse a demoted connection.
        let live = crate::computer_use_sessions::live_ops::live_identity(&self.home_dir, ctx);
        let Some(live) = live.filter(|c| c.is_admin()) else {
            audit(&self.home_dir, method, &ctx.user_id, None, "refused");
            return WsFrame::error_response("", "只有管理員可以管理網站登入資料。");
        };
        let home = self.home_dir.clone();
        let actor = live.user_id;
        let method = method.to_string();
        // Move, rather than clone, the request into its zeroizing owner.
        // Never reflect serde's parse error: it can include rejected values.
        let result = match method.as_str() {
            "website_credentials.list" => {
                tokio::task::spawn_blocking(move || {
                    let result = credentials::list(&home).map(|records| json!({"credentials": records}));
                    audit(&home, &method, &actor, None, if result.is_ok() { "listed" } else { "failed" });
                    result
                }).await
            }
            "website_credentials.upsert" => {
                let input = match serde_json::from_value::<Upsert>(params.take()) {
                    Ok(input) => input,
                    Err(_) => {
                        audit(&home, &method, &actor, None, "refused");
                        return WsFrame::error_response("", "網站登入資料格式不正確。");
                    }
                };
                tokio::task::spawn_blocking(move || {
                    let requested_id = input.id.clone().filter(|id| uuid::Uuid::parse_str(id).is_ok());
                    let result = credentials::upsert(&home, input, &actor);
                    let id = result.as_ref().ok().map(|m| m.id.as_str()).or(requested_id.as_deref());
                    audit(&home, &method, &actor, id, if result.is_ok() { "saved" } else { "refused" });
                    result.map(|metadata| json!({"credential": metadata}))
                }).await
            }
            "website_credentials.delete" => {
                let id = params["id"].as_str().unwrap_or("").to_string();
                tokio::task::spawn_blocking(move || {
                    let result = credentials::delete(&home, &id);
                    audit(&home, &method, &actor,
                        uuid::Uuid::parse_str(&id).is_ok().then_some(id.as_str()),
                        match &result { Ok(true) => "deleted", Ok(false) => "noop", Err(_) => "refused" });
                    result.map(|deleted| json!({"deleted": deleted}))
                }).await
            }
            _ => return WsFrame::error_response("", "不支援此網站登入資料操作。"),
        };
        match result {
            Ok(Ok(body)) => WsFrame::ok_response("", body),
            Ok(Err(message)) => WsFrame::error_response("", &message),
            Err(_) => WsFrame::error_response("", "網站登入資料操作中斷，請重新整理。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(frame: WsFrame) -> Value {
        match frame {
            WsFrame::Response { ok: true, payload: Some(body), .. } => body,
            _ => panic!("expected successful credential RPC"),
        }
    }

    #[tokio::test]
    async fn admin_round_trip_and_audit_never_return_secret_values() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        let admin = UserContext::admin_fallback();
        let saved = payload(handler.handle("website_credentials.upsert", json!({
            "label":"work", "login_url":"https://example.com/login", "username":"audit-user-canary",
            "password":"audit-password-canary", "allowed_agents":["alice"],
        }), &admin).await);
        assert!(!saved.to_string().contains("audit-password-canary"));
        let listed = payload(handler.handle("website_credentials.list", json!({}), &admin).await);
        assert!(listed["credentials"][0].get("password").is_none());
        let id = saved["credential"]["id"].as_str().unwrap();
        let deleted = payload(handler.handle("website_credentials.delete", json!({"id":id}), &admin).await);
        assert_eq!(deleted["deleted"], true);
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        for value in ["audit-password-canary", "audit-user-canary", "https://example.com"] {
            assert!(!audit.contains(value));
        }
        for event in ["website_credentials.list", "website_credentials.upsert", "website_credentials.delete"] {
            assert!(audit.contains(event));
        }
    }

    fn credential_audits(home: &Path) -> Vec<Value> {
        std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap()
            .lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["event_type"].as_str().is_some_and(|method| method.starts_with("website_credentials.")))
            .collect()
    }

    #[tokio::test]
    async fn outer_acl_refusals_are_audited_without_request_values() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        let mut employee = UserContext::admin_fallback();
        employee.role = UserRole::Employee;
        let methods = ["website_credentials.list", "website_credentials.upsert", "website_credentials.delete"];
        for method in methods {
            let params = json!({"id":"id-canary", "label":"label-canary", "login_url":"https://canary.example/login",
                "username":"username-canary", "password":"password-canary", "allowed_agents":["agent-canary"]});
            assert!(matches!(handler.handle(method, params, &employee).await,
                WsFrame::Response { ok: false, .. }));
        }
        let rows = credential_audits(home.path());
        assert_eq!(rows.len(), methods.len());
        for (row, method) in rows.iter().zip(methods) {
            assert_eq!(row["event_type"], method);
            assert_eq!(row["details"], json!({"actor_user_id":employee.user_id,
                "credential_id":null, "status":"refused"}));
            assert!(!row.to_string().contains("canary"));
        }
        assert!(!credentials::path(home.path()).exists());
    }

    #[tokio::test]
    async fn delete_audit_distinguishes_deleted_noop_and_refused() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        let admin = UserContext::admin_fallback();
        let saved = payload(handler.handle("website_credentials.upsert", json!({
            "label":"work", "login_url":"https://example.com/login", "username":"person",
            "password":"secret", "allowed_agents":["alice"],
        }), &admin).await);
        let id = saved["credential"]["id"].as_str().unwrap();
        assert_eq!(payload(handler.handle("website_credentials.delete", json!({"id":id}), &admin).await)["deleted"], true);
        assert_eq!(payload(handler.handle("website_credentials.delete", json!({"id":id}), &admin).await)["deleted"], false);
        assert!(matches!(handler.handle("website_credentials.delete", json!({"id":"invalid"}), &admin).await,
            WsFrame::Response { ok: false, .. }));
        let statuses: Vec<_> = credential_audits(home.path()).into_iter()
            .filter(|row| row["event_type"] == "website_credentials.delete")
            .map(|row| row["details"]["status"].as_str().unwrap().to_string()).collect();
        assert_eq!(statuses, ["deleted", "noop", "refused"]);
    }

    #[tokio::test]
    async fn direct_handler_refuses_non_admins() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        let mut employee = UserContext::admin_fallback();
        employee.role = UserRole::Employee;
        for method in ["website_credentials.list", "website_credentials.upsert", "website_credentials.delete"] {
            assert!(matches!(handler.handle_website_credentials(method, json!({}), &employee).await,
                WsFrame::Response { ok: false, .. }));
        }
        assert!(!credentials::path(home.path()).exists());
    }
}
