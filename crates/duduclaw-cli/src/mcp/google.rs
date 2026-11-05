use super::*;

pub(crate) use duduclaw_gateway::google_identity::native_tools::{
    arg_u32, arg_u64, handle_calendar_create_event, handle_calendar_list_events,
    handle_docs_append, handle_docs_read, handle_drive_read, handle_drive_search, handle_forms_get,
    handle_forms_list_responses, handle_gmail_create_draft, handle_gmail_read, handle_gmail_search,
    handle_gtasks_complete, handle_gtasks_create, handle_gtasks_list, handle_gtasks_lists,
    handle_sheets_append, handle_sheets_read, handle_slides_read,
};

pub(crate) async fn handle_google_status(
    home_dir: &Path,
    g: &super::google_identity_client::GoogleCaller,
) -> Value {
    // Status never loads token/client secrets or calls resolve_backend: even
    // the Home path must not decrypt, mint, refresh or contact Google.
    let (header, describes_home) = super::google_identity_client::status_header(home_dir, g).await;
    let mut out = header;
    if describes_home {
        let enabled = duduclaw_gateway::google_workspace::integration_enabled(home_dir);
        out.push_str(if enabled {
            "狀態：Google 整合已啟用。本次只讀取本機設定，未取得權杖或連線測試 Google；請到控制台查看連線詳情。"
        } else {
            "狀態：Google 整合尚未啟用。本次只讀取本機設定。"
        });
    }
    tool_text(out.trim_end())
}
