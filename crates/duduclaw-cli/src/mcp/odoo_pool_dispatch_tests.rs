//! Validates the routing seam between MCP dispatch and the
//! [`crate::odoo_pool::OdooConnectorPool`]: classification, permission
//! checks, and pool-key isolation. Actual HTTP round-trips to Odoo are
//! not exercised here — that is covered by `duduclaw-odoo` connector
//! tests against a live or mocked server.

use super::*;
use std::sync::Arc;

#[test]
fn classify_maps_search_class_tools() {
    for (tool, expected_model) in &[
        ("odoo_crm_leads", "crm.lead"),
        ("odoo_sale_orders", "sale.order"),
        ("odoo_inventory_products", "product.product"),
        ("odoo_inventory_check", "stock.quant"),
        ("odoo_invoice_list", "account.move"),
        ("odoo_payment_status", "account.move"),
    ] {
        let (verb, model) = classify_odoo_call(tool, &serde_json::json!({})).unwrap();
        assert_eq!(verb, "search", "tool={tool}");
        assert_eq!(model, *expected_model, "tool={tool}");
    }
}

#[test]
fn classify_maps_create_class_tools() {
    for (tool, expected_model) in &[
        ("odoo_crm_create_lead", "crm.lead"),
        ("odoo_sale_create_quotation", "sale.order"),
    ] {
        let (verb, model) = classify_odoo_call(tool, &serde_json::json!({})).unwrap();
        assert_eq!(verb, "create", "tool={tool}");
        assert_eq!(model, *expected_model);
    }
}

#[test]
fn classify_maps_write_class_tools() {
    let (verb, model) =
        classify_odoo_call("odoo_crm_update_stage", &serde_json::json!({})).unwrap();
    assert_eq!(verb, "write");
    assert_eq!(model, "crm.lead");
}

#[test]
fn classify_maps_execute_class_tools() {
    let (verb, model) =
        classify_odoo_call("odoo_sale_confirm", &serde_json::json!({})).unwrap();
    assert_eq!(verb, "execute");
    assert_eq!(model, "sale.order");
}

#[test]
fn classify_extracts_model_from_params_for_generic_search() {
    let (verb, model) = classify_odoo_call(
        "odoo_search",
        &serde_json::json!({ "model": "res.partner" }),
    )
    .unwrap();
    assert_eq!(verb, "search");
    assert_eq!(model, "res.partner");
}

#[test]
fn classify_returns_none_for_generic_search_without_model() {
    // No model arg → can't classify. The downstream handler will reject
    // with "model is required" — same v1.10.1 behaviour.
    assert!(classify_odoo_call("odoo_search", &serde_json::json!({})).is_none());
}

#[test]
fn classify_extracts_model_from_params_for_generic_execute() {
    // An unrecognised RPC method falls back to the generic `execute` verb.
    let (verb, model) = classify_odoo_call(
        "odoo_execute",
        &serde_json::json!({ "model": "res.partner", "method": "some_custom_rpc" }),
    )
    .unwrap();
    assert_eq!(verb, "execute");
    assert_eq!(model, "res.partner");
}

#[test]
fn classify_odoo_execute_derives_verb_from_method() {
    // HS8: the verb must reflect the actual ORM method, not a hard-coded
    // "execute", so the per-agent allowed_actions filter can block writes
    // that arrive through the generic odoo_execute tool.
    let cases = [
        ("search", "read"),
        ("search_read", "read"),
        ("read", "read"),
        ("create", "create"),
        ("write", "write"),
        ("unlink", "unlink"),
        ("action_archive", "action"),
        ("button_confirm", "action"),
        ("name_get", "read"),
        ("custom_method", "execute"),
    ];
    for (method, want) in cases {
        let (verb, model) = classify_odoo_call(
            "odoo_execute",
            &serde_json::json!({ "model": "crm.lead", "method": method }),
        )
        .unwrap();
        assert_eq!(verb, want, "method {method} should classify as {want}");
        assert_eq!(model, "crm.lead");
    }
}

