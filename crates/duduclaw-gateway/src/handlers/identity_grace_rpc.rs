//! RM-166 round 2 (owner decision 2026-11-09): the dashboard's one-click
//! manual identity-token grace and "end grace now".
//!
//! Strict identity verification is never weakened automatically. During the
//! post-upgrade notice an Admin who sees identity refusals may turn on a
//! bounded grace — `[delegation] require_identity_token = false` plus
//! `identity_token_grace_until` at most 30 days ahead, the existing
//! mechanism the reader enforces — and end it at any time:
//!
//! - `system.identity_grace_start { password, code?, days? }` — Admin
//!   (dispatch), re-checked live, plus a **fresh step-up**: the caller's own
//!   current password in this request, against the same credential epoch as
//!   the session (the `mfa.disable` pattern), rate-limited like a login; an
//!   admin with MFA active must also send `code` (TOTP or recovery code),
//!   else the refusal is exactly `MFA_CODE_REQUIRED`. Failed password/MFA
//!   step-ups are audited (`identity_token_grace_step_up_failed`). The
//!   audit row is written (fail closed) before `config.toml` changes.
//! - `system.identity_grace_end {}` — Admin; the safe direction, so no
//!   step-up (like `maintenance.disable`). Audited.
//!
//! Both write through the locked, comment-preserving config commit and
//! re-broadcast `system.status` (whose `identity_token` field is Admin-only).

use super::*;
use crate::identity_transition::{self, GraceEdit, MAX_MANUAL_GRACE_DAYS};
use duduclaw_security::audit::{AuditEvent, Severity};

/// Audit event types (security_audit.jsonl).
pub(crate) const EVENT_GRACE_MANUAL_ENABLED: &str = "identity_token_grace_manual_enabled";
pub(crate) const EVENT_GRACE_MANUAL_FAILED: &str = "identity_token_grace_manual_failed";
pub(crate) const EVENT_GRACE_ENDED: &str = "identity_token_grace_ended";

pub(crate) const EVENT_GRACE_STEP_UP_FAILED: &str = "identity_token_grace_step_up_failed";

/// Refusal code (the whole error string) the dashboard keys on to ask for the
/// authenticator / recovery code; not a failed attempt.
pub(crate) const MFA_CODE_REQUIRED: &str = "MFA_CODE_REQUIRED";
const STEP_UP_FAILED_MFA: &str = "驗證碼不正確，未變更身分驗證設定。";
const STEP_UP_REQUIRED: &str = "請輸入您目前的登入密碼以確認這項變更。";
const STEP_UP_FAILED: &str = "密碼不正確，未變更身分驗證設定。";

