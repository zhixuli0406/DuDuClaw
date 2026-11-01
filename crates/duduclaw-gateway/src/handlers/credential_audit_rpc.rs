use super::*;

impl MethodHandler {
    pub(crate) async fn handle_credentials_audit(&self, method: &str, params: Value) -> WsFrame {
        let home = self.home_dir.clone();
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            match method.as_str() {
                "credentials.audit_list" => crate::credential_ledger::list(&home, &params),
                "credentials.audit_get" | "credentials.audit_update" => {
                    let id = params.get("id").and_then(Value::as_str).ok_or("credential id required")?;
                    if method == "credentials.audit_update" {
                        crate::credential_ledger::update(&home, id, &params)?;
                    }
                    crate::credential_ledger::get(&home, id)
                }
                _ => Err("unknown credential method".into()),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", "憑證稽核暫時無法使用"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn audit_methods_require_admin_before_opening_ledger() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        for role in [UserRole::Employee, UserRole::Manager] {
            let mut caller = UserContext::admin_fallback();
            caller.role = role;
            for method in ["credentials.audit_list", "credentials.audit_get", "credentials.audit_update", "security.audit_verify"] {
                let frame = Box::pin(handler.handle(method, json!({"id":"cred_test","export":true}), &caller)).await;
                assert!(matches!(frame, WsFrame::Response { ok:false, .. }), "{method}");
            }
        }
        assert!(!home.path().join("credentials/ledger.db").exists());
    }
}

#[cfg(test)]
mod admin_routing_tests {
    use super::*;

    #[tokio::test]
    async fn administrator_can_reach_credential_list_and_export_routes() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let admin = UserContext::admin_fallback();
        let frame = Box::pin(handler.handle("credentials.audit_list", json!({"export":true}), &admin)).await;
        match frame {
            WsFrame::Response { ok:true, payload:Some(value), .. } => {
                assert_eq!(value["total"], 0);
                assert_eq!(value["mode"], "audit");
            }
            other => panic!("expected credential list, got {other:?}"),
        }
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("credential_exported"));
    }
}
