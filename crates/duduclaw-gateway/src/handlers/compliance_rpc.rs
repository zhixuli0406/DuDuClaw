//! Admin-only evidence export; the resolver is shared with all edition gates.
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_compliance_generate(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let edition = self.resolve_edition_profile().await;
        if let Err(error) = crate::compliance::authorize(&self.home_dir, ctx, edition) {
            return WsFrame::error_response("", &error);
        }
        let request = match serde_json::from_value::<crate::compliance::Request>(params) {
            Ok(request) => request,
            Err(_) => return WsFrame::error_response("", "COMPLIANCE_INVALID_REQUEST"),
        };
        // Bound expensive generation across concurrent dashboard connections.
        static RUNNING: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
        let permit = match RUNNING.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))).clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return WsFrame::error_response("", "COMPLIANCE_BUSY"),
        };
        let home = self.home_dir.clone();
        let live = ctx.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::compliance::generate(&home, &live, edition, &request)
        }).await;
        // A license change during generation must not release the report.
        let edition = self.resolve_edition_profile().await;
        if let Err(error) = crate::compliance::authorize(&self.home_dir, ctx, edition) {
            return WsFrame::error_response("", &error);
        }
        match result {
            Ok(Ok(output)) => WsFrame::ok_response("", json!(output)),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", "COMPLIANCE_UNAVAILABLE"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct ComplianceClaimingExtension {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl GatewayExtension for ComplianceClaimingExtension {
        fn rpc_methods(&self) -> Vec<String> { vec!["ext.duduclaw.compliance_probe".into()] }
        async fn handle_method(&self, method: &str, _params: Value, _ctx: &UserContext) -> Option<WsFrame> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match method {
                "compliance.generate" | "ext.duduclaw.compliance_probe" => {
                    Some(WsFrame::ok_response("", json!({"extension": true})))
                }
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn compliance_rpc_reserves_admin_gate_before_extension_dispatch() {
        let home = tempfile::tempdir().unwrap();
        let extension = Arc::new(ComplianceClaimingExtension::default());
        let handler = MethodHandler::with_extension(home.path().to_path_buf(), extension.clone()).await;
        handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;

        for role in [UserRole::Manager, UserRole::Employee] {
            let mut ctx = UserContext::admin_fallback();
            ctx.role = role;
            let expected = acl::require_role(&ctx, UserRole::Admin).unwrap_err();
            let frame = handler.handle("compliance.generate", json!({}), &ctx).await;
            match frame {
                WsFrame::Response { ok: false, error: Some(error), .. } => assert_eq!(error, expected),
                other => panic!("expected Admin gate refusal for {role:?}: {other:?}"),
            }
            assert_eq!(extension.calls.load(Ordering::SeqCst), 0);
        }
        assert!(!home.path().join("security_audit.jsonl").exists());

        // Prove the extension is wired in and still handles unreserved methods.
        let frame = handler.handle("ext.duduclaw.compliance_probe", json!({}), &UserContext::admin_fallback()).await;
        assert!(matches!(frame, WsFrame::Response { ok: true, payload: Some(payload), .. } if payload == json!({"extension": true})));
        assert_eq!(extension.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn compliance_rpc_refuses_non_admin_before_reading_sources() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        for role in [UserRole::Manager, UserRole::Employee] {
            let mut ctx = UserContext::admin_fallback();
            ctx.role = role;
            let frame = handler.dispatch_system("compliance.generate", json!({}), &ctx,
                crate::power_local::RpcConnInfo::internal()).await;
            assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
        }
        assert!(!home.path().join("security_audit.jsonl").exists());
    }
}
