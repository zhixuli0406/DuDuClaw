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
    use duduclaw_gateway::google_workspace::{
        GOOGLE_PROVIDER, GoogleBackend, REQUIRED_SCOPES, resolve_backend,
    };
    use duduclaw_gateway::mcp_oauth;

    let token = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GOOGLE_PROVIDER);
    let configured = mcp_oauth::has_client_config(home_dir, GOOGLE_PROVIDER);

    // Lead with who this employee acts as (per-employee Google identity).
    // A person identity or a refusal is the whole answer: the home vault
    // details below would describe an account this employee does not use.
    let (header, describes_home) = super::google_identity_client::status_header(home_dir, g).await;
    let mut out = header;
    if !describes_home {
        return tool_text(out.trim_end());
    }
    out.push('\n');

    // Lead with the credential source actually in effect. Three exist (OAuth
    // vault, service-account delegation, Apps Script bridge) and the OAuth
    // detail below only describes the first — without this line a customer on
    // delegation or the bridge reads "NOT connected" while their tools work.
    match resolve_backend(home_dir).await {
        Ok(GoogleBackend::Direct(_)) => {
            out.push_str("Credential source: direct API token (OAuth vault or service-account delegation). Token obtained; service API access has not been verified. Run per-service verification in the dashboard.\n\n");
        }
        Ok(GoogleBackend::AppsScript(cfg)) => {
            out.push_str(&format!(
                "Credential source: {}. The bridge supports eight actions across Gmail / Calendar / Sheets; this status has not tested them; Drive, Docs, Slides, Forms and Tasks need OAuth or a service account.\n\n",
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
                    "\nMissing scopes needed for Google Workspace functionality (reconnect from the dashboard to grant):\n",
                );
                for m in missing {
                    out.push_str(&format!("  - {m}\n"));
                }
            }
        }
    }
    tool_text(out.trim_end())
}
