//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── User management handlers (admin only) ────────────────

    pub(crate) async fn handle_users_list(&self) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        match db.list_users() {
            Ok(users) => {
                let mut result: Vec<Value> = Vec::new();
                for u in &users {
                    let bindings = db.get_user_agents(&u.id).unwrap_or_default();
                    result.push(json!({
                        "id": u.id,
                        "email": u.email,
                        "display_name": u.display_name,
                        "role": u.role,
                        "status": u.status,
                        "department": u.department,
                        "created_at": u.created_at,
                        "updated_at": u.updated_at,
                        "last_login": u.last_login,
                        "bindings": bindings,
                    }));
                }
                WsFrame::ok_response("", json!({ "users": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to list users: {e}")),
        }
    }

    pub(crate) async fn handle_users_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let email = params.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let password = params
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let role_str = params
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("employee");

        if email.is_empty() || display_name.is_empty() || password.is_empty() {
            return WsFrame::error_response("", "email, display_name, and password are required");
        }
        // Email format validation (MEDIUM fix)
        if !email.contains('@') || email.len() > 254 {
            return WsFrame::error_response("", "invalid email format");
        }
        // Display name length limit
        if display_name.len() > 200 {
            return WsFrame::error_response("", "display_name too long (max 200 chars)");
        }
        if password.len() < 8 {
            return WsFrame::error_response("", "password must be at least 8 characters");
        }
        if password.len() > 1024 {
            return WsFrame::error_response("", "password too long");
        }

        let role: UserRole = match role_str.parse() {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Optional department (install-approval routing). Validate the slug
        // if present; empty / absent ⇒ no department.
        let department = params
            .get("department")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        if let Some(d) = department {
            if !duduclaw_core::is_valid_department(d) {
                return WsFrame::error_response(
                    "",
                    "invalid department name (1-64 bytes, no path separators / whitespace / control chars)",
                );
            }
        }

        match db.create_user(email, display_name, password, role) {
            Ok(mut user) => {
                if let Some(d) = department {
                    if let Err(e) = db.set_department(&user.id, Some(d)) {
                        return WsFrame::error_response(
                            "",
                            &format!("user created but setting department failed: {e}"),
                        );
                    }
                    user.department = Some(d.to_string());
                }
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "user.create",
                    Some(&user.id),
                    Some(&format!("email={email}")),
                    None,
                );
                WsFrame::ok_response("", json!({ "user": user }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to create user: {e}")),
        }
    }

    pub(crate) async fn handle_users_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        let display_name = params.get("display_name").and_then(|v| v.as_str());
        let role = params
            .get("role")
            .and_then(|v| v.as_str())
            .and_then(|r| r.parse::<UserRole>().ok());
        let password = params.get("password").and_then(|v| v.as_str());

        if let Some(pw) = password {
            if pw.len() < 8 {
                return WsFrame::error_response("", "password must be at least 8 characters");
            }
            if pw.len() > 1024 {
                return WsFrame::error_response("", "password too long");
            }
        }
        if let Some(name) = display_name {
            if name.len() > 200 {
                return WsFrame::error_response("", "display_name too long (max 200 chars)");
            }
        }

        // Department update: an explicit "" clears it; a non-empty value is
        // validated. Absent key ⇒ leave unchanged.
        let department_change: Option<Option<String>> = match params.get("department") {
            Some(Value::String(s)) if s.trim().is_empty() => Some(None),
            Some(Value::String(s)) => {
                let d = s.trim();
                if !duduclaw_core::is_valid_department(d) {
                    return WsFrame::error_response(
                        "",
                        "invalid department name (1-64 bytes, no path separators / whitespace / control chars)",
                    );
                }
                Some(Some(d.to_string()))
            }
            Some(Value::Null) => Some(None),
            _ => None,
        };

        match db.update_user(user_id, display_name, role, password) {
            Ok(()) => {
                if let Some(dept) = department_change {
                    if let Err(e) = db.set_department(user_id, dept.as_deref()) {
                        return WsFrame::error_response(
                            "",
                            &format!("failed to update department: {e}"),
                        );
                    }
                }
                let _ = db.log_action(Some(&ctx.user_id), "user.update", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "updated"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to update user: {e}")),
        }
    }

    pub(crate) async fn handle_users_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        match db.set_user_status(user_id, duduclaw_auth::UserStatus::Suspended) {
            Ok(()) => {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "user.suspend",
                    Some(user_id),
                    None,
                    None,
                );
                WsFrame::ok_response("", json!({"status": "suspended"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to suspend user: {e}")),
        }
    }

    pub(crate) async fn handle_users_bind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };
        let access_level_str = params
            .get("access_level")
            .and_then(|v| v.as_str())
            .unwrap_or("owner");
        let access_level: AccessLevel = match access_level_str.parse() {
            Ok(l) => l,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Verify agent exists
        let reg = self.registry.read().await;
        if reg.get(agent_name).is_none() {
            return WsFrame::error_response("", &format!("agent not found: {agent_name}"));
        }
        drop(reg);

        match db.bind_agent(user_id, agent_name, access_level) {
            Ok(()) => {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "user.bind_agent",
                    Some(agent_name),
                    Some(&format!("user={user_id}, level={access_level}")),
                    None,
                );
                WsFrame::ok_response("", json!({"status": "bound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to bind agent: {e}")),
        }
    }

    pub(crate) async fn handle_users_unbind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };

        match db.unbind_agent(user_id, agent_name) {
            Ok(()) => {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "user.unbind_agent",
                    Some(agent_name),
                    Some(&format!("user={user_id}")),
                    None,
                );
                WsFrame::ok_response("", json!({"status": "unbound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to unbind agent: {e}")),
        }
    }

    pub(crate) async fn handle_users_offboard(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let transfer_to = params.get("transfer_to").and_then(|v| v.as_str());

        // Get user's bound agents before offboarding
        let bindings = db.get_user_agents(user_id).unwrap_or_default();

        // Set user status to offboarded
        if let Err(e) = db.set_user_status(user_id, duduclaw_auth::UserStatus::Offboarded) {
            return WsFrame::error_response("", &format!("failed to offboard user: {e}"));
        }

        // Transfer agent ownership if specified
        let mut transferred = Vec::new();
        if let Some(new_owner_id) = transfer_to {
            for binding in &bindings {
                // Unbind from old user
                let _ = db.unbind_agent(user_id, &binding.agent_name);
                // Bind to new owner
                let _ = db.bind_agent(new_owner_id, &binding.agent_name, binding.access_level);
                transferred.push(binding.agent_name.clone());
            }
        }

        let _ = db.log_action(
            Some(&ctx.user_id),
            "user.offboard",
            Some(user_id),
            Some(&format!(
                "transferred_agents={transferred:?}, transfer_to={transfer_to:?}"
            )),
            None,
        );

        WsFrame::ok_response(
            "",
            json!({
                "status": "offboarded",
                "transferred_agents": transferred,
            }),
        )
    }

    pub(crate) async fn handle_users_me(&self, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => {
                // No user DB — return context from JWT
                return WsFrame::ok_response(
                    "",
                    json!({
                        "user": {
                            "id": ctx.user_id,
                            "email": ctx.email,
                            "role": ctx.role.to_string(),
                        },
                        "bindings": [],
                    }),
                );
            }
        };

        match db.get_user(&ctx.user_id) {
            Ok(Some(user)) => {
                let bindings = db.get_user_agents(&user.id).unwrap_or_default();
                WsFrame::ok_response(
                    "",
                    json!({
                        "user": user,
                        "bindings": bindings,
                    }),
                )
            }
            _ => WsFrame::ok_response(
                "",
                json!({
                    "user": {
                        "id": ctx.user_id,
                        "email": ctx.email,
                        "role": ctx.role.to_string(),
                    },
                    "bindings": [],
                }),
            ),
        }
    }

    /// Self-service password change for the logged-in user. Available in every
    /// edition: the personal/single-owner edition hides the multi-user Users
    /// page, so this is the only way the sole admin can rotate their own
    /// password. No admin role required — it only ever mutates the caller's own
    /// account (identified by `ctx.user_id`). Verifies the current password
    /// first; on success `update_user` also clears any `must_change_password`.
    pub(crate) async fn handle_users_change_password(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let current = params
            .get("current_password")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_password = match params.get("new_password").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return WsFrame::error_response("", "new_password is required"),
        };
        if new_password.len() < 8 {
            return WsFrame::error_response("", "new password must be at least 8 characters");
        }
        if new_password.len() > 1024 {
            return WsFrame::error_response("", "new password too long");
        }

        // Resolve the caller's own account; the stored email is needed to verify
        // the current password.
        let user = match db.get_user(&ctx.user_id) {
            Ok(Some(u)) => u,
            _ => return WsFrame::error_response("", "user not found"),
        };

        // Verify the current password (timing-safe inside verify_password).
        if db.verify_password(&user.email, current).is_err() {
            let _ = db.log_action(
                Some(&ctx.user_id),
                "password.change_failed",
                Some(&ctx.user_id),
                None,
                None,
            );
            return WsFrame::error_response("", "current password is incorrect");
        }

        if current == new_password {
            return WsFrame::error_response("", "new password must differ from the current one");
        }

        match db.update_user(&ctx.user_id, None, None, Some(new_password)) {
            Ok(()) => {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "password.change",
                    Some(&ctx.user_id),
                    None,
                    None,
                );
                WsFrame::ok_response("", json!({"status": "changed"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to change password: {e}")),
        }
    }

    pub(crate) async fn handle_users_audit_log(&self, params: Value) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = params.get("user_id").and_then(|v| v.as_str());
        let action = params.get("action").and_then(|v| v.as_str());
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(1000) as u32;

        match db.query_audit_log(user_id, action, limit) {
            Ok(entries) => WsFrame::ok_response("", json!({ "entries": entries })),
            Err(e) => WsFrame::error_response("", &format!("failed to query audit log: {e}")),
        }
    }
}
