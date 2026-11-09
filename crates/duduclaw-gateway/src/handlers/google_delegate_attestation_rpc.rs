//! Admin attestation of a Google delegate subject (native Linux isolation
//! v1.81, SR4 item 1; rule D-5 (b) of the work breakdown).
//!
//! | method | who | what |
//! |---|---|---|
//! | `google.delegation.attest_subject {agent_id, subject, csrf_token}` | Admin + step-up | record that the Shared employee's `delegate:<subject>` belongs to the ONE Active users.db account with that login email |
//! | `google.delegation.revoke_attestation {agent_id, csrf_token}` | Admin + step-up | remove it |
//!
//! In protected mode a delegate employee acts as a person only for that
//! person, and only after this attestation: an email match alone proves
//! nothing (staff can set their own login email to any unused address). The
//! attestation records the account's user id, its email and the
//! `credential_changes` high-water mark in users.db; any later email change of
//! that account revokes it at use time (`google_delegate_attestation`).
//! Existing delegate configurations have no attestation and stay refused
//! until an Admin attests (the dashboard shows `attestation_required`).
//!
//! Authority exactly like `google.delegation.tenant_settings_set`:
//! dashboard-only, Enterprise-only, protected mode only, Admin re-read from
//! users.db (`mfa_require_current_admin`), `system` refused, and the migration
//! step-up proof (password + TOTP, five minutes, bound to this session and
//! credential epoch) with its CSRF token, re-checked on the blocking pool just
//! before the write. Audited twice: `auth_audit_log` in the write's own
//! transaction, and a security audit event after it.
use super::*;

use duduclaw_core::google_identity as gi;
use duduclaw_security::audit::{AuditEvent, Severity};

use crate::google_identity::tenant_source;

const NOT_ALLOWED: &str = "不允許此操作。";
const ADMIN_REQUIRED: &str = "請以管理員帳號重新登入後再操作。";
const NOT_PROTECTED: &str = "這個環境未啟用受保護模式；代表同仁的確認只在受保護模式使用。";
const USERS_UNAVAILABLE: &str = "無法讀取帳號資料，或有無法辨識的登入 email，已拒絕確認。";
const REQUEST_INVALID: &str = "請提供員工、要確認的信箱與驗證權杖。";
const NOT_SHARED: &str = "只有受保護模式的共用員工可以設定代表同仁；個人專屬員工不使用公司授權代表。";
const NOT_DELEGATE: &str = "這位員工目前設定的身分不是「代表這個信箱」。請先在員工的 Google 設定選擇代表此信箱。";
const IS_SERVICE_MAILBOX: &str = "這個信箱已指定為服務信箱（不屬於任何個人），不能同時確認為某位同仁的信箱。";
const CONFIG_UNREADABLE: &str = "目前的 Google 身分設定無法辨識，請先修正後再確認。";
const FAILED: &str = "無法保存確認，未變更。";
/// What the Admin confirms (reply + docs).
pub(crate) const ATTESTATION_COPY: &str =
    "管理員確認：這個信箱屬於上列同仁本人。AI 員工只會在該同仁本人提出請求時代表他使用 Google；同仁變更登入 email 後，這份確認會自動失效，需要重新確認。";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttestRequest {
    agent_id: String,
    subject: String,
    csrf_token: [u8; 32],
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    agent_id: String,
    csrf_token: [u8; 32],
}

