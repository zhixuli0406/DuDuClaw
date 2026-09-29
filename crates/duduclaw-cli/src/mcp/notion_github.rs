use super::*;

pub(crate) async fn handle_notion_status(home_dir: &Path) -> Value {
    use duduclaw_gateway::mcp_oauth;
    use duduclaw_gateway::notion_workspace::NOTION_PROVIDER;

    let token = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == NOTION_PROVIDER);
    let configured = mcp_oauth::has_client_config(home_dir, NOTION_PROVIDER);

    let mut out = String::new();
    match token {
        None => {
            out.push_str("Notion: NOT connected.\n");
            if configured {
                out.push_str(
                    "Integration credentials are set. Open the dashboard Integrations → Notion page and click \"Connect Notion\" to authorize.",
                );
            } else {
                out.push_str(
                    "No Notion integration is configured. Open the dashboard Integrations → Notion page to set up your Notion OAuth integration, then connect.",
                );
            }
        }
        Some(_t) => {
            out.push_str(
                "Notion: connected.\nToken: valid (Notion access tokens are long-lived and do not expire).\nRemember: a page/database must be shared with your integration before these tools can see it.",
            );
        }
    }
    tool_text(out.trim_end())
}

pub(crate) async fn handle_notion_search(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::notion_workspace as nw;

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return tool_error("Missing required parameter: query"),
    };
    let max = arg_u32(args, "max_results", 10);

    let token = match nw::get_valid_notion_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match nw::notion_search(&token, query, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_notion_page_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::notion_workspace as nw;

    let page_id = match args.get("page_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim(),
        _ => return tool_error("Missing required parameter: page_id"),
    };

    let token = match nw::get_valid_notion_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match nw::notion_page_read(&token, page_id).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_notion_page_append(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::notion_workspace as nw;

    let page_id = match args.get("page_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim(),
        _ => return tool_error("Missing required parameter: page_id"),
    };
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return tool_error("Missing required parameter: text"),
    };

    let token = match nw::get_valid_notion_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match nw::notion_page_append(&token, page_id, text).await {
        Ok(r) => tool_text(&format!(
            "Appended {} paragraph block(s) to Notion page {}.",
            r.appended_blocks, r.page_id
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
// GitHub tool handlers. Consume the OAuth vault token via the gateway
// `github_workspace` module. github_issue_comment posts a publicly visible
// comment — operators should gate it via approval_required_tools.
// ─────────────────────────────────────────────────────────────────

pub(crate) async fn handle_github_status(home_dir: &Path) -> Value {
    use duduclaw_gateway::github_workspace::GITHUB_PROVIDER;
    use duduclaw_gateway::mcp_oauth;

    let token = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GITHUB_PROVIDER);
    let configured = mcp_oauth::has_client_config(home_dir, GITHUB_PROVIDER);

    let mut out = String::new();
    match token {
        None => {
            out.push_str("GitHub: NOT connected.\n");
            if configured {
                out.push_str(
                    "OAuth App credentials are set. Open the dashboard Integrations → GitHub page and click \"Connect GitHub\" to authorize.",
                );
            } else {
                out.push_str(
                    "No GitHub OAuth App is configured. Open the dashboard Integrations → GitHub page to set up your GitHub OAuth App, then connect.",
                );
            }
        }
        Some(t) => {
            let expiry = match t.expires_at {
                Some(exp) if chrono::Utc::now() >= exp => {
                    format!("EXPIRED ({})", exp.to_rfc3339())
                }
                Some(exp) => format!("valid (expires {})", exp.to_rfc3339()),
                None => "valid (no expiry — classic OAuth App token)".to_string(),
            };
            out.push_str(&format!(
                "GitHub: connected.\nToken: {expiry}\nGranted scopes:\n"
            ));
            if t.scopes.is_empty() {
                out.push_str("  (none recorded)\n");
            } else {
                for s in &t.scopes {
                    out.push_str(&format!("  - {s}\n"));
                }
            }
            if !t.scopes.iter().any(|s| s == "repo") {
                out.push_str(
                    "\nNote: the 'repo' scope is not granted — private repositories will 404. Reconnect from the dashboard to grant it.\n",
                );
            }
        }
    }
    tool_text(out.trim_end())
}

pub(crate) async fn handle_github_search_issues(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::github_workspace as gh;

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let max = arg_u32(args, "max_results", 10);

    let token = match gh::get_valid_github_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gh::github_search_issues(&token, query, max).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

/// Extract (owner, repo, number) from tool args (owner/repo strings + numeric).
/// Returns a static error string on missing/invalid input. Uses the fully
/// qualified `std::result::Result` because this module aliases `Result<T>` to a
/// single-parameter `DuDuClawError` result.
pub(crate) fn github_target(args: &Value) -> std::result::Result<(String, String, u64), &'static str> {
    let owner = args
        .get("owner")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or("Missing required parameter: owner")?;
    let repo = args
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or("Missing required parameter: repo")?;
    let number = arg_u64(args, "number").ok_or("Missing/invalid required parameter: number")?;
    Ok((owner.to_string(), repo.to_string(), number))
}

pub(crate) async fn handle_github_issue_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::github_workspace as gh;

    let (owner, repo, number) = match github_target(args) {
        Ok(t) => t,
        Err(e) => return tool_error(e),
    };
    let token = match gh::get_valid_github_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gh::github_issue_read(&token, &owner, &repo, number).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_github_pr_read(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::github_workspace as gh;

    let (owner, repo, number) = match github_target(args) {
        Ok(t) => t,
        Err(e) => return tool_error(e),
    };
    let token = match gh::get_valid_github_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gh::github_pr_read(&token, &owner, &repo, number).await {
        Ok(r) => tool_text(&serde_json::to_string_pretty(&r).unwrap_or_default()),
        Err(e) => tool_error(&e.to_string()),
    }
}

pub(crate) async fn handle_github_issue_comment(args: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::github_workspace as gh;

    let (owner, repo, number) = match github_target(args) {
        Ok(t) => t,
        Err(e) => return tool_error(e),
    };
    let body = match args.get("body").and_then(|v| v.as_str()) {
        Some(b) if !b.trim().is_empty() => b,
        _ => return tool_error("Missing required parameter: body"),
    };
    let token = match gh::get_valid_github_token(home_dir).await {
        Ok(t) => t,
        Err(e) => return tool_error(&e.to_string()),
    };
    match gh::github_issue_comment(&token, &owner, &repo, number, body).await {
        Ok(r) => tool_text(&format!(
            "Comment posted (publicly visible) on {owner}/{repo}#{number}.\nComment ID: {}\nLink: {}",
            r.id, r.url
        )),
        Err(e) => tool_error(&e.to_string()),
    }
}
