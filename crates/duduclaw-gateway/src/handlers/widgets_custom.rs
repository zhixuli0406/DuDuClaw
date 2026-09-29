//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Custom widgets (sandboxed-iframe HTML cards) ─────────
    //
    // Store: `custom_widgets.rs`. The SECURITY boundary is the renderer
    // (unique-origin sandboxed iframe + injected CSP + allowlisted data
    // bridge), so any authenticated user may own widgets; the admin-only gate
    // on `origin = "html"` keeps the raw-HTML authoring surface a
    // distributor/engineer tool as designed, it is not a security control.

    pub(crate) fn custom_widget_store(&self) -> Result<crate::custom_widgets::CustomWidgetStore, WsFrame> {
        crate::custom_widgets::CustomWidgetStore::open(&self.home_dir)
            .map_err(|e| WsFrame::error_response("", &format!("open custom widgets: {e}")))
    }

    /// Serialize a widget for list payloads — html is stripped (lazy-loaded
    /// via `widgets.custom.get` at render time) but its size is reported.
    pub(crate) fn custom_widget_summary(w: &crate::custom_widgets::CustomWidget) -> Value {
        json!({
            "id": w.id,
            "title": w.title,
            "description": w.description,
            "origin": w.origin.as_str(),
            "created_by_user": w.created_by_user,
            "shared": w.shared,
            "html_bytes": w.html.len(),
            "created_at": w.created_at,
            "updated_at": w.updated_at,
        })
    }

    pub(crate) async fn handle_widgets_custom_list(&self, ctx: &UserContext) -> WsFrame {
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_visible(&ctx.user_id).await {
            Ok(rows) => {
                let items: Vec<Value> = rows.iter().map(Self::custom_widget_summary).collect();
                // 0 = unlimited; the client only renders a "/cap" suffix when > 0.
                let max_per_user = crate::custom_widgets::max_widgets_per_user();
                WsFrame::ok_response(
                    "",
                    json!({ "widgets": items, "max_per_user": max_per_user }),
                )
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_widgets_custom_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "id is required");
        };
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.get(id).await {
            Ok(Some(w)) if w.shared || w.created_by_user == ctx.user_id => {
                let mut v = Self::custom_widget_summary(&w);
                v["html"] = json!(w.html);
                WsFrame::ok_response("", v)
            }
            Ok(_) => WsFrame::error_response("", "找不到此 widget 或無權檢視"),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_widgets_custom_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let html = params.get("html").and_then(|v| v.as_str()).unwrap_or("");
        let origin_raw = params
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("ai");
        let Some(origin) = crate::custom_widgets::WidgetOrigin::parse(origin_raw) else {
            return WsFrame::error_response("", "origin must be 'html' or 'ai'");
        };
        // Product gate (not a security boundary — the sandbox is): the raw
        // HTML authoring surface is admin-only per the 2026-07-16 design.
        if origin == crate::custom_widgets::WidgetOrigin::Html && ctx.role != UserRole::Admin {
            return WsFrame::error_response("", "HTML 自訂 widget 僅限管理員建立");
        }
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store
            .create(title, description, html, origin, &ctx.user_id)
            .await
        {
            Ok(id) => WsFrame::ok_response("", json!({ "success": true, "id": id })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_widgets_custom_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "id is required");
        };
        let title = params.get("title").and_then(|v| v.as_str());
        let description = params.get("description").and_then(|v| v.as_str());
        let html = params.get("html").and_then(|v| v.as_str());
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        // Editing the HTML of an `origin=html` widget stays admin-only (same
        // product gate as create); AI widgets are re-generated, and their
        // owner may still rename/describe them here.
        if html.is_some() && ctx.role != UserRole::Admin {
            match store.get(id).await {
                Ok(Some(w)) if w.origin == crate::custom_widgets::WidgetOrigin::Html => {
                    return WsFrame::error_response("", "HTML widget 僅限管理員編輯");
                }
                Ok(_) => {}
                Err(e) => return WsFrame::error_response("", &e),
            }
        }
        match store
            .update(id, &ctx.user_id, title, description, html)
            .await
        {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_widgets_custom_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "id is required");
        };
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store
            .remove(id, &ctx.user_id, ctx.role == UserRole::Admin)
            .await
        {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_widgets_custom_share(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "id is required");
        };
        let shared = params
            .get("shared")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let store = match self.custom_widget_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.set_shared(id, &ctx.user_id, shared).await {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true, "shared": shared })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `widgets.custom.generate` — the guided natural-language flow (P2).
    /// Generates widget HTML from the picker answers + freeform description
    /// (optionally revising a prior draft with feedback). NOTHING is stored —
    /// the client previews in the sandbox and calls `widgets.custom.create`
    /// only when the user accepts. Call chain mirrors the night engine:
    /// rotated Claude CLI (zero-tool caps) → Direct API fallback.
    pub(crate) async fn handle_widgets_custom_generate(&self, params: Value) -> WsFrame {
        let freeform = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let style = params.get("style").and_then(|v| v.as_str()).unwrap_or("");
        let data_sources: Vec<String> = params
            .get("data_sources")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let prior_html = params.get("prior_html").and_then(|v| v.as_str());
        let feedback = params.get("feedback").and_then(|v| v.as_str());
        if freeform.trim().is_empty() && data_sources.is_empty() && prior_html.is_none() {
            return WsFrame::error_response("", "請至少選擇一種資料來源或描述需求");
        }
        if freeform.chars().count() > 2000 {
            return WsFrame::error_response("", "需求描述最長 2000 字");
        }

        let (system, user) = crate::custom_widgets::build_generation_prompt(
            &data_sources,
            style,
            freeform,
            prior_html,
            feedback,
        );
        // Zero-tool capabilities: generation is pure text-out, the spawned CLI
        // must not get the default tool/MCP surface. (Named for the night
        // engine, but it is just the shared "no tools" preset.)
        let caps = crate::night_llm::night_capabilities();
        let model = crate::custom_widgets::GENERATE_MODEL;

        let cli_err = match crate::channel_reply::call_claude_cli_rotated(
            &user,
            model,
            &system,
            &self.home_dir,
            None,
            None,
            Some(&caps),
            None,
            &[],
            // System-level utility call, not an agent turn — no account pool.
            &[],
            None, // P1/WP-3 effort: resolved from the agent dir in the callee
        )
        .await
        {
            Ok(text) if !text.trim().is_empty() => {
                let html = match crate::custom_widgets::extract_html_fragment(&text) {
                    Ok(h) => h,
                    Err(e) => return WsFrame::error_response("", &format!("產生結果無效：{e}")),
                };
                if let Err(e) = crate::custom_widgets::validate_widget_fields("t", "", &html) {
                    return WsFrame::error_response("", &format!("產生結果無效：{e}"));
                }
                return WsFrame::ok_response("", json!({ "html": html }));
            }
            Ok(_) => "empty CLI response".to_string(),
            Err(e) => duduclaw_core::truncate_chars(&e, 200),
        };

        let api_key = crate::claude_runner::get_api_key_from_home(&self.home_dir).await;
        if api_key.is_empty() {
            return WsFrame::error_response(
                "",
                &format!("widget 產生失敗（{cli_err}），且未設定 API key 可作備援"),
            );
        }
        match crate::direct_api::call_direct_api(&api_key, model, &system, &user, &[]).await {
            Ok(resp) if !resp.text.trim().is_empty() => {
                let html = match crate::custom_widgets::extract_html_fragment(&resp.text) {
                    Ok(h) => h,
                    Err(e) => return WsFrame::error_response("", &format!("產生結果無效：{e}")),
                };
                if let Err(e) = crate::custom_widgets::validate_widget_fields("t", "", &html) {
                    return WsFrame::error_response("", &format!("產生結果無效：{e}"));
                }
                WsFrame::ok_response("", json!({ "html": html }))
            }
            Ok(_) => WsFrame::error_response("", "widget 產生失敗：模型回傳空內容"),
            Err(e) => WsFrame::error_response(
                "",
                &format!(
                    "widget 產生失敗：{}",
                    duduclaw_core::truncate_chars(&e, 200)
                ),
            ),
        }
    }
}