/// The attestation itself, after the authority checks: `agent`'s configured
/// identity must be exactly `delegate:<subject>`, the subject must not be a
/// designated service mailbox, every stored login email must be comparable,
/// and exactly one Active account holds it (checked again inside the users.db
/// transaction). Blocking.
pub(crate) fn attest_subject(home: &Path, db: &UserDb, agent: &str, subject: &str, actor: &str)
    -> Result<Value, String> {
    if !is_valid_agent_id(agent) { return Err(REQUEST_INVALID.into()); }
    let subject = gi::normalize_email(subject).map_err(|_| REQUEST_INVALID.to_owned())?;
    let gi::SectionRead::Present(section) = gi::read_agent_section(home, agent) else {
        return Err(NOT_DELEGATE.into());
    };
    if !matches!(&section.identity, Some(gi::Selector::Delegate(configured)) if *configured == subject) {
        return Err(NOT_DELEGATE.into());
    }
    let config = gi::read_home_config(home).map_err(|_| CONFIG_UNREADABLE.to_owned())?;
    if config.service_mailboxes.contains(&subject) { return Err(IS_SERVICE_MAILBOX.into()); }
    // Every stored address (current and history) must be comparable.
    tenant_source::subject_accounts(db, agent, &subject).map_err(|_| USERS_UNAVAILABLE.to_owned())?;
    let matches = |email: &str| tenant_source::stored_email(email).ok().flatten().as_deref() == Some(subject.as_str());
    let attestation = db.attest_google_delegate(agent, &subject, &matches, actor)?;
    Ok(json!({
        "agent_id": agent,
        "subject": subject,
        "user_id": attestation.user_id,
        "attested_at": attestation.attested_at,
        "attestation": ATTESTATION_COPY,
    }))
}

