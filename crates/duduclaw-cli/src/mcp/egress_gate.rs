//! Deterministic, host-owned egress decision shared by the dispatcher and proxy.
#[cfg(test)]
use serde_json::json;
use serde_json::Value;

#[cfg(test)]
pub(crate) use duduclaw_gateway::egress_gate::check;

pub(crate) fn model_visible_payload(response: &Value) -> bool {
    fn carries_data(value: &Value) -> bool {
        ["data", "content", "structuredContent"].iter().any(|key| value.get(key).is_some())
    }
    response.get("result").is_some_and(|result| result.is_object()
        && (result.get("isError").is_none() || result.get("isError") == Some(&Value::Bool(false))
            || carries_data(result)))
        || response.get("error").is_some_and(carries_data)
}

/// Positive, exact-name attestation of the built-in handler's completion
/// contract. These handlers map typed transport/local failures to isError=true.
/// Local queue/file tools attest durable admission, not later employee work.
/// Never infer completion from arbitrary content or a generic success shape:
/// search aggregators and background launchers can hide unfinished effects.
pub(crate) fn confirmed_sink_completion(tool: &str, response: &Value) -> bool {
    let terminal_contract = matches!(tool,
        "send_message" | "send_photo" | "send_sticker" | "web_search"
        | "web_fetch_cached" | "web_extract"
        | "notion_search" | "notion_page_read" | "notion_page_append"
        | "github_search_issues" | "github_issue_read" | "github_pr_read" | "github_issue_comment"
        | "odoo_crm_create_lead" | "odoo_crm_leads" | "odoo_crm_update_stage"
        | "odoo_execute" | "odoo_inventory_check" | "odoo_inventory_products"
        | "odoo_invoice_list" | "odoo_partner_search" | "odoo_payment_status" | "odoo_report"
        | "odoo_sale_confirm" | "odoo_sale_create_quotation" | "odoo_sale_orders"
        | "odoo_schema_fields" | "odoo_search"
        | "transcribe_audio" | "synthesize_speech" | "db_query" | "db_select" | "db_tables"
        | "calendar_create_event" | "calendar_list_events" | "docs_append" | "docs_read"
        | "drive_read" | "drive_search" | "forms_get" | "forms_list_responses"
        | "gmail_create_draft" | "gmail_read" | "gmail_search" | "gtasks_complete"
        | "gtasks_create" | "gtasks_list" | "sheets_append" | "sheets_read" | "slides_read"
        | "mail_send" | "wiki_write" | "wiki_share" | "shared_skill_share"
        | "tasks_create" | "update_cron_task" | "create_reminder"
        | "send_to_agent" | "spawn_agent" | "team_handoff" | "responsibility_ask"
        | "skill_hub_install" | "knowledge_add_source" | "computer_navigate" | "computer_type" | "os_notify"
    );
    terminal_contract && response.get("error").is_none()
        && response.get("result").is_some_and(|result| {
            result.is_object() && (result.get("isError").is_none()
                || result.get("isError") == Some(&Value::Bool(false)))
        })
}

/// A rejected/prepared call has not entered its private-data handler.
pub(crate) async fn cancel_before_dispatch(
    read: Option<duduclaw_gateway::egress_client::PrivateRead>, response: Value,
) -> Value {
    if let Some(read) = read { let _ = read.finish(false).await; }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn egress_error_payloads_taint_but_payloadless_failures_do_not() {
        for response in [
            json!({"result":{"content":[]}}),
            json!({"result":{"isError":true,"content":[{"text":"secret"}]}}),
            json!({"result":{"isError":true,"structuredContent":{"secret":42}}}),
            json!({"error":{"code":-1,"data":{"secret":42}}}),
        ] { assert!(model_visible_payload(&response)); }
        for response in [json!({"error":{"code":-1}}), json!({"result":{"isError":true}}), json!({"result":null})] {
            assert!(!model_visible_payload(&response));
        }
    }
}

/// A host-owned immediate return before network or durable queue admission.
pub(crate) fn pre_dispatch_error(message: &str) -> Value {
    duduclaw_gateway::egress_client::builtin_not_dispatched();
    super::tool_error(message)
}

#[cfg(test)]
mod dispatch_observation_tests {
    use super::*;
    use duduclaw_gateway::egress_client::observe_builtin_dispatch;