impl MethodHandler {
    pub(crate) async fn handle_identity_grace_start(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.identity_grace_start(params, ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_identity_grace_end(&self, ctx: &UserContext) -> WsFrame {
        match self.identity_grace_end(ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn identity_grace_start(&self, params: Value, ctx: &UserContext) -> Result<Value, String> {
        let days = match params.get("days") {
            None | Some(Value::Null) => MAX_MANUAL_GRACE_DAYS,
            Some(v) => v
                .as_i64()
                .ok_or_else(|| format!("寬限天數必須是 1 到 {MAX_MANUAL_GRACE_DAYS} 天。"))?,
        };
        let grace_until = identity_transition::manual_grace_deadline(chrono::Utc::now(), days)?;
        let password = params
            .get("password")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty() && p.len() <= 4096)
            .ok_or(STEP_UP_REQUIRED)?
            .to_owned();
        // A legacy shared-token / local-fallback session has no account
        // whose password could be re-verified.
        if ctx.user_id == "system" {
            return Err("這項變更需要以管理員帳號登入並重新輸入密碼；或由管理者直接編輯 config.toml。".into());
        }
        let db = self
            .user_db
            .read()
            .await
            .clone()
            .ok_or("帳號系統尚未啟動，無法驗證密碼。")?;
        if identity_transition::effective_mode(&self.home_dir) == "grace" {
            return Err("目前已在寬限期內；如需重新設定，請先結束寬限期。".into());
        }
        if !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            &ctx.email,
        ) {
            return Err("嘗試次數過多，請稍後再試。".into());
        }

        // Fresh step-up: live Admin + the caller's own current password (and
        // the second factor when MFA is active) — see `admin_step_up`.
        let code = params
            .get("code")
            .and_then(Value::as_str)
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty() && c.len() <= 64);
        let step_up = admin_step_up(
            &self.home_dir,
            &db,
            ctx,
            password,
            code,
            StepUpCopy { failed_password: STEP_UP_FAILED, failed_mfa: STEP_UP_FAILED_MFA },
        )
        .await;
        let step_up = match step_up {
            Ok(kind) => kind,
            Err((message, failed_step)) => {
                if let Some(step) = failed_step {
                    let _ = db.log_action(Some(&ctx.user_id), "identity.grace_step_up_failed", None, None, None);
                    // Best-effort: a failed attempt is evidence, never a gate.
                    duduclaw_security::audit::append_audit_event(
                        &self.home_dir,
                        &grace_event(EVENT_GRACE_STEP_UP_FAILED, ctx, json!({"failed_step": step})),
                    );
                }
                return Err(message);
            }
        };

        // Audit first, fail closed: no weakening without a record of who,
        // until when and why.
        let refusals = identity_transition::status_json(&self.home_dir)["untrusted_refusals_24h"].clone();
        let event = grace_event(
            EVENT_GRACE_MANUAL_ENABLED,
            ctx,
            json!({
                "grace_until": grace_until,
                "days": days,
                "untrusted_refusals_24h": refusals,
                "weakens": "unverified AI-employee identity claims are accepted until grace_until",
                "step_up": step_up,
            }),
        );
        duduclaw_security::audit::try_append_audit_event(&self.home_dir, &event)
            .map_err(|_| "無法寫入稽核紀錄，未變更身分驗證設定。".to_string())?;

        let (guard_db, guard_id) = (db.clone(), ctx.user_id.clone());
        let edit = GraceEdit::Start { grace_until: grace_until.clone() };
        if let Err(e) = identity_transition::commit_grace_edit(&self.home_dir, edit, move || {
            crate::mfa::require_admin(&guard_db, &guard_id)
        })
        .await
        {
            duduclaw_security::audit::append_audit_event(
                &self.home_dir,
                &grace_event(EVENT_GRACE_MANUAL_FAILED, ctx, json!({"grace_until": grace_until})),
            );
            return Err(e);
        }
        let _ = db.log_action(Some(&ctx.user_id), "identity.grace_start", None, Some(&grace_until), None);
        tracing::warn!(
            actor = %ctx.user_id,
            grace_until = %grace_until,
            "AI employee identity verification: an Admin enabled a manual grace; unverified \
             employee identity claims are accepted until identity_token_grace_until"
        );
        self.broadcast_system_status().await;
        let identity = identity_transition::status_json(&self.home_dir);
        Ok(json!({
            "grace_until": grace_until,
            // This gateway process latched strict mode after an earlier expired
            // grace; hooks and MCP servers (separate processes) already honour
            // the new deadline, the gateway itself after a restart.
            "restart_required": identity["mode"] != "grace",
            "identity_token": identity,
        }))
    }

    async fn identity_grace_end(&self, ctx: &UserContext) -> Result<Value, String> {
        let db = self.user_db.read().await.clone();
        let id = ctx.user_id.clone();
        let changed = identity_transition::commit_grace_edit(&self.home_dir, GraceEdit::End, move || {
            match &db {
                // Re-check a real account live; the legacy shared-token admin
                // session has none and already passed the dispatch gate.
                Some(db) if id != "system" => crate::mfa::require_admin(db, &id),
                _ => Ok(()),
            }
        })
        .await?;
        if changed {
            duduclaw_security::audit::append_audit_event(
                &self.home_dir,
                &grace_event(EVENT_GRACE_ENDED, ctx, json!({"reason": "admin ended the grace from the dashboard"})),
            );
            if let Some(db) = self.user_db.read().await.clone() {
                let _ = db.log_action(Some(&ctx.user_id), "identity.grace_end", None, None, None);
            }
            tracing::info!(actor = %ctx.user_id, "AI employee identity verification: grace ended; strict mode");
            self.broadcast_system_status().await;
        }
        Ok(json!({
            "changed": changed,
            "identity_token": identity_transition::status_json(&self.home_dir),
        }))
    }
}

/// User-facing refusal copy for a failed step-up factor (the caller names
/// what was NOT changed).
pub(crate) struct StepUpCopy {
    pub failed_password: &'static str,
    pub failed_mfa: &'static str,
}

/// Fresh admin step-up shared by sensitive admin actions (RM-166 grace,
/// RM-173 team-history backdating): the caller must be a live Admin (no
/// pending password change) and present their OWN current password for the
/// credential epoch this session was issued under; an admin with MFA active
/// must also present a TOTP / recovery code (verified like `mfa.disable`),
/// else the refusal is exactly [`MFA_CODE_REQUIRED`].
///
/// `Ok(kind)` names the verified factors (`"password"` / `"password+mfa"`).
/// `Err((message, failed_step))`: `failed_step` is `Some` for a failed
/// credential check (callers audit it), `None` for a refusal that is not a
/// failed attempt. Callers apply their own login-style rate limit first and
/// refuse the shared-token `system` context (it has no password).
pub(crate) async fn admin_step_up(
    home: &std::path::Path,
    db: &Arc<UserDb>,
    ctx: &UserContext,
    password: String,
    code: Option<String>,
    copy: StepUpCopy,
) -> Result<&'static str, (String, Option<&'static str>)> {
    let (id, epoch, step_db, home) =
        (ctx.user_id.clone(), ctx.credential_epoch, db.clone(), home.to_path_buf());
    tokio::task::spawn_blocking(move || -> Result<&'static str, (String, Option<&'static str>)> {
        crate::mfa::require_admin(&step_db, &id).map_err(|e| (e, None))?;
        let user = step_db
            .get_user(&id)
            .map_err(|e| (e, None))?
            .ok_or(("無法讀取帳號資料。".to_string(), None))?;
        let verified = step_db
            .verify_password(&user.email, &password)
            .map_err(|_| (copy.failed_password.to_string(), Some("password")))?;
        if verified.id != id || verified.credential_epoch != epoch {
            return Err(("登入憑證已失效，請重新登入。".into(), None));
        }
        if !step_db.mfa_active(&id).map_err(|e| (e, None))? {
            return Ok("password");
        }
        let Some(code) = code else {
            return Err((MFA_CODE_REQUIRED.to_string(), None));
        };
        let now = chrono::Utc::now().timestamp();
        step_db
            .mfa_step_up_verify(&id, &code, &crate::mfa::Cipher(&home), now)
            .map_err(|_| (copy.failed_mfa.to_string(), Some("mfa")))?;
        Ok("password+mfa")
    })
    .await
    .unwrap_or_else(|_| Err(("身分確認暫時無法進行，請稍後再試。".to_string(), None)))
}

