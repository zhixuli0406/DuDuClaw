//! Deterministic, host-owned egress decision shared by the dispatcher and proxy.
#[cfg(test)]
use serde_json::json;
use serde_json::Value;

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
