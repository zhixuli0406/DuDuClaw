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

pub const NATIVE_GOOGLE_TOOLS: &[&str] = &[
    "google_status",
    "gmail_search",
    "gmail_read",
    "gmail_create_draft",
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
    "slides_read",
];

pub fn native_google_scope(tool: &str) -> Option<NativeGoogleScope> {
    match tool {
        "google_status"
        | "gmail_search"
        | "gmail_read"
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
        | "calendar_create_event"
        | "sheets_append"
        | "gtasks_create"
        | "gtasks_complete"
        | "docs_append" => Some(NativeGoogleScope::Write),
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
        assert_eq!(unique.len(), 19);
        assert_eq!(
            native_google_scope("gtasks_complete"),
            Some(NativeGoogleScope::Write)
        );
        assert!(native_google_scope("google_admin_delete").is_none());
        assert!(native_google_scope("gmail_search ").is_none());
    }
}
