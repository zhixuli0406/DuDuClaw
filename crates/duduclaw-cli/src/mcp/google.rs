use super::*;

/// Read a u32 argument that may arrive as a JSON number or string.
pub(crate) fn arg_u32(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .or_else(|| {
            args.get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .unwrap_or(default)
}

pub(crate) async fn handle_google_status(home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace::{
        GOOGLE_PROVIDER, GoogleBackend, REQUIRED_SCOPES, resolve_backend,
    };
    use duduclaw_gateway::mcp_oauth;

    let token = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GOOGLE_PROVIDER);
    let configured = mcp_oauth::has_client_config(home_dir, GOOGLE_PROVIDER);

    let mut out = String::new();

    // Lead with the credential source actually in effect. Three exist (OAuth
    // vault, service-account delegation, Apps Script bridge) and the OAuth
    // detail below only describes the first — without this line a customer on
    // delegation or the bridge reads "NOT connected" while their tools work.
    match resolve_backend(home_dir).await {
        Ok(GoogleBackend::Direct(_)) => {
            out.push_str("Credential source: direct API token (OAuth vault or service-account delegation). All 19 tools available.\n\n");
        }
        Ok(GoogleBackend::AppsScript(cfg)) => {
            out.push_str(&format!(
                "Credential source: {}. Gmail / Calendar / Sheets are available; Drive, Docs, Slides, Forms and Tasks need OAuth or a service account.\n\n",
                cfg.describe()
            ));
        }
        Err(e) => {
            out.push_str(&format!("Credential source: none usable — {e}\n\n"));
        }
    }
    match token {
        None => {
            out.push_str("Google Workspace: NOT connected.\n");
            if configured {
                out.push_str(
                    "Client credentials are set. Open the dashboard Integrations → Google page and click \"Connect Google\" to authorize.",
                );
            } else {
                out.push_str(
                    "No OAuth client is configured. Open the dashboard Integrations → Google page to set up your Google OAuth client, then connect.",
                );
            }
        }
        Some(t) => {
            let now = chrono::Utc::now();
            let (state, expiry) = match t.expires_at {
                Some(exp) if now >= exp => ("EXPIRED".to_string(), exp.to_rfc3339()),
                Some(exp) => ("valid".to_string(), exp.to_rfc3339()),
                None => ("valid (no expiry)".to_string(), "n/a".to_string()),
            };
            let has_refresh = t
                .refresh_token
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let auto_refresh = if has_refresh && configured {
                "available"
            } else {
                "unavailable — reconnect to enable automatic refresh"
            };
            out.push_str(&format!(
                "Google Workspace: connected.\nToken: {state} (expires: {expiry})\nAuto-refresh: {auto_refresh}\nGranted scopes:\n"
            ));
            for s in &t.scopes {
                out.push_str(&format!("  - {s}\n"));
            }
            let missing: Vec<&str> = REQUIRED_SCOPES
                .iter()
                .copied()
                .filter(|req| !t.scopes.iter().any(|s| s == req))
                .collect();
            if !missing.is_empty() {
                out.push_str(
                    "\nMissing scopes needed for full Gmail/Calendar functionality (reconnect from the dashboard to grant):\n",
                );
                for m in missing {
                    out.push_str(&format!("  - {m}\n"));
                }
            }
        }
    }
    tool_text(out.trim_end())
}

pub(crate) async fn handle_gmail_search(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let max = arg_u32(args, "max_results", 10);

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::gmail_search_via(&backend, query, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gmail_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let message_id = match args.get("message_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id,
        _ => return tool_error("Missing required parameter: message_id"),
    };

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::gmail_read_via(&backend, message_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gmail_create_draft(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let to = match args.get("to").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: to"),
    };
    let subject = match args.get("subject").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error("Missing required parameter: subject"),
    };
    let body = match args.get("body").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return tool_error("Missing required parameter: body"),
    };
    let cc = args
        .get("cc")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::gmail_create_draft_via(&backend, to, subject, body, cc).await {
        Ok(r) => tool_text(&format!(
            "Draft created — NOT sent. Review and send it manually in Gmail.\nDraft ID: {}\nTo: {}\nSubject: {}",
            r.draft_id, r.to, r.subject
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_calendar_list_events(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let time_min = args
        .get("time_min")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let time_max = args
        .get("time_max")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    // Validate provided timestamps before hitting the API.
    for (label, val) in [("time_min", time_min), ("time_max", time_max)] {
        if let Some(v) = val {
            if !gw::is_rfc3339(v) {
                return tool_error(&format!(
                    "Invalid {label}: '{v}' is not a valid RFC-3339 timestamp (e.g. 2026-07-26T14:00:00+08:00)"
                ));
            }
        }
    }
    let max = arg_u32(args, "max_results", 20);

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::calendar_list_events_via(&backend, time_min, time_max, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_calendar_create_event(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let summary = match args.get("summary").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: summary"),
    };
    let start = match args.get("start").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: start"),
    };
    let end = match args.get("end").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: end"),
    };
    if !gw::is_rfc3339(start) {
        return tool_error(&format!(
            "Invalid start: '{start}' is not RFC-3339 (e.g. 2026-07-26T14:00:00+08:00)"
        ));
    }
    if !gw::is_rfc3339(end) {
        return tool_error(&format!(
            "Invalid end: '{end}' is not RFC-3339 (e.g. 2026-07-26T15:00:00+08:00)"
        ));
    }
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let attendees: Option<Vec<String>> = args.get("attendees").and_then(|v| v.as_str()).map(|s| {
        s.split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect()
    });
    let with_meet = args
        .get("with_meet")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            args.get("with_meet")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("true"))
        })
        .unwrap_or(false);

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::calendar_create_event_via(
        &backend,
        summary,
        start,
        end,
        description,
        attendees.as_deref(),
        with_meet,
    )
    .await
    {
        Ok(r) => {
            let meet = r
                .meet_link
                .as_deref()
                .map(|l| format!("\nGoogle Meet: {l}"))
                .unwrap_or_default();
            tool_text(&format!(
                "Calendar event created (externally visible; attendees notified).\nEvent ID: {}\nSummary: {}\nStart: {}\nEnd: {}\nLink: {}{}",
                r.id, r.summary, r.start, r.end, r.html_link, meet
            ))
        }
        Err(e) => tool_error(&e.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
// Google Sheets tool handlers. Consume the OAuth vault token via the gateway
// `google_workspace` module (shared `google` provider — same connect flow as
// Gmail/Calendar; requires the spreadsheets scope, so a token authorized before
// Sheets shipped will 403 and guide the user to reconnect).
// ─────────────────────────────────────────────────────────────────

/// Read a u64 argument that may arrive as a JSON number or string.
pub(crate) fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64()).or_else(|| {
        args.get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| s.trim().parse::<u64>().ok())
    })
}

pub(crate) async fn handle_sheets_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let spreadsheet = match args.get("spreadsheet_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: spreadsheet_id"),
    };
    let range = match args.get("range").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: range"),
    };

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::sheets_read_via(&backend, spreadsheet, range).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_sheets_append(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let spreadsheet = match args.get("spreadsheet_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: spreadsheet_id"),
    };
    let range = match args.get("range").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: range"),
    };
    // Accept values as a JSON array of strings, or a comma-separated string.
    let values: Vec<String> = match args.get("values") {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect(),
        Some(Value::String(s)) => s.split(',').map(|c| c.trim().to_string()).collect(),
        _ => {
            return tool_error(
                "Missing required parameter: values (JSON array of strings or comma-separated list)",
            );
        }
    };
    if values.is_empty() {
        return tool_error("Parameter 'values' must contain at least one cell");
    }

    let backend = match gw::resolve_backend(home_dir).await {
        Ok(b) => b,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::sheets_append_via(&backend, spreadsheet, range, values).await {
        Ok(r) => tool_text(&format!(
            "Appended 1 row to the sheet.\nUpdated range: {}\nUpdated rows: {}\nUpdated cells: {}",
            r.updated_range, r.updated_rows, r.updated_cells
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
// Google Forms + Tasks handlers. Neither service has an official Google
// remote MCP server (probed 404 on 2026-07-30, absent from Google's docs), so
// DuDuClaw serves them natively off the same connected Google account.
// ─────────────────────────────────────────────────────────────────

/// Default Google Tasks list alias — the API accepts `@default` in place of an
/// id, so an agent can create/list tasks without first calling `tasks_lists`.
pub(crate) const TASKS_DEFAULT_LIST: &str = "@default";

pub(crate) fn task_list_arg(args: &Value) -> &str {
    args.get("task_list_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(TASKS_DEFAULT_LIST)
}

pub(crate) async fn handle_forms_get(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let form_id = match args.get("form_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: form_id"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::forms_get(&token, form_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_forms_list_responses(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let form_id = match args.get("form_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: form_id"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::forms_list_responses(&token, form_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gtasks_lists(home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::tasks_list_tasklists(&token).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gtasks_list(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let list_id = task_list_arg(args);
    let show_completed = args
        .get("show_completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max = arg_u32(args, "max_results", 50);

    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::tasks_list(&token, list_id, show_completed, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gtasks_create(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let title = match args.get("title").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: title"),
    };
    let list_id = task_list_arg(args);
    let notes = args
        .get("notes")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let due = args
        .get("due")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // Validate the timestamp locally so a bad format fails fast with guidance
    // rather than as an opaque Google 400.
    if let Some(d) = due {
        if !gw::is_rfc3339(d) {
            return tool_error(&format!(
                "Invalid due: '{d}' is not a valid RFC-3339 timestamp (e.g. 2026-08-01T00:00:00Z)"
            ));
        }
    }

    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::tasks_create(&token, list_id, title, notes, due).await {
        Ok(t) => tool_text(&format!(
            "Task created in the user's Google Tasks.\nTask ID: {}\nTitle: {}\nStatus: {}\nDue: {}",
            t.id,
            t.title,
            t.status,
            if t.due.is_empty() { "(none)" } else { &t.due }
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_gtasks_complete(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let task_id = match args.get("task_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return tool_error("Missing required parameter: task_id"),
    };
    let list_id = task_list_arg(args);

    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::tasks_complete(&token, list_id, task_id).await {
        Ok(t) => tool_text(&format!(
            "Task marked completed.\nTask ID: {}\nTitle: {}\nStatus: {}",
            t.id, t.title, t.status
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
// Google Drive / Docs / Slides handlers. Google's official MCP servers for
// these three are Developer-Preview-only and their terms forbid exposing
// Pre-GA APIs outside your own domain, so DuDuClaw uses the GA REST APIs
// natively (2026-07-30 decision) — no preview enrollment for any customer.
// ─────────────────────────────────────────────────────────────────

pub(crate) async fn handle_drive_search(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let mime_type = args
        .get("mime_type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let max = arg_u32(args, "max_results", 20);

    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::drive_search(&token, query, mime_type, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_drive_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let file_id = match args.get("file_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: file_id"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::drive_read(&token, file_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_docs_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let document_id = match args.get("document_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: document_id"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::docs_read(&token, document_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_docs_append(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let document_id = match args.get("document_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: document_id"),
    };
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return tool_error("Missing required parameter: text"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::docs_append(&token, document_id, text).await {
        Ok(r) => tool_text(&format!(
            "Appended to the Google Doc.\nDocument ID: {}\nCharacters appended: {}",
            r.document_id, r.appended_chars
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_slides_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::google_workspace as gw;

    let presentation_id = match args.get("presentation_id").and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => return tool_error("Missing required parameter: presentation_id"),
    };
    let token = match gw::get_valid_google_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gw::slides_read(&token, presentation_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
// Notion tool handlers. Consume the OAuth vault token via the gateway
// `notion_workspace` module. Notion content is an external knowledge source —
// surfaced for query/citation only, never auto-written into the shared wiki.
// ─────────────────────────────────────────────────────────────────