#[test]
fn classify_odoo_execute_write_blocked_when_only_execute_allowed() {
    // Regression for HS8: allowed_actions=["read","search","execute"] must
    // NOT permit a method:"write" call routed through odoo_execute.
    let (verb, model) = classify_odoo_call(
        "odoo_execute",
        &serde_json::json!({ "model": "crm.lead", "method": "write" }),
    )
    .unwrap();
    let cfg = duduclaw_odoo::AgentOdooConfig {
        allowed_actions: vec!["read".into(), "search".into(), "execute".into()],
        ..Default::default()
    };
    let res = crate::odoo_pool::check_action_permission(Some(&cfg), verb, &model);
    assert!(res.is_err(), "write must be denied, got verb={verb}");
}

#[test]
fn classify_returns_none_for_status_and_connect() {
    // These tools intentionally bypass model-permission gating —
    // odoo_status reports state, odoo_connect bootstraps the slot.
    assert!(classify_odoo_call("odoo_status", &serde_json::json!({})).is_none());
    assert!(classify_odoo_call("odoo_connect", &serde_json::json!({})).is_none());
}

#[test]
fn classify_returns_none_for_unknown_tool() {
    assert!(classify_odoo_call("odoo_blarg", &serde_json::json!({})).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_status_reports_not_connected_for_fresh_agent() {
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let result = handle_odoo_tool(
        "odoo_status",
        &serde_json::json!({}),
        std::path::Path::new("/tmp"),
        &pool,
        "agnes",
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("Odoo not connected"), "got: {text}");
    assert!(
        text.contains("agnes"),
        "should name the caller, got: {text}"
    );
    assert!(result["isError"].as_bool().unwrap_or(false));
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_tool_blocks_disallowed_model_before_any_network_call() {
    // Register an agent override that whitelists only crm.lead.
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    pool.register_agent(
        "agnes",
        duduclaw_odoo::AgentOdooConfig {
            profile: Some("test".into()),
            allowed_models: vec!["crm.lead".into()],
            ..Default::default()
        },
    )
    .await;

    // Attempt a sale.order search — must be rejected at the gate, no
    // get_or_connect HTTP call attempted.
    let result = handle_odoo_tool(
        "odoo_sale_orders",
        &serde_json::json!({}),
        std::path::Path::new("/tmp"),
        &pool,
        "agnes",
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(result["isError"].as_bool().unwrap_or(false));
    assert!(text.contains("permission denied"), "got: {text}");
    assert!(text.contains("allowed_models"), "got: {text}");
    // No connector slot should have been touched.
    assert!(!pool.is_connected("agnes").await);
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_tool_blocks_disallowed_action_verb() {
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    pool.register_agent(
        "agnes",
        duduclaw_odoo::AgentOdooConfig {
            profile: Some("readonly".into()),
            allowed_actions: vec!["read".into(), "search".into()],
            ..Default::default()
        },
    )
    .await;

    // Attempt a write — must be denied even though crm.lead is permitted
    // (no model whitelist set).
    let result = handle_odoo_tool(
        "odoo_crm_create_lead",
        &serde_json::json!({ "name": "test lead" }),
        std::path::Path::new("/tmp"),
        &pool,
        "agnes",
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(result["isError"].as_bool().unwrap_or(false));
    assert!(text.contains("permission denied"), "got: {text}");
    assert!(text.contains("allowed_actions"), "got: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_tool_without_override_falls_through_to_connection_check() {
    // No override → permission gate is permissive → handler proceeds to
    // get_or_connect, which fails with the "not connected" message
    // because no connect was issued.
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let result = handle_odoo_tool(
        "odoo_crm_leads",
        &serde_json::json!({}),
        std::path::Path::new("/tmp"),
        &pool,
        "agnes",
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(result["isError"].as_bool().unwrap_or(false));
    assert!(text.contains("not connected"), "got: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn two_agents_get_isolated_pool_slots() {
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    pool.register_agent(
        "alpha-pm",
        duduclaw_odoo::AgentOdooConfig {
            profile: Some("alpha".into()),
            ..Default::default()
        },
    )
    .await;
    pool.register_agent(
        "beta-pm",
        duduclaw_odoo::AgentOdooConfig {
            profile: Some("beta".into()),
            ..Default::default()
        },
    )
    .await;

    let alpha_key = pool.pool_key("alpha-pm").await;
    let beta_key = pool.pool_key("beta-pm").await;
    assert_ne!(alpha_key, beta_key);
    assert_eq!(alpha_key.1, "alpha");
    assert_eq!(beta_key.1, "beta");
}
