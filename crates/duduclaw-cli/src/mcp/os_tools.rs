use super::*;

pub(crate) async fn handle_os_notify(args: &Value) -> Value {
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    if title.trim().is_empty() && body.trim().is_empty() {
        return tool_error("os_notify 需要至少一個非空的 title 或 body。");
    }
    match duduclaw_os::send_notification(title, body).await {
        Ok(()) => tool_text(
            "通知已送出。若系統未顯示，請至「系統設定 → 通知」確認權限；\
             在 launchd 情境下 osascript 通知可能被系統靜音（無法程式化偵測，請人工確認）。",
        ),
        Err(e) => tool_error(&format!("通知送出失敗：{e}")),
    }
}

pub(crate) async fn handle_os_open(args: &Value) -> Value {
    let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
    if target.trim().is_empty() {
        return tool_error("os_open 需要非空的 `target`（檔案路徑或 http/https URL）。");
    }
    match duduclaw_os::open_path_or_url(target).await {
        Ok(()) => tool_text(&format!("已開啟：{target}")),
        Err(e) => tool_error(&format!("開啟失敗：{e}")),
    }
}

pub(crate) async fn handle_os_watch_status(home_dir: &Path, agent_id: &str) -> Value {
    let path = home_dir.join(duduclaw_gateway::os_events::STATS_FILE_NAME);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(_) => {
            return tool_text(
                "目前沒有檔案監看統計（os_watch 尚未啟動，或 gateway 尚未寫入第一份統計；\
                 統計每 60 秒更新一次）。",
            );
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return tool_error(&format!("os_watch_stats.json 解析失敗：{e}")),
    };
    let updated_at = value
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Namespace isolation: only ever surface the calling agent's own stats.
    match value.get("agents").and_then(|a| a.get(agent_id)) {
        Some(entry) => {
            let out = serde_json::json!({
                "agent_id": agent_id,
                "updated_at": updated_at,
                "watched_paths": entry.get("watched_paths").cloned().unwrap_or(Value::Array(vec![])),
                "emitted": entry.get("emitted").cloned().unwrap_or_else(|| Value::from(0)),
                "dropped": entry.get("dropped").cloned().unwrap_or_else(|| Value::from(0)),
            });
            tool_text(&serde_json::to_string_pretty(&out).unwrap_or_default())
        }
        None => tool_text(&format!(
            "代理「{agent_id}」目前沒有啟用中的檔案監看（未設定 [os_watch] paths，或 os_native 未啟用）。"
        )),
    }
}

// ─────────────────────────────────────────────────────────────────
// OS-native P2-4 tool handlers (os_frontmost / os_spotlight_search /
// os_calendar_today). All three are read-only structured sensing sources
// (research doc §②-6: "structured API over pixels") — no ActionGuard, only
// the [capabilities] os_native + Scope::OsNative gate enforced upstream in
// mcp_dispatch.
// ─────────────────────────────────────────────────────────────────

pub(crate) async fn handle_os_frontmost() -> Value {
    match duduclaw_os::frontmost_info().await {
        Ok(info) => {
            let out = serde_json::json!({
                "app": info.app,
                "window_title": info.window_title,
            });
            tool_text(&serde_json::to_string_pretty(&out).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("取得前景視窗失敗：{e}")),
    }
}

pub(crate) async fn handle_os_spotlight_search(args: &Value) -> Value {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return tool_error("os_spotlight_search 需要非空的 `query`。");
    }
    // `scope_dir` is expanded (`~` → home) the same way `[os_watch] paths` is,
    // before being handed to `duduclaw_os::spotlight_search`, which itself
    // canonicalizes it fail-closed (a non-existent scope is an error, not a
    // silently-unscoped search).
    let scope_dir = args
        .get("scope_dir")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(duduclaw_core::expand_tilde);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    match duduclaw_os::spotlight_search(query, scope_dir.as_deref(), limit).await {
        Ok(paths) => {
            let results: Vec<String> = paths
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let out = serde_json::json!({ "count": results.len(), "results": results });
            tool_text(&serde_json::to_string_pretty(&out).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("Spotlight 搜尋失敗：{e}")),
    }
}

pub(crate) async fn handle_os_calendar_today() -> Value {
    match duduclaw_os::today_events().await {
        Ok(events) => {
            let out_events: Vec<Value> = events
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "title": e.title,
                        "start": e.start,
                        "end": e.end,
                        "calendar": e.calendar,
                    })
                })
                .collect();
            let out = serde_json::json!({ "count": out_events.len(), "events": out_events });
            tool_text(&serde_json::to_string_pretty(&out).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("讀取行事曆失敗：{e}")),
    }
}

// ─────────────────────────────────────────────────────────────────
// Task Board / Activity Feed / Autopilot / Shared Skills MCP tools
// (Multica-inspired "Agent-as-teammate" integration, v1.8.27+)
// ─────────────────────────────────────────────────────────────────
