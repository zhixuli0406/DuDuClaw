use super::*;

#[tokio::test]
async fn removed_branding_and_partner_sales_methods_are_unavailable_to_admins() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for method in [
        "branding.get",
        "branding.set",
        "branding.reset",
        "branding.preview",
        "branding.bundle.create",
        "distributor.bundle.sign",
        "partner.profile",
        "partner.stats",
        "partner.customers",
        "partner.profile.update",
        "partner.customer.add",
        "partner.customer.update",
        "partner.customer.delete",
    ] {
        let frame = handler
            .dispatch_ops(
                method,
                json!({"product_name": "Other Brand"}),
                &UserContext::admin_fallback(),
                crate::power_local::RpcConnInfo::internal(),
            )
            .await;
        match frame {
            WsFrame::Response {
                error: Some(error), ..
            } => assert!(
                error.to_string().contains("Unknown method"),
                "{method}: {error}"
            ),
            other => panic!("removed method {method} must fail: {other:?}"),
        }
    }
    assert!(!home.path().join("branding.json").exists());
    assert!(!home.path().join("partner.db").exists());
}

#[tokio::test]
async fn about_uses_fixed_product_metadata_without_local_brand_overrides() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("branding.json"),
        r#"{"product_name":"Other Brand","website":"https://direct-sales.example"}"#,
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("about.get", json!({}), &UserContext::admin_fallback())
        .await;
    match frame {
        WsFrame::Response {
            payload: Some(result),
            error: None,
            ..
        } => {
            assert_eq!(result["product_name"], "DuDuClaw");
            assert_eq!(result["vendor"]["name_zh"], "嘟嘟數位科技有限公司");
            assert!(result.get("version").is_some());
            assert!(result.get("tier").is_some());
            for removed in [
                "branding",
                "source",
                "white_label_active",
                "editable_fields",
            ] {
                assert!(result.get(removed).is_none(), "{removed} must be absent");
            }
        }
        other => panic!("about must remain readable: {other:?}"),
    }
}

#[tokio::test]
async fn retired_partner_redemption_rpc_is_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "license.redeem",
            json!({"code": "PARTNER-TEST"}),
            &UserContext::admin_fallback(),
        )
        .await;
    match frame {
        WsFrame::Response {
            error: Some(error), ..
        } => {
            assert!(error.to_string().contains("Unknown method"), "{error}");
        }
        other => panic!("retired redemption RPC must fail: {other:?}"),
    }
    assert!(!home.path().join("license.json").exists());
}