impl MethodHandler {
    /// `google.delegation.attest_subject`.
    pub(crate) async fn handle_google_delegation_attest_subject(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.google_delegation_attestation(params, ctx, true).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(message) => WsFrame::error_response("", &message),
        }
    }

    /// `google.delegation.revoke_attestation`.
    pub(crate) async fn handle_google_delegation_revoke_attestation(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.google_delegation_attestation(params, ctx, false).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(message) => WsFrame::error_response("", &message),
        }
    }

    async fn google_delegation_attestation(&self, params: Value, ctx: &UserContext, attest: bool)
        -> Result<Value, String> {
        if ctx.user_id == "system" || ctx.user_id.is_empty() { return Err(NOT_ALLOWED.into()); }
        if ctx.requires_password_change() { return Err(ADMIN_REQUIRED.into()); }
        if acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err() {
            return Err(ADMIN_REQUIRED.into());
        }
        let session = crate::google_personal::session_for(&ctx.user_id).map_err(|_| ADMIN_REQUIRED)?;
        if !crate::runtime_readiness::is_protected_linux() { return Err(NOT_PROTECTED.into()); }
        let db = self.user_db.read().await.clone().ok_or(USERS_UNAVAILABLE)?;
        let (agent, subject, csrf) = if attest {
            let input: AttestRequest = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
            (input.agent_id, Some(input.subject), input.csrf_token)
        } else {
            let input: RevokeRequest = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
            (input.agent_id, None, input.csrf_token)
        };
        if !is_valid_agent_id(&agent) { return Err(REQUEST_INVALID.into()); }
        let (user, epoch, home) = (ctx.user_id.clone(), ctx.credential_epoch, self.home_dir.clone());
        let outcome = {
            let (db, user, agent, home) = (db.clone(), user.clone(), agent.clone(), home.clone());
            tokio::task::spawn_blocking(move || -> Result<Value, String> {
                super::google_tenant_settings_rpc::check_step_up(&db, &user, &session, epoch, &csrf)?;
                let Some(subject) = subject else {
                    let revoked = db.revoke_google_delegate_attestation(&agent, &user).map_err(|_| FAILED.to_owned())?;
                    return Ok(json!({ "agent_id": agent, "revoked": revoked }));
                };
                // Shared employees of this installation only.
                match crate::isolation_lifecycle::read_lifecycle(&home, &agent) {
                    Ok(lifecycle) if matches!(lifecycle.binding,
                        duduclaw_isolation::capability::EmployeeBinding::Shared(_)) => {}
                    _ => return Err(NOT_SHARED.into()),
                }
                attest_subject(&home, &db, &agent, &subject, &user)
            }).await.map_err(|_| FAILED.to_owned())??
        };
        let mut audit = outcome.clone();
        audit["actor_user_id"] = json!(user);
        audit["actor_email"] = json!(ctx.email);
        if let Some(object) = audit.as_object_mut() { object.remove("attestation"); }
        crate::security_autopilot::audit_and_emit(&home, &AuditEvent::new(
            if attest { "google_delegate_subject_attested" } else { "google_delegate_subject_attestation_revoked" },
            "dashboard", Severity::Warning, audit));
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTEST_METHOD: &str = "google.delegation.attest_subject";
    const REVOKE_METHOD: &str = "google.delegation.revoke_attestation";

    fn fixture() -> (tempfile::TempDir, UserDb) {
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[integrations.google_identity]\nallowed_subject_domains = [\"customer.com\"]\nservice_mailboxes = [\"support@customer.com\"]\n")
            .unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        (home, db)
    }
    fn delegate(home: &Path, subject: &str) {
        std::fs::write(home.join("agents/bot/agent.toml"), format!("[google]\nidentity = \"delegate:{subject}\"\n")).unwrap();
    }

    #[test]
    fn sr4_attest_subject_binds_the_configured_delegate_to_one_person() {
        let (home, db) = fixture();
        let alice = db.create_user("Alice@Customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        // Not a delegate employee (yet).
        assert_eq!(attest_subject(home.path(), &db, "bot", "alice@customer.com", "admin-id").unwrap_err(), NOT_DELEGATE);
        delegate(home.path(), "alice@customer.com");
        // The subject must be exactly the configured one.
        assert_eq!(attest_subject(home.path(), &db, "bot", "bob@customer.com", "admin-id").unwrap_err(), NOT_DELEGATE);
        let reply = attest_subject(home.path(), &db, "bot", " ALICE@customer.com ", "admin-id").unwrap();
        assert_eq!((reply["user_id"].as_str(), reply["subject"].as_str()), (Some(alice.id.as_str()), Some("alice@customer.com")));
        assert_eq!(reply["attestation"], ATTESTATION_COPY);
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("alice@customer.com", &alice.id));
        // The account changes its email: revoked at use time.
        db.change_own_email(&alice.id, "alice2@customer.com").unwrap();
        db.change_own_email(&alice.id, "Alice@Customer.com").unwrap();
        assert!(!db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id));
        // A designated service mailbox is never a person's.
        delegate(home.path(), "support@customer.com");
        assert_eq!(attest_subject(home.path(), &db, "bot", "support@customer.com", "admin-id").unwrap_err(),
            IS_SERVICE_MAILBOX);
        // Nobody holds the address: nothing to attest.
        delegate(home.path(), "ceo@customer.com");
        assert!(attest_subject(home.path(), &db, "bot", "ceo@customer.com", "admin-id").is_err());
        // An address that cannot be compared refuses.
        db.create_user("ceo@customer.com.", "Odd", "password1", UserRole::Employee).unwrap();
        assert_eq!(attest_subject(home.path(), &db, "bot", "ceo@customer.com", "admin-id").unwrap_err(), USERS_UNAVAILABLE);
    }

    #[test]
    fn sr4_attestation_request_carries_no_user_id() {
        let ok = json!({"agent_id": "bot", "subject": "alice@customer.com", "csrf_token": vec![7u8; 32]});
        assert!(serde_json::from_value::<AttestRequest>(ok.clone()).is_ok());
        for extra in ["user_id", "admin", "step_up_valid", "email"] {
            let mut forged = ok.clone();
            forged[extra] = json!("x");
            assert!(serde_json::from_value::<AttestRequest>(forged).is_err(), "{extra}");
        }
        assert!(serde_json::from_value::<RevokeRequest>(json!({"agent_id": "bot", "csrf_token": vec![7u8; 32]})).is_ok());
    }

    #[tokio::test]
    async fn sr4_attestation_refuses_non_admin_and_system() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let params = json!({"agent_id": "bot", "subject": "alice@customer.com", "csrf_token": vec![7u8; 32]});
        let system = UserContext::admin_fallback();
        let mut employee = UserContext::admin_fallback();
        employee.user_id = "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10".into();
        employee.role = UserRole::Employee;
        for caller in [&system, &employee] {
            for method in [ATTEST_METHOD, REVOKE_METHOD] {
                let frame = handler.handle(method, params.clone(), caller).await;
                assert!(!matches!(frame, WsFrame::Response { ok: true, .. }), "{method}");
            }
        }
    }
}
