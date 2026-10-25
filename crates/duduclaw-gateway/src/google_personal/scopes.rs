//! A partial consent can serve only the operations it actually covers.

fn has(granted: &[String], candidates: &[&str]) -> bool {
    granted
        .iter()
        .any(|scope| candidates.contains(&scope.as_str()))
}

pub fn permitted(tool: &str, granted: &[String]) -> bool {
    match tool {
        "google_status" => true,
        "gmail_search" | "gmail_read" | "mail_poll" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/gmail.readonly",
                "https://www.googleapis.com/auth/gmail.modify",
                "https://mail.google.com/",
            ],
        ),
        "gmail_create_draft" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/gmail.compose",
                "https://www.googleapis.com/auth/gmail.modify",
                "https://mail.google.com/",
            ],
        ),
        "calendar_list_events" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/calendar.events",
                "https://www.googleapis.com/auth/calendar.events.readonly",
                "https://www.googleapis.com/auth/calendar",
            ],
        ),
        "calendar_create_event" => has(
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
        "sheets_append" => has(granted, &["https://www.googleapis.com/auth/spreadsheets"]),
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
                "https://www.googleapis.com/auth/drive",
            ],
        ),
        "docs_read" => has(
            granted,
            &[
                "https://www.googleapis.com/auth/documents",
                "https://www.googleapis.com/auth/documents.readonly",
            ],
        ),
        "docs_append" => has(granted, &["https://www.googleapis.com/auth/documents"]),
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
