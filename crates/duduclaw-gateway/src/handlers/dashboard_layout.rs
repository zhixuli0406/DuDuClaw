//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Personal dashboard (WP15 MVP): widget catalog + per-user layout ──

    /// Widgets a given ROLE may see (fail-closed: an unauthorized widget id is
    /// never sent, and `layout.set` re-validates against this same set).
    pub(crate) fn dashboard_widgets_for_role(role: UserRole) -> Vec<(&'static str, &'static str)> {
        const CATALOG: &[(&str, &str, UserRole)] = &[
            ("needs_me", "manager", UserRole::Manager),
            ("my_agents", "employee", UserRole::Employee),
            ("recent_activity", "employee", UserRole::Employee),
            ("my_tasks", "employee", UserRole::Employee),
            ("channel_health", "admin", UserRole::Admin),
        ];
        CATALOG
            .iter()
            .filter(|(_, _, min)| role.level() >= min.level())
            .map(|(id, min_str, _)| (*id, *min_str))
            .collect()
    }

    /// Widgets the CALLER may see.
    pub(crate) fn dashboard_widgets_for(ctx: &UserContext) -> Vec<(&'static str, &'static str)> {
        Self::dashboard_widgets_for_role(ctx.role)
    }

    /// Read a saved layout file (shared by `get` and the read-only `view`).
    pub(crate) async fn read_dashboard_layout(&self, user_id: &str) -> Result<Value, String> {
        let path = self.dashboard_layout_path(user_id)?;
        Ok(tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v.is_object())
            .unwrap_or(Value::Null))
    }

    /// `users.subordinates` — ACTIVE reports in the caller's reporting tree
    /// (all other active accounts for Admin), with minimal fields. Feeds the
    /// read-only dashboard viewer's picker; deliberately far narrower than
    /// the admin-gated `users.list` (no email, no bindings, no status detail).
    pub(crate) async fn handle_users_subordinates(&self, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::ok_response("", json!({ "users": [] })),
        };
        let all = match db.list_users() {
            Ok(users) => users,
            Err(_) => return WsFrame::error_response("", "user list unavailable"),
        };
        let visible = dashboard_report_ids(&all, ctx);
        let users: Vec<Value> = all.into_iter()
            .filter(|u| {
                visible.contains(&u.id)
                    && matches!(u.status, duduclaw_auth::models::UserStatus::Active)
            })
            .map(|u| {
                json!({
                    "id": u.id,
                    "display_name": u.display_name,
                    "role": u.role.to_string().to_lowercase(),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "users": users }))
    }

    /// Read-only view of an actual report's board (Admin may view all others).
    /// The write path always remains the caller's own layout.
    pub(crate) async fn handle_dashboard_layout_view(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(target_id) = params
            .get("user_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "user_id is required");
        };
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        let Ok(Some(target)) = db.get_user(target_id) else {
            return WsFrame::error_response("", "user not found");
        };
        let users = match db.list_users() {
            Ok(users) => users,
            Err(_) => return WsFrame::error_response("", "user list unavailable"),
        };
        if !dashboard_report_ids(&users, ctx).contains(&target.id)
            || !matches!(target.status, duduclaw_auth::models::UserStatus::Active)
        {
            return WsFrame::error_response("", "permission denied");
        }

        let layout = match self.read_dashboard_layout(&target.id).await {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let widgets: Vec<Value> = Self::dashboard_widgets_for_role(target.role)
            .into_iter()
            .map(|(id, min_role)| json!({ "id": id, "min_role": min_role }))
            .collect();
        // The target's bound agents let the viewer's client scope widget data
        // the way the target actually sees it (WP11 employee data scope).
        let bound_agents: Vec<String> = db
            .get_user_agents(&target.id)
            .unwrap_or_default()
            .into_iter()
            .map(|b| b.agent_name)
            .collect();
        // Custom widgets referenced by the target's layout, WITH html — the
        // view-as grant (strict-rank, enforced above) covers rendering the
        // subordinate's board, and `widgets.custom.get` would deny the viewer
        // for the target's private widgets. Only ids actually on the layout
        // are exposed, read-only like everything else here.
        let custom_widgets: Vec<Value> = {
            let referenced: Vec<String> = layout
                .get("widgets")
                .and_then(|v| v.as_array())
                .map(|ws| {
                    ws.iter()
                        .filter_map(|w| w.get("id").and_then(|i| i.as_str()))
                        .filter_map(|id| {
                            id.strip_prefix(crate::custom_widgets::LAYOUT_ID_PREFIX)
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if referenced.is_empty() {
                Vec::new()
            } else {
                match crate::custom_widgets::CustomWidgetStore::open(&self.home_dir) {
                    Ok(store) => {
                        let mut out = Vec::new();
                        for id in referenced {
                            if let Ok(Some(w)) = store.get(&id).await {
                                out.push(json!({ "id": w.id, "title": w.title, "html": w.html }));
                            }
                        }
                        out
                    }
                    Err(_) => Vec::new(),
                }
            }
        };
        WsFrame::ok_response(
            "",
            json!({
                "user": {
                    "id": target.id,
                    "display_name": target.display_name,
                    "role": target.role.to_string().to_lowercase(),
                },
                "widgets": widgets,
                "layout": layout,
                "bound_agents": bound_agents,
                "custom_widgets": custom_widgets,
                "read_only": true,
            }),
        )
    }

    /// Path for a user's saved layout. The user id comes from a verified JWT
    /// (uuid or "system"), but validate the charset anyway before path use.
    pub(crate) fn dashboard_layout_path(&self, user_id: &str) -> Result<std::path::PathBuf, String> {
        let ok = !user_id.is_empty()
            && user_id.len() <= 64
            && user_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err("invalid user id".into());
        }
        Ok(self
            .home_dir
            .join("dashboard")
            .join("layouts")
            .join(format!("{user_id}.json")))
    }

    pub(crate) async fn handle_dashboard_widgets_catalog(&self, ctx: &UserContext) -> WsFrame {
        let widgets: Vec<Value> = Self::dashboard_widgets_for(ctx)
            .into_iter()
            .map(|(id, min_role)| json!({ "id": id, "min_role": min_role }))
            .collect();
        WsFrame::ok_response("", json!({ "widgets": widgets }))
    }

    pub(crate) async fn handle_dashboard_layout_get(&self, ctx: &UserContext) -> WsFrame {
        match self.read_dashboard_layout(&ctx.user_id).await {
            Ok(layout) => WsFrame::ok_response("", json!({ "layout": layout })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_dashboard_layout_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let path = match self.dashboard_layout_path(&ctx.user_id) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let Some(widgets) = params.get("widgets").and_then(|v| v.as_array()) else {
            return WsFrame::error_response("", "widgets array is required");
        };
        if widgets.len() > 32 {
            return WsFrame::error_response("", "layout accepts at most 32 widgets");
        }
        let allowed: std::collections::HashSet<&str> = Self::dashboard_widgets_for(ctx)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        // Custom widgets ride the layout as `custom:<uuid>` — allowed when the
        // widget is VISIBLE to the caller (their own or instance-shared).
        // Resolved once here so the loop below stays synchronous.
        let custom_visible: std::collections::HashSet<String> =
            match crate::custom_widgets::CustomWidgetStore::open(&self.home_dir) {
                Ok(store) => store
                    .list_visible(&ctx.user_id)
                    .await
                    .map(|ws| ws.into_iter().map(|w| w.id).collect())
                    .unwrap_or_default(),
                Err(_) => Default::default(),
            };
        let mut normalized: Vec<Value> = Vec::with_capacity(widgets.len());
        let mut seen = std::collections::HashSet::new();
        for w in widgets {
            let Some(id) = w.get("id").and_then(|v| v.as_str()) else {
                return WsFrame::error_response("", "each widget needs an 'id'");
            };
            // Fail-closed: an id outside the caller's own catalogue is refused
            // (never silently dropped — the client should know it drifted).
            let is_ok = if let Some(cid) = id.strip_prefix(crate::custom_widgets::LAYOUT_ID_PREFIX)
            {
                custom_visible.contains(cid)
            } else {
                allowed.contains(id)
            };
            if !is_ok {
                return WsFrame::error_response(
                    "",
                    &format!("widget '{id}' is not available to you"),
                );
            }
            if !seen.insert(id.to_string()) {
                continue; // duplicates collapse to first occurrence
            }
            let hidden = w.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
            normalized.push(json!({ "id": id, "hidden": hidden }));
        }
        let layout = json!({ "schema": 1, "widgets": normalized });

        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return WsFrame::error_response("", &format!("Failed to create layout dir: {e}"));
            }
        }
        let body = serde_json::to_string_pretty(&layout).unwrap_or_default();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp, &body).await {
            return WsFrame::error_response("", &format!("Failed to write layout: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit layout: {e}"));
        }
        WsFrame::ok_response("", json!({ "success": true, "layout": layout }))
    }
}

/// Walk the existing manager links, including through suspended managers,
/// but never through offboarded accounts or invalid Employee managers.
fn dashboard_report_ids(users: &[duduclaw_auth::models::User], ctx: &UserContext) -> std::collections::HashSet<String> {
    use duduclaw_auth::models::UserStatus;
    if ctx.is_admin() {
        return users.iter().filter(|u| u.id != ctx.user_id).map(|u| u.id.clone()).collect();
    }
    let mut seen = std::collections::HashSet::from([ctx.user_id.clone()]);
    let mut visible = std::collections::HashSet::new();
    if ctx.role.level() < UserRole::Manager.level() { return visible; }
    let mut parents = vec![ctx.user_id.clone()];
    for _ in 0..duduclaw_auth::org::MAX_ORG_DEPTH {
        let mut next = Vec::new();
        for user in users {
            if matches!(user.status, UserStatus::Offboarded)
                || !user.manager_user_id.as_ref().is_some_and(|id| parents.contains(id))
                || !seen.insert(user.id.clone()) { continue; }
            visible.insert(user.id.clone());
            if user.role.level() >= UserRole::Manager.level() { next.push(user.id.clone()); }
        }
        if next.is_empty() { break; }
        parents = next;
    }
    visible
}
