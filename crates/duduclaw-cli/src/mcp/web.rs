use super::*;

/// Per-process rate limiter shared by web_fetch_cached and web_extract
/// (10 requests/min, matching the gateway-side default).
pub(crate) static WEB_FETCH_LIMITER: std::sync::LazyLock<duduclaw_gateway::web_fetch::RateLimiter> =
    std::sync::LazyLock::new(duduclaw_gateway::web_fetch::RateLimiter::new);

/// Cap on body/extraction text returned through MCP (keeps responses sane).
pub(crate) const WEB_BODY_CAP_CHARS: usize = 60_000;

pub(crate) async fn fetch_for_tool(
    args: &Value,
    home_dir: &Path,
) -> std::result::Result<duduclaw_gateway::web_fetch::FetchResult, Value> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| tool_error("Missing required parameter: url"))?;
    let ttl = args
        .get("ttl_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if !WEB_FETCH_LIMITER.check("mcp-server") {
        return Err(tool_error(
            "Rate limit exceeded (10 requests/min) — try again shortly",
        ));
    }

    if super::workflow_operation::is_workflow_read() {
        return duduclaw_gateway::web_fetch::web_fetch_workflow(url)
            .await
            .map_err(|e| tool_error(&format!("Fetch failed: {e}")));
    }
    let cache_dir = home_dir.join("web_cache");
    duduclaw_gateway::web_fetch::web_fetch_cached(url, ttl, &cache_dir)
        .await
        .map_err(|e| tool_error(&format!("Fetch failed: {e}")))
}

pub(crate) async fn handle_web_fetch_cached(args: &Value, home_dir: &Path) -> Value {
    let result = match fetch_for_tool(args, home_dir).await {
        Ok(r) => r,
        Err(err) => return err,
    };

    let total_chars = result.body.chars().count();
    let truncated = total_chars > WEB_BODY_CAP_CHARS;
    let body: String = result.body.chars().take(WEB_BODY_CAP_CHARS).collect();
    let report = serde_json::json!({
        "url": result.url,
        "status_code": result.status_code,
        "content_type": result.content_type,
        "cached": result.cached,
        "fetched_at": result.fetched_at.to_rfc3339(),
        "body_chars": total_chars,
        "truncated": truncated,
        "body": body,
    });
    if super::workflow_operation::is_workflow_read() {
        // A public-page fetch cannot prove authenticated account state. A login
        // challenge, empty/truncated response or old observation is unusable.
        let lower = result.body.to_ascii_lowercase();
        if truncated
            || result.body.trim().is_empty()
            || lower.contains("type=\"password\"")
            || lower.contains("type='password'")
            || chrono::Utc::now()
                .signed_duration_since(result.fetched_at)
                .num_seconds()
                > 60
        {
            return tool_error("workflow public page empty/truncated/stale/login challenge");
        }
        use sha2::{Digest, Sha256};
        let source_hash = format!("{:x}", Sha256::digest(result.body.as_bytes()));
        super::workflow_operation::read_observed(
            report.clone(),
            serde_json::json!({
                "adapter": "web_fetch_cached",
                "adapter_version": 1,
                "source_url": result.url,
                "observed_at": result.fetched_at.to_rfc3339(),
                "source_hash": source_hash,
                "authenticated": false,
                "login_state": "public_anonymous",
                "redirects_followed": 0,
                "cached": false
            }),
        );
    }
    tool_text(&serde_json::to_string_pretty(&report).unwrap_or_default())
}

pub(crate) async fn handle_web_extract(args: &Value, home_dir: &Path) -> Value {
    let selector = match args.get("selector").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return tool_error("Missing required parameter: selector"),
    };
    let format = match args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
    {
        "html" => duduclaw_gateway::web_extract::OutputFormat::Html,
        "json" => duduclaw_gateway::web_extract::OutputFormat::Json,
        "text" => duduclaw_gateway::web_extract::OutputFormat::Text,
        other => return tool_error(&format!("Invalid format: {other}. Valid: text, html, json")),
    };

    let fetched = match fetch_for_tool(args, home_dir).await {
        Ok(r) => r,
        Err(err) => return err,
    };

    let queries = vec![duduclaw_gateway::web_extract::SelectorQuery {
        name: "result".to_string(),
        selector: selector.clone(),
        format,
    }];
    match duduclaw_gateway::web_extract::extract_multiple(&fetched.body, &queries) {
        Ok(extraction) => {
            let values = extraction
                .results
                .get("result")
                .cloned()
                .unwrap_or_default();
            let report = serde_json::json!({
                "url": fetched.url,
                "selector": selector,
                "matches": values.len(),
                "cached": fetched.cached,
                "results": values,
            });
            let mut text = serde_json::to_string_pretty(&report).unwrap_or_default();
            if text.chars().count() > WEB_BODY_CAP_CHARS {
                text = text.chars().take(WEB_BODY_CAP_CHARS).collect::<String>() + "\n…[truncated]";
            }
            tool_text(&text)
        }
        Err(e) => tool_error(&format!("Extraction failed: {e}")),
    }
}

// ── Wiki Knowledge Base handlers ────────────────────────────