    #[tokio::test]
    async fn egress_builtin_local_failures_attest_not_dispatched() {
        let home = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        for channel in ["telegram", "unknown"] {
            let args = json!({"channel":channel,"chat_id":"test","text":"hello"});
            let (response, local) = observe_builtin_dispatch(
                crate::mcp::messaging::handle_send_message(&args, home.path(), &http, "bot")
            ).await;
            assert_eq!(response["isError"], true, "{response}");
            assert!(local, "{channel}: {response}");
        }
        for channel in ["telegram", "discord", "unknown"] {
            let args = json!({"channel":channel,"chat_id":"test","url":"https://example.com/photo"});
            let (response, local) = observe_builtin_dispatch(
                crate::mcp::messaging::handle_send_media(&args, home.path(), &http, "photo")
            ).await;
            assert_eq!(response["isError"], true);
            assert!(local, "{channel}: {response}");
        }
        let odoo = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::new(Default::default()));
        let (response, local) = observe_builtin_dispatch(
            crate::mcp::odoo::handle_odoo_tool("odoo_invoice_list", &json!({}), home.path(), &odoo, "bot")
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local, "{response}");
        let (response, local) = observe_builtin_dispatch(
            crate::mcp_db::handle_db_tables(&json!({"source":"unknown"}), home.path(), "bot")
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local);
        let (response, local) = observe_builtin_dispatch(
            crate::mcp::spawn::handle_spawn_agent(&json!({"agent_id":"unknown","task":"hello"}), home.path(), "bot")
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local, "unknown employee: {response}");
        let (response, local) = observe_builtin_dispatch(
            crate::mcp::delegation::handle_send_to_agent(&json!({}), home.path(), "bot")
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local);
        for args in [json!({}), json!({"owner":"owner"}), json!({"owner":"owner","repo":"repo"})] {
            let (response, local) = observe_builtin_dispatch(
                crate::mcp::notion_github::handle_github_issue_comment(&args, home.path())
            ).await;
            assert_eq!(response["isError"], true);
            assert!(local, "GitHub validation: {response}");
        }
        for provider in ["notion", "github"] {
            let args = json!({"query":"test"});
            let (response, local) = observe_builtin_dispatch(async {
                if provider == "notion" {
                    crate::mcp::notion_github::handle_notion_search(&args, home.path()).await
                } else {
                    crate::mcp::notion_github::handle_github_search_issues(&args, home.path()).await
                }
            }).await;
            assert_eq!(response["isError"], true);
            assert!(local, "missing connection: {response}");
        }
        let google = crate::mcp::google_identity_client::GoogleCaller::default();
        let (response, local) = observe_builtin_dispatch(
            crate::mcp::google::handle_calendar_create_event(&json!({}), home.path(), &google)
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local, "Google validation: {response}");
        let (response, local) = observe_builtin_dispatch(
            crate::mcp::google::handle_drive_search(&json!({"query":"test"}), home.path(), &google)
        ).await;
        assert_eq!(response["isError"], true);
        assert!(local, "Google missing connection: {response}");
        let (_, local) = observe_builtin_dispatch(async { json!({"isError":true,"not_dispatched":true}) }).await;
        assert!(!local, "response fields cannot attest dispatch state");
        let (_, local) = observe_builtin_dispatch(async {
            duduclaw_gateway::egress_client::builtin_dispatch_started();
            let _ = tokio::time::timeout(std::time::Duration::from_millis(1), std::future::pending::<()>()).await;
            json!({"isError":true,"content":[{"text":"request timed out"}]})
        }).await;
        assert!(!local, "post-dispatch timeout must stay uncertain");
        let (_, local) = observe_builtin_dispatch(async {
            duduclaw_gateway::egress_client::builtin_dispatch_started();
            pre_dispatch_error("local resolution failed after a request started")
        }).await;
        assert!(!local, "nested local failure cannot downgrade a started request");
    }

    #[test]
    fn egress_credential_declare_is_local_metadata() {
        assert_eq!(duduclaw_core::tool_effect::effect_of_builtin("credential_declare"), Some(duduclaw_core::ToolEffect::Modify));
        assert_eq!(duduclaw_core::tool_effect::egress_capable("credential_declare"), Some(false));
        assert_eq!(duduclaw_core::tool_effect::reads_private("credential_declare"), Some(false));
    }
}
