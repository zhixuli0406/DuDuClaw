//! Closed service scope groups. Unknown operations never receive a broad token.
use std::path::Path;

pub(crate) const SERVICES: &[(&str, &[&str])] = &[
    ("gmail", &["https://www.googleapis.com/auth/gmail.readonly", "https://www.googleapis.com/auth/gmail.compose"]),
    ("calendar", &["https://www.googleapis.com/auth/calendar.events", "https://www.googleapis.com/auth/calendar.calendarlist.readonly"]),
    ("docs", &["https://www.googleapis.com/auth/documents"]),
    ("sheets", &["https://www.googleapis.com/auth/spreadsheets"]),
    ("drive", &["https://www.googleapis.com/auth/drive.readonly"]),
    ("slides", &["https://www.googleapis.com/auth/presentations.readonly"]),
    ("forms", &["https://www.googleapis.com/auth/forms.body.readonly", "https://www.googleapis.com/auth/forms.responses.readonly"]),
    ("tasks", &["https://www.googleapis.com/auth/tasks"]),
    ("chat", crate::google_workspace::CHAT_READ_SCOPES),
];

pub(crate) fn for_service(home: &Path, service: &str) -> Result<&'static [&'static str], super::Refusal> {
    if service != "chat" || crate::google_workspace::chat_read_enabled(home) {
        if let Some((_, scopes)) = SERVICES.iter().find(|(name, _)| *name == service) {
            return Ok(scopes);
        }
    }
    Err(super::Refusal::hard("scope_missing", "這項 Google 服務尚未開放，或無法確認所需權限。"))
}

pub(crate) fn for_tool(home: &Path, tool: &str) -> Result<&'static [&'static str], super::Refusal> {
    if tool == "google_chat_send" {
        return if crate::google_workspace::chat_send_enabled(home) { Ok(crate::google_workspace::CHAT_SEND_SCOPES) }
            else { Err(super::Refusal::hard("scope_missing", crate::google_workspace::chat_send::GUIDANCE)) };
    }
    let service = match tool {
        "gmail_search" | "gmail_read" | "gmail_create_draft" | "mail_poll" => "gmail",
        "calendar_list_events" | "calendar_create_event" | "calendar_update_event" => "calendar",
        "docs_read" | "docs_append" | "docs_create" | "docs_replace_text" => "docs",
        "sheets_read" | "sheets_append" | "sheets_create" | "sheets_update" => "sheets",
        "drive_search" | "drive_read" => "drive",
        "slides_read" => "slides",
        "forms_get" | "forms_list_responses" => "forms",
        "gtasks_lists" | "gtasks_list" | "gtasks_create" | "gtasks_complete" => "tasks",
        "google_chat_spaces" | "google_chat_messages" => "chat",
        _ => "",
    };
    for_service(home, service)
}

pub(crate) fn for_endpoint(home: &Path, endpoint: &str) -> Result<&'static [&'static str], super::Refusal> {
    let service = match endpoint {
        "https://gmailmcp.googleapis.com/mcp/v1" => "gmail",
        "https://calendarmcp.googleapis.com/mcp/v1" => "calendar",
        "https://docsmcp.googleapis.com/mcp/v1" => "docs",
        "https://sheetsmcp.googleapis.com/mcp/v1" => "sheets",
        "https://drivemcp.googleapis.com/mcp/v1" => "drive",
        "https://slidesmcp.googleapis.com/mcp/v1" => "slides",
        "https://chatmcp.googleapis.com/mcp/v1" => "chat",
        _ => "",
    };
    for_service(home, service)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_send_scope_is_independent_and_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        for config in ["", "[google]\nchat_read=true", "[google]\nchat_send='true'", "[broken"] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            assert!(for_tool(home.path(), "google_chat_send").is_err());
        }
        std::fs::write(home.path().join("config.toml"), "[google]\nchat_send=true").unwrap();
        assert_eq!(for_tool(home.path(), "google_chat_send").unwrap(), crate::google_workspace::CHAT_SEND_SCOPES);
        assert!(for_tool(home.path(), "google_chat_messages").is_err());
        assert!(!for_tool(home.path(), "gmail_read").unwrap().contains(&crate::google_workspace::CHAT_SEND_SCOPES[0]));
    }
    #[test]
    fn scopes_are_per_service_and_chat_is_opt_in() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(for_tool(home.path(), "gmail_read").unwrap(), SERVICES[0].1);
        assert!(for_tool(home.path(), "google_chat_messages").is_err());
        std::fs::write(home.path().join("config.toml"), "[google]\nchat_read = true\n").unwrap();
        assert_eq!(for_tool(home.path(), "google_chat_messages").unwrap(), crate::google_workspace::CHAT_READ_SCOPES);
        assert!(for_tool(home.path(), "unknown").is_err());
    }
}