fn grace_event(event_type: &str, ctx: &UserContext, mut details: Value) -> AuditEvent {
    if let Some(map) = details.as_object_mut() {
        map.insert("actor_user_id".into(), json!(ctx.user_id));
        map.insert("actor_email".into(), json!(ctx.email));
    }
    AuditEvent::new(event_type, "system", Severity::Warning, details)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "grace-step-up-password";

    async fn fixture(email: &str) -> (tempfile::TempDir, MethodHandler, Arc<UserDb>, UserContext) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "# operator note\n[gateway]\nport = 18789\n").unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user(email, "Admin", PASSWORD, UserRole::Admin).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler
            .set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!")))
            .await;
        let ctx = db.live_user_context(&admin.id).unwrap().unwrap();
        (home, handler, db, ctx)
    }

    fn audit_types(home: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(home.join("security_audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<AuditEvent>(l).ok())
            .map(|e| e.event_type)
            .collect()
    }

    fn delegation(home: &std::path::Path) -> toml::Table {
        let t: toml::Table = std::fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
        t.get("delegation").and_then(|v| v.as_table()).cloned().unwrap_or_default()
    }

    fn ok(frame: &WsFrame) -> Value {
        match frame {
            WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
            other => panic!("expected ok, got {}", serde_json::to_string(other).unwrap_or_default()),
        }
    }

    fn is_err(frame: &WsFrame) -> bool {
        matches!(frame, WsFrame::Response { ok: false, .. })
    }

    #[tokio::test]
    async fn rm166b_manual_grace_requires_a_fresh_step_up_and_is_audited() {
        let (home, handler, _db, ctx) = fixture("grace-admin-1@test.invalid").await;
        let h = home.path();
        let before = std::fs::read(h.join("config.toml")).unwrap();

        // No password, a wrong password, an over-long grace: nothing written.
        for params in [
            json!({}),
            json!({"password": ""}),
            json!({"password": "wrong-password"}),
            json!({"password": PASSWORD, "days": 31}),
            json!({"password": PASSWORD, "days": 0}),
        ] {
            let frame = handler.handle_identity_grace_start(params.clone(), &ctx).await;
            assert!(is_err(&frame), "{params}");
            assert_eq!(std::fs::read(h.join("config.toml")).unwrap(), before, "{params}");
        }
        assert!(!audit_types(h).iter().any(|t| t == EVENT_GRACE_MANUAL_ENABLED));
        assert!(duduclaw_core::require_identity_token_from_home(h).unwrap());

        // The legacy shared-token admin cannot step up at all.
        let legacy = UserContext::admin_fallback();
        assert!(is_err(&handler.handle_identity_grace_start(json!({"password": PASSWORD}), &legacy).await));

        // Fresh password: bounded grace written through the preserving writer.
        let payload = ok(&handler.handle_identity_grace_start(json!({"password": PASSWORD}), &ctx).await);
        let until = payload["grace_until"].as_str().unwrap().to_string();
        let deadline = chrono::DateTime::parse_from_rfc3339(&until).unwrap().with_timezone(&chrono::Utc);
        assert!(deadline - chrono::Utc::now() <= chrono::Duration::days(30));
        let d = delegation(h);
        assert_eq!(d.get("require_identity_token").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(d.get("identity_token_grace_until").and_then(|v| v.as_str()), Some(until.as_str()));
        assert!(std::fs::read_to_string(h.join("config.toml")).unwrap().contains("# operator note"));
        assert_eq!(payload["identity_token"]["mode"], "grace");
        assert!(payload["identity_token"]["days_left"].as_i64().unwrap() <= 30);
        let types = audit_types(h);
        assert!(types.iter().any(|t| t == EVENT_GRACE_MANUAL_ENABLED), "{types:?}");
        let raw = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        assert!(raw.contains(&ctx.user_id) && raw.contains(&until));
        assert!(!raw.contains(PASSWORD), "the password is never audited");
        assert!(raw.contains(r#""step_up":"password""#), "no MFA: password-only step-up: {raw}");
        assert!(types.iter().any(|t| t == EVENT_GRACE_STEP_UP_FAILED), "wrong password is audited: {types:?}");

        // Already in grace: no silent extension.
        assert!(is_err(&handler.handle_identity_grace_start(json!({"password": PASSWORD}), &ctx).await));
        assert_eq!(delegation(h).get("identity_token_grace_until").and_then(|v| v.as_str()), Some(until.as_str()));
    }

    fn totp(secret: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut bytes = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in secret.bytes() {
            let v = if c.is_ascii_uppercase() { c - b'A' } else { c - b'2' + 26 };
            acc = (acc << 5) | v as u32;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes.push((acc >> bits) as u8);
            }
        }
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&bytes).unwrap();
        mac.update(&((chrono::Utc::now().timestamp() / 30) as u64).to_be_bytes());
        let hash = mac.finalize().into_bytes();
        let offset = (hash[19] & 15) as usize;
        let value = u32::from_be_bytes(hash[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
        format!("{:06}", value % 1_000_000)
    }

    /// Distinctive wrong code: "000000" can collide with timestamps/ids in the log.
    const WRONG_CODE: &str = "913572";

    #[tokio::test]
    async fn rm166c_mfa_admin_needs_the_second_factor_and_failures_are_audited() {
        let (home, handler, db, ctx) = fixture("grace-admin-mfa@test.invalid").await;
        let h = home.path();
        let now = chrono::Utc::now().timestamp();
        let cipher = crate::mfa::Cipher(h);
        let token = db.mfa_password_challenge(&ctx.user_id, PASSWORD, "test-session", now).unwrap();
        db.mfa_prepare_enrollment(&token, false, now).unwrap();
        let secret = db.mfa_begin(&ctx.user_id, Some(&token), &cipher, now).unwrap();
        let recovery = db
            .mfa_confirm(&ctx.user_id, Some(&token), &totp(&secret), &cipher, now)
            .unwrap();
        // Enrollment bumps the credential epoch: a fresh session context.
        let ctx = db.live_user_context(&ctx.user_id).unwrap().unwrap();
        let before = std::fs::read(h.join("config.toml")).unwrap();

        // Password alone: a refusal code the UI keys on, not a failed attempt.
        let frame = handler.handle_identity_grace_start(json!({"password": PASSWORD}), &ctx).await;
        match &frame {
            WsFrame::Response { ok: false, error: Some(e), .. } => assert_eq!(e, MFA_CODE_REQUIRED),
            other => panic!("{}", serde_json::to_string(other).unwrap_or_default()),
        }
        assert!(!audit_types(h).iter().any(|t| t == EVENT_GRACE_STEP_UP_FAILED));

        // Wrong code, wrong password: refused, written nothing, audited (N3).
        assert!(is_err(&handler.handle_identity_grace_start(json!({"password": PASSWORD, "code": WRONG_CODE}), &ctx).await));
        assert_eq!(audit_types(h).iter().filter(|t| *t == EVENT_GRACE_STEP_UP_FAILED).count(), 1);
        assert!(is_err(&handler.handle_identity_grace_start(json!({"password": "nope", "code": WRONG_CODE}), &ctx).await));
        assert_eq!(audit_types(h).iter().filter(|t| *t == EVENT_GRACE_STEP_UP_FAILED).count(), 2);
        assert_eq!(std::fs::read(h.join("config.toml")).unwrap(), before);
        assert!(!audit_types(h).iter().any(|t| t == EVENT_GRACE_MANUAL_ENABLED));
        let raw = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        assert!(!raw.contains(WRONG_CODE) && !raw.contains(PASSWORD));

        // Password + a recovery code: allowed, and the audit names the step-up.
        ok(&handler
            .handle_identity_grace_start(json!({"password": PASSWORD, "code": recovery[0]}), &ctx)
            .await);
        let raw = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        assert!(raw.contains("password+mfa"), "{raw}");
        assert!(!raw.contains(&recovery[0]));
        assert_eq!(identity_transition::effective_mode(h), "grace");
    }

    #[tokio::test]
    async fn rm166b_end_grace_restores_strict_mode_and_is_audited() {
        let (home, handler, _db, ctx) = fixture("grace-admin-2@test.invalid").await;
        let h = home.path();
        ok(&handler.handle_identity_grace_start(json!({"password": PASSWORD, "days": 7}), &ctx).await);
        assert_eq!(identity_transition::effective_mode(h), "grace");

        let payload = ok(&handler.handle_identity_grace_end(&ctx).await);
        assert_eq!(payload["changed"], true);
        assert_eq!(payload["identity_token"]["mode"], "strict");
        let d = delegation(h);
        assert_eq!(d.get("require_identity_token").and_then(|v| v.as_bool()), Some(true));
        assert!(!d.contains_key("identity_token_grace_until"));
        assert!(audit_types(h).iter().any(|t| t == EVENT_GRACE_ENDED));
        // Idempotent: nothing to end, nothing written or audited again.
        let ended = audit_types(h).iter().filter(|t| *t == EVENT_GRACE_ENDED).count();
        assert_eq!(ok(&handler.handle_identity_grace_end(&ctx).await)["changed"], false);
        assert_eq!(audit_types(h).iter().filter(|t| *t == EVENT_GRACE_ENDED).count(), ended);
    }

    #[tokio::test]
    async fn rm166b_grace_rpcs_are_admin_only_at_dispatch() {
        let (home, handler, db, _ctx) = fixture("grace-admin-3@test.invalid").await;
        let manager = db
            .create_user("grace-manager@test.invalid", "Manager", PASSWORD, UserRole::Manager)
            .unwrap();
        let manager = db.live_user_context(&manager.id).unwrap().unwrap();
        let before = std::fs::read(home.path().join("config.toml")).unwrap();
        for method in ["system.identity_grace_start", "system.identity_grace_end"] {
            let frame = handler.handle(method, json!({"password": PASSWORD}), &manager).await;
            assert!(is_err(&frame), "{method}");
        }
        assert_eq!(std::fs::read(home.path().join("config.toml")).unwrap(), before);
    }
}
