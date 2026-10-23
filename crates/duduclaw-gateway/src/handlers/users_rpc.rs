//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;
use super::users_org::{ManagerChange, org_error_frame, parse_manager_change};

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
                        "manager_user_id": u.manager_user_id,
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

        // Optional direct manager: created in the same transaction as the
        // account, so a refused manager leaves no account behind.
        let manager = match parse_manager_change(&params) {
            Ok(change) => change.target().map(str::to_string),
            Err(frame) => return frame,
        };

        // Argon2 + SQLite: off the runtime workers.
        let created = {
            let db = db.clone();
            let (email, display_name, password) =
                (email.to_string(), display_name.to_string(), password.to_string());
            let (department, manager) = (department.map(str::to_string), manager.clone());
            tokio::task::spawn_blocking(move || {
                db.create_user_with(&duduclaw_auth::NewUser {
                    email: &email,
                    display_name: &display_name,
                    password: &password,
                    role,
                    department: department.as_deref(),
                    manager: manager.as_deref(),
                })
            })
            .await
        };
        match created {
            Ok(Ok(user)) => {
                if let Some(m) = &manager {
                    let _ = db.log_action(
                        Some(&ctx.user_id),
                        "user.set_manager",
                        Some(&user.id),
                        Some(&format!("manager={m}")),
                        None,
                    );
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
            Ok(Err(duduclaw_auth::UserWriteError::Org(e))) => org_error_frame(&e),
            Ok(Err(duduclaw_auth::UserWriteError::Other(e))) => WsFrame::error_response("", &e),
            Err(_) => WsFrame::error_response("", "failed to create user"),
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

        let manager_change = match parse_manager_change(&params) {
            Ok(change) => change,
            Err(frame) => return frame,
        };

        // Every field of this request commits together or not at all: a
        // refused manager or demotion leaves the name, password and
        // department as they were.
        let applied = {
            let db = db.clone();
            let user_id = user_id.to_string();
            let display_name = display_name.map(str::to_string);
            let password = password.map(str::to_string);
            let manager_change = manager_change.clone();
            tokio::task::spawn_blocking(move || {
                let manager = match &manager_change {
                    ManagerChange::Unchanged => None,
                    ManagerChange::Clear => Some(None),
                    ManagerChange::Set(m) => Some(Some(m.as_str())),
                };
                db.apply_user_update(
                    &user_id,
                    &duduclaw_auth::UserUpdate {
                        display_name: display_name.as_deref(),
                        role,
                        password: password.as_deref(),
                        department: department_change.as_ref().map(|d| d.as_deref()),
                        manager,
                    },
                )
            })
            .await
        };
        match applied {
            Ok(Ok(())) => {
                if manager_change != ManagerChange::Unchanged {
                    let _ = db.log_action(
                        Some(&ctx.user_id),
                        "user.set_manager",
                        Some(user_id),
                        Some(&format!("manager={}", manager_change.target().unwrap_or("none"))),
                        None,
                    );
                }
                let _ = db.log_action(Some(&ctx.user_id), "user.update", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "updated"}))
            }
            Ok(Err(duduclaw_auth::UserWriteError::Org(e))) => org_error_frame(&e),
            Ok(Err(duduclaw_auth::UserWriteError::Other(e))) => {
                WsFrame::error_response("", &format!("failed to update user: {e}"))
            }
            Err(_) => WsFrame::error_response("", "failed to update user"),
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

        let google_affected_agents = crate::google_personal::lifecycle::affected_agents(&self.home_dir, &db, user_id);
        match db.set_user_status(user_id, duduclaw_auth::UserStatus::Suspended) {
            Ok(()) => {
                let cleanup_error = crate::google_personal::lifecycle::disconnect(&self.home_dir, &db, user_id, &ctx.user_id, true).await.err();
                if let Ok(agents) = &google_affected_agents { crate::google_personal::lifecycle::notify_affected(&self.home_dir, &ctx.user_id, agents); }
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "user.suspend",
                    Some(user_id),
                    None,
                    None,
                );
                WsFrame::ok_response("", json!({"status": "suspended", "google_cleanup_error":cleanup_error,"google_affected_agents":google_affected_agents.ok()}))
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
        let transfer_to = params
            .get("transfer_to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty());

        // One transaction: status, the leaver's reports (moved to
        // `transfer_to`, else to the leaver's manager, each re-checked and
        // audited as `user.reparent`) and, with `transfer_to`, the bindings.
        let google_affected_agents = crate::google_personal::lifecycle::affected_agents(&self.home_dir, &db, user_id);
        let outcome = match db.offboard_user(user_id, transfer_to, Some(&ctx.user_id)) {
            Ok(outcome) => outcome,
            Err(duduclaw_auth::OrgError::UserNotFound) => {
                return WsFrame::error_response("", "failed to offboard user: user not found");
            }
            Err(e) => return org_error_frame(&e),
        };
        let transferred = outcome.transferred_agents;
        let google_cleanup_error = crate::google_personal::lifecycle::disconnect(&self.home_dir, &db, user_id, &ctx.user_id, true).await.err();
        if let Ok(agents) = &google_affected_agents { crate::google_personal::lifecycle::notify_affected(&self.home_dir, &ctx.user_id, agents); }
        let reparented: Vec<Value> = outcome
            .reparented
            .iter()
            .map(|r| json!({ "user_id": r.user_id, "manager_user_id": r.to }))
            .collect();

        let _ = db.log_action(
            Some(&ctx.user_id),
            "user.offboard",
            Some(user_id),
            Some(&format!(
                "transferred_agents={transferred:?}, transfer_to={transfer_to:?}, reparented={}",
                reparented.len()
            )),
            None,
        );

        WsFrame::ok_response(
            "",
            json!({
                "status": "offboarded",
                "google_cleanup_error": google_cleanup_error,
                "google_affected_agents": google_affected_agents.ok(),
                "transferred_agents": transferred,
                "reparented": reparented,
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
                let setup = self.email_setup_state(&db, &user).await;
                WsFrame::ok_response(
                    "",
                    json!({
                        "user": user,
                        "bindings": bindings,
                        "email_setup": {
                            "placeholder": setup.placeholder,
                            "password_required": setup.password_required,
                        },
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

    /// How the caller's own email can be changed.
    ///
    /// `placeholder`: the caller is the bootstrap owner still on
    /// `admin@local` (the Personal-edition implicit claim, or a claim that set
    /// no email), so the dashboard offers to set a real address.
    ///
    /// `password_required`: changing the email or the password needs the
    /// current password, except for the bootstrap owner on a Personal install
    /// with local auto-login on. There the owner never chose a password (the
    /// implicit claim discards a random one) and anyone at the machine already
    /// gets this session without one, so asking for it would only lock the
    /// owner out — including out of setting a password before turning
    /// auto-login off.
    async fn email_setup_state(
        &self,
        db: &duduclaw_auth::UserDb,
        user: &duduclaw_auth::User,
    ) -> EmailSetupState {
        let is_owner = matches!(db.get_bootstrap_owner(), Ok(Some(ref o)) if o.id == user.id);
        // `must_change_password` still set means the implicit claim never ran:
        // the owner holds the one-time bootstrap password and must prove it.
        let passwordless_owner = is_owner
            && !user.must_change_password
            && crate::local_session::auto_login_enabled(&self.home_dir)
            && self.resolve_edition_profile().await.is_personal();
        EmailSetupState {
            placeholder: is_owner && user.email == duduclaw_auth::db::BOOTSTRAP_PLACEHOLDER_EMAIL,
            password_required: !passwordless_owner,
        }
    }

    /// Self-service email change for the logged-in user. Like
    /// `users.change_password`, it only ever touches the caller's own account
    /// (`ctx.user_id`) and needs no admin role. The current password is
    /// required unless [`Self::email_setup_state`] says otherwise. The caller's
    /// tokens keep the old email until refreshed; the dashboard refreshes them
    /// right after.
    pub(crate) async fn handle_users_change_email(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        let raw = params.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let email = match duduclaw_auth::db::validate_account_email(raw) {
            Ok(e) => e,
            Err(reason) => return WsFrame::error_response("", reason),
        };
        let user = match db.get_user(&ctx.user_id) {
            Ok(Some(u)) => u,
            _ => return WsFrame::error_response("", "user not found"),
        };
        if self.email_setup_state(&db, &user).await.password_required {
            let current = params
                .get("current_password")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if db.verify_password(&user.email, current).is_err() {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "email.change_failed",
                    Some(&ctx.user_id),
                    None,
                    None,
                );
                return WsFrame::error_response("", "current password is incorrect");
            }
        }
        if email == user.email {
            return WsFrame::ok_response("", json!({"status": "unchanged", "email": email}));
        }
        match db.change_email(&ctx.user_id, email) {
            Ok(()) => {
                // The audit row keeps both addresses: it is the only record of
                // which login name the account used before.
                let detail = format!("{} -> {}", user.email, email);
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    "email.change",
                    Some(&ctx.user_id),
                    Some(&detail),
                    None,
                );
                WsFrame::ok_response("", json!({"status": "changed", "email": email}))
            }
            Err(e) if e == duduclaw_auth::db::EMAIL_IN_USE => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("failed to change email: {e}")),
        }
    }

    /// Self-service password change for the logged-in user. Available in every
    /// edition: the personal/single-owner edition hides the multi-user Users
    /// page, so this is the only way the sole admin can rotate their own
    /// password. No admin role required — it only ever mutates the caller's own
    /// account (identified by `ctx.user_id`). Verifies the current password
    /// first, unless [`Self::email_setup_state`] says none is needed; on
    /// success `update_user` also clears any `must_change_password`.
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
        let password_required = self.email_setup_state(&db, &user).await.password_required;
        if password_required && db.verify_password(&user.email, current).is_err() {
            let _ = db.log_action(
                Some(&ctx.user_id),
                "password.change_failed",
                Some(&ctx.user_id),
                None,
                None,
            );
            return WsFrame::error_response("", "current password is incorrect");
        }

        if password_required && current == new_password {
            return WsFrame::error_response("", "new password must differ from the current one");
        }

        match db.update_user(&ctx.user_id, None, None, Some(new_password)) {
            Ok(()) => {
                let _ = db.log_action(
                    Some(&ctx.user_id),
                    if password_required { "password.change" } else { "password.set_local_owner" },
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

/// See [`MethodHandler::email_setup_state`].
struct EmailSetupState {
    placeholder: bool,
    password_required: bool,
}
