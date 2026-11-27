//! Closed native Google tool vocabulary shared by dispatch and host execution.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeGoogleScope {
    Read,
    Write,
}

impl NativeGoogleScope {
    pub const fn grant(self) -> &'static str {
        match self {
            Self::Read => "google:read",
            Self::Write => "google:write",
        }
    }
}

pub const DRIVE_WRITE_TOOLS: &[&str] = &[
    "drive_create_file",
    "drive_copy_file",
    "drive_update_file",
    "docs_insert_text",
    "docs_format",
    "sheets_add_sheet",
    "sheets_add_note",
    "drive_move_file",
    "drive_trash_file",
];

/// Exact membership in the gateway-only Drive write vocabulary.
pub fn is_drive_write_tool(tool: &str) -> bool {
    DRIVE_WRITE_TOOLS.contains(&tool)
}

pub const NATIVE_GOOGLE_TOOLS: &[&str] = &[
    "google_status",
    "gmail_search",
    "gmail_read",
    "google_chat_spaces",
    "google_chat_messages",
    "google_chat_send",
    "gmail_create_draft",
    "gmail_send_draft",
    "calendar_delete_event",
    "calendar_list_events",
    "calendar_create_event",
    "sheets_read",
    "sheets_append",
    "forms_get",
    "forms_list_responses",
    "gtasks_lists",
    "gtasks_list",
    "gtasks_create",
    "gtasks_complete",
    "drive_search",
    "drive_read",
    "docs_read",
    "docs_append",
    "docs_create",
    "docs_replace_text",
    "sheets_create",
    "sheets_update",
    "calendar_update_event",
    "slides_read",
    "drive_create_file",
    "drive_copy_file",
    "drive_update_file",
    "drive_move_file",
    "drive_trash_file",
    "docs_insert_text",
    "docs_format",
    "sheets_add_sheet",
    "sheets_add_note",
];

pub fn native_google_scope(tool: &str) -> Option<NativeGoogleScope> {
    if is_drive_write_tool(tool) { return Some(NativeGoogleScope::Write); }
    match tool {
        "google_status"
        | "gmail_search"
        | "gmail_read"
        | "google_chat_spaces"
        | "google_chat_messages"
        | "calendar_list_events"
        | "sheets_read"
        | "forms_get"
        | "forms_list_responses"
        | "gtasks_lists"
        | "gtasks_list"
        | "drive_search"
        | "drive_read"
        | "docs_read"
        | "slides_read" => Some(NativeGoogleScope::Read),
        "gmail_create_draft"
        | "gmail_send_draft"
        | "calendar_delete_event"
        | "google_chat_send"
        | "calendar_create_event"
        | "sheets_append"
        | "gtasks_create"
        | "gtasks_complete"
        | "docs_append"
        | "docs_create"
        | "docs_replace_text"
        | "sheets_create"
        | "sheets_update"
        | "calendar_update_event" => Some(NativeGoogleScope::Write),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_native_tool_has_one_scope_and_unknown_names_never_inherit_one() {
        let mut unique = std::collections::HashSet::new();
        for tool in NATIVE_GOOGLE_TOOLS {
            assert!(unique.insert(tool));
            assert!(native_google_scope(tool).is_some(), "{tool}");
        }
        assert_eq!(unique.len(), 38);
        assert_eq!(
            native_google_scope("gtasks_complete"),
            Some(NativeGoogleScope::Write)
        );
        assert!(native_google_scope("google_admin_delete").is_none());
        assert!(native_google_scope("gmail_search ").is_none());
    }
}

#[cfg(test)]
mod drive_write_tests {
    use super::*;
    #[test]
    fn drive_write_vocabulary_is_exact_and_write_scoped() {
        assert_eq!(DRIVE_WRITE_TOOLS.len(), 9);
        for tool in DRIVE_WRITE_TOOLS {
            assert!(NATIVE_GOOGLE_TOOLS.contains(tool));
            assert_eq!(native_google_scope(tool), Some(NativeGoogleScope::Write));
            assert!(!is_drive_write_tool(&format!("{tool} ")));
            assert!(!is_drive_write_tool(&format!("x{tool}")));
        }
    }
}
