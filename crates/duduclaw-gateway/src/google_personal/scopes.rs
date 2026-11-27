//! A partial consent can serve only the operations it actually covers.

/// The bearer broker accepts only these operations; status is metadata-only.
pub const TOKEN_TOOLS: &[&str] = &[
    "google_chat_spaces", "google_chat_messages", "google_chat_send",
    "gmail_search", "gmail_read", "mail_poll", "gmail_create_draft", "gmail_send_draft", "calendar_delete_event",
    "calendar_list_events", "calendar_create_event", "sheets_read", "sheets_append",
    "forms_get", "forms_list_responses", "gtasks_lists", "gtasks_list",
    "gtasks_create", "gtasks_complete", "drive_search", "drive_read",
    "docs_read", "docs_append", "docs_create", "docs_replace_text", "sheets_create", "sheets_update", "calendar_update_event", "slides_read", "google_remote_mcp",
];

pub fn token_tool(tool: &str) -> bool {
    TOKEN_TOOLS.contains(&tool)
}

/// Expose only scopes that have a corresponding local purpose grant.
pub fn visible_scopes(metadata: &crate::person_credentials::Metadata) -> Vec<String> {
    metadata.granted_scopes.iter().filter(|scope| {
        let mut single = metadata.clone();
        single.granted_scopes = vec![(*scope).clone()];
        TOKEN_TOOLS.iter().any(|tool| permitted_for(tool, &single))
            || (metadata.local_purposes.iter().any(|p| p == "meeting_recordings")
                && matches!(scope.as_str(),
                    "https://www.googleapis.com/auth/drive.file"
                    | "https://www.googleapis.com/auth/meetings.space.readonly"))
    }).cloned().collect()
}

pub fn permitted_for(tool: &str, metadata: &crate::person_credentials::Metadata) -> bool {
    if !token_tool(tool) { return false; }
    let consent = |purpose: &str| metadata.local_purposes.iter().any(|p| p == purpose);
    let local = match tool {
        "google_chat_spaces" | "google_chat_messages" => consent("chat_read"),
        "google_chat_send" => consent("chat_send"),
        "drive_search" | "drive_read" => consent("drive_tools"),
        "google_remote_mcp" => consent("base") && consent("drive_tools"),
        _ => consent("base"),
    };
    local && permitted(tool, &metadata.granted_scopes)
}

fn has(granted: &[String], candidates: &[&str]) -> bool {
    granted
        .iter()
        .any(|scope| candidates.contains(&scope.as_str()))
}

pub fn permitted(tool: &str, granted: &[String]) -> bool {
    if duduclaw_core::google_identity::is_drive_write_tool(tool) { return false; }
    match tool {
        "google_chat_send" => has(granted, &["https://www.googleapis.com/auth/chat.messages.create", "https://www.googleapis.com/auth/chat.messages"]),
        "google_chat_spaces" => has(granted, &["https://www.googleapis.com/auth/chat.spaces.readonly"]),
        "google_chat_messages" => has(granted, &["https://www.googleapis.com/auth/chat.messages.readonly"]),
        "gmail_search" | "gmail_read" | "mail_poll" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/gmail.readonly",
                "https://www.googleapis.com/auth/gmail.modify",
                "https://mail.google.com/",
            ],
        ),
        "gmail_create_draft" | "gmail_send_draft" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/gmail.compose",
                "https://www.googleapis.com/auth/gmail.modify",
                "https://mail.google.com/",
            ],
        ),
        "calendar_delete_event" => has(granted, &[
            "https://www.googleapis.com/auth/calendar.events", "https://www.googleapis.com/auth/calendar",
        ]) && has(granted, &[
            "https://www.googleapis.com/auth/calendar.calendarlist.readonly", "https://www.googleapis.com/auth/calendar.calendarlist",
            "https://www.googleapis.com/auth/calendar.readonly", "https://www.googleapis.com/auth/calendar",
        ]),
        "calendar_list_events" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/calendar.events",
                "https://www.googleapis.com/auth/calendar.events.readonly",
                "https://www.googleapis.com/auth/calendar",
            ],
        ),
        "calendar_create_event" | "calendar_update_event" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/calendar.events",
                "https://www.googleapis.com/auth/calendar",
            ],
        ),
        "sheets_read" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/spreadsheets",
                "https://www.googleapis.com/auth/spreadsheets.readonly",
            ],
        ),
        "sheets_append" | "sheets_create" | "sheets_update" => has(granted, &["https://www.googleapis.com/auth/spreadsheets"]),
        "forms_get" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/forms.body.readonly",
                "https://www.googleapis.com/auth/forms.body",
            ],
        ),
        "forms_list_responses" => has(
            granted,
            &["https://www.googleapis.com/auth/forms.responses.readonly"],
        ),
        "gtasks_lists" | "gtasks_list" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/tasks",
                "https://www.googleapis.com/auth/tasks.readonly",
            ],
        ),
        "gtasks_create" | "gtasks_complete" => {
            has(granted, &["https://www.googleapis.com/auth/tasks"])
        }
        "drive_search" | "drive_read" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/drive.readonly",
                super::oauth::DRIVE_WRITE_SCOPE,
            ],
        ),
        "docs_read" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/documents",
                "https://www.googleapis.com/auth/documents.readonly",
            ],
        ),
        "docs_append" | "docs_create" | "docs_replace_text" => has(granted, &["https://www.googleapis.com/auth/documents"]),
        "slides_read" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/presentations.readonly",
                "https://www.googleapis.com/auth/presentations",
            ],
        ),
        "google_remote_mcp" => crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .all(|scope| granted.iter().any(|s| s == scope)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_grant_does_not_authorize_a_write_or_an_unknown_operation() {
        let scopes = vec!["https://www.googleapis.com/auth/spreadsheets.readonly".into()];
        assert!(permitted("sheets_read", &scopes));
        assert!(!permitted("sheets_append", &scopes));
        assert!(!permitted("gmail_search", &scopes));
        assert!(!permitted("unknown_google_tool", &scopes));
        assert!(!permitted("google_remote_mcp", &scopes));
    }
}

#[cfg(test)]
mod native_write_scope_tests {
    use super::*;
    #[test]
    fn every_new_write_requires_its_existing_service_scope() {
        for (tool, service) in [
            ("docs_create", "documents"), ("docs_replace_text", "documents"),
            ("sheets_create", "spreadsheets"), ("sheets_update", "spreadsheets"),
            ("calendar_update_event", "calendar.events"), ("gmail_create_draft", "gmail.compose"),
        ] {
            let scope = format!("https://www.googleapis.com/auth/{service}");
            assert!(token_tool(tool), "{tool}");
            assert!(permitted(tool, &[scope]), "{tool}");
            assert!(!permitted(tool, &[]), "{tool}");
            assert!(!permitted(tool, &[format!("https://www.googleapis.com/auth/{service}.readonly")]), "{tool}");
        }
        for tool in duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS {
            if duduclaw_core::google_identity::is_drive_write_tool(tool) {
                assert!(!token_tool(tool));
                assert!(!permitted(tool, &[crate::google_personal::oauth::DRIVE_WRITE_SCOPE.to_owned()]));
            } else if *tool != "google_status" {
                assert!(token_tool(tool), "missing personal scope table entry: {tool}");
                assert!(permitted(tool, &crate::google_workspace::REQUIRED_SCOPES.iter()
                    .chain(crate::google_workspace::CHAT_READ_SCOPES.iter()).chain(crate::google_workspace::CHAT_SEND_SCOPES.iter()).map(|s| s.to_string()).collect::<Vec<_>>()), "{tool}");
            }
        }
    }
}
