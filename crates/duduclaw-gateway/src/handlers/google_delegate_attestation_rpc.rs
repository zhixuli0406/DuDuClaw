//! Admin attestation of a Google delegate subject (native Linux isolation
//! v1.81, SR4 item 1; rule D-5 (b) of the work breakdown).
//!
//! | method | who | what |
//! |---|---|---|
//! | `google.delegation.attest_subject {agent_id, subject, user_id, acknowledge_self_set?, csrf_token}` | Admin + step-up | record that the Shared employee's `delegate:<subject>` belongs to the ONE Active users.db account with that login email, which must be `user_id` |
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
//! SR4 round 3:
//! - N1: the Admin attests the holder they were shown (`tenant_subject.holder`
//!   of `google.delegation.get`: name, user id, account creation date,
//!   whether the holder set the address themself). `user_id` is required and
//!   must be the single Active holder inside the attesting transaction
//!   (`attest_holder_mismatch` otherwise). A self-set address also needs
//!   `acknowledge_self_set: true`; the acknowledgement is audited.
//! - N2: the attestation binds the live employee id and incarnation of the
//!   label; a recreated or restored employee needs a fresh one.
//!
//! Authority exactly like `google.delegation.tenant_settings_set`:
//! dashboard-only, Enterprise-only, protected mode only, Admin re-read from
//! users.db (`mfa_require_current_admin`), `system` refused, and the migration
//! step-up proof (password + TOTP, five minutes, bound to this session and
//! credential epoch) with its CSRF token, re-checked on the blocking pool just
//! before the write. Audited twice: `auth_audit_log` in the write's own
//! transaction, and a security audit event after it.
use super::*;

use duduclaw_auth::google_delegate_attestation::AttestationRequest;
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
/// Longest accepted `user_id` (users.db ids are UUIDs).
const MAX_USER_ID: usize = 128;

/// What the Admin confirms (reply + docs), naming the listed person.
pub(crate) fn attestation_copy(subject: &str, name: &str) -> String {
    format!("管理員確認：{subject} 是「{name}」本人的信箱。AI 員工只會在「{name}」本人提出請求時，代表本人使用 Google；\
        「{name}」變更登入 email 後，這份確認會自動失效，需要重新確認。")
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttestRequest {
    agent_id: String,
    subject: String,
    /// SR4 round 3 (N1): the holder the Admin was shown.
    user_id: String,
    /// SR4 round 3 (N1 (3)): required when the holder set the address themself.
    #[serde(default)]
    acknowledge_self_set: bool,
    csrf_token: [u8; 32],
}

/// One attestation request after parsing.
pub(crate) struct Attest<'a> {
    pub(crate) agent: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) acknowledge_self_set: bool,
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
/// and exactly one Active account holds it — the account the Admin was shown
/// (`request.user_id`), with `acknowledge_self_set` when that account set the
/// address itself — all checked again inside the users.db transaction. The
/// row binds `employee` (the live employee id and incarnation). Blocking.
pub(crate) fn attest_subject(home: &Path, db: &UserDb, request: &Attest<'_>, employee: tenant_source::Incarnation,
    actor: &str) -> Result<Value, String> {
    let agent = request.agent;
    if !is_valid_agent_id(agent) { return Err(REQUEST_INVALID.into()); }
    if request.user_id.trim().is_empty() || request.user_id.len() > MAX_USER_ID {
        return Err(REQUEST_INVALID.into());
    }
    let subject = gi::normalize_email(request.subject).map_err(|_| REQUEST_INVALID.to_owned())?;
    let gi::SectionRead::Present(section) = gi::read_agent_section(home, agent) else {
        return Err(NOT_DELEGATE.into());
    };
    if !matches!(&section.identity, Some(gi::Selector::Delegate(configured)) if *configured == subject) {
        return Err(NOT_DELEGATE.into());
    }
    let config = gi::read_home_config(home).map_err(|_| CONFIG_UNREADABLE.to_owned())?;
    if config.service_mailboxes.contains(&subject) { return Err(IS_SERVICE_MAILBOX.into()); }
    // Every stored address (current and history) must be comparable.
    tenant_source::subject_accounts(db, agent, Some(employee), &subject).map_err(|_| USERS_UNAVAILABLE.to_owned())?;
    let matches = |email: &str| tenant_source::stored_email(email).ok().flatten().as_deref() == Some(subject.as_str());
    let employee_id = hex::encode(employee.0.0);
    let attested = db.attest_google_delegate(&AttestationRequest {
        agent_id: agent, employee_id: &employee_id, employee_incarnation: employee.1, subject: &subject,
        user_id: request.user_id, acknowledge_self_set: request.acknowledge_self_set,
    }, &matches, actor)?;
    Ok(json!({
        "agent_id": agent,
        "subject": subject,
        "user_id": attested.attestation.user_id,
        "attested_at": attested.attestation.attested_at,
        "employee_id": employee_id,
        "employee_incarnation": employee.1,
        "email_self_set": attested.email_self_set,
        "acknowledged_self_set": attested.email_self_set && request.acknowledge_self_set,
        "attestation": attestation_copy(&subject, &attested.display_name),
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
            (input.agent_id, Some((input.subject, input.user_id, input.acknowledge_self_set)), input.csrf_token)
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
                let Some((subject, user_id, acknowledge_self_set)) = subject else {
                    let revoked = db.revoke_google_delegate_attestation(&agent, &user).map_err(|_| FAILED.to_owned())?;
                    return Ok(json!({ "agent_id": agent, "revoked": revoked }));
                };
                // Shared employees of this installation only; the attestation
                // binds this live employee id and incarnation (SR4 round 3, N2).
                let employee = match crate::isolation_lifecycle::read_lifecycle(&home, &agent) {
                    Ok(lifecycle) if matches!(lifecycle.binding,
                        duduclaw_isolation::capability::EmployeeBinding::Shared(_)) =>
                        (lifecycle.employee, lifecycle.incarnation),
                    _ => return Err(NOT_SHARED.into()),
                };
                attest_subject(&home, &db, &Attest { agent: &agent, subject: &subject, user_id: &user_id,
                    acknowledge_self_set }, employee, &user)
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
    /// The live employee incarnation of `bot` in these fixtures.
    const EMP: tenant_source::Incarnation = (duduclaw_isolation::ids::EmployeeId([1; 16]), 0);
    fn emp_hex() -> String { hex::encode([1u8; 16]) }
    fn attest(home: &Path, db: &UserDb, subject: &str, user_id: &str) -> Result<Value, String> {
        attest_subject(home, db, &Attest { agent: "bot", subject, user_id, acknowledge_self_set: false }, EMP, "admin-id")
    }

    #[test]
    fn sr4_attest_subject_binds_the_configured_delegate_to_one_person() {
        let (home, db) = fixture();
        let alice = db.create_user("Alice@Customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        // Not a delegate employee (yet).
        assert_eq!(attest(home.path(), &db, "alice@customer.com", &alice.id).unwrap_err(), NOT_DELEGATE);
        delegate(home.path(), "alice@customer.com");
        // The subject must be exactly the configured one.
        assert_eq!(attest(home.path(), &db, "bob@customer.com", &alice.id).unwrap_err(), NOT_DELEGATE);
        let reply = attest(home.path(), &db, " ALICE@customer.com ", &alice.id).unwrap();
        assert_eq!((reply["user_id"].as_str(), reply["subject"].as_str()), (Some(alice.id.as_str()), Some("alice@customer.com")));
        assert_eq!(reply["attestation"], attestation_copy("alice@customer.com", "Alice"));
        assert_eq!((reply["employee_id"].as_str(), reply["employee_incarnation"].as_u64()), (Some(emp_hex().as_str()), Some(0)));
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("alice@customer.com", &alice.id, &emp_hex(), 0));
        // The account changes its email: revoked at use time.
        db.change_own_email(&alice.id, "alice2@customer.com").unwrap();
        db.change_own_email(&alice.id, "Alice@Customer.com").unwrap();
        assert!(!db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id, &emp_hex(), 0));
        // A designated service mailbox is never a person's.
        delegate(home.path(), "support@customer.com");
        assert_eq!(attest(home.path(), &db, "support@customer.com", &alice.id).unwrap_err(), IS_SERVICE_MAILBOX);
        // Nobody holds the address: nothing to attest.
        delegate(home.path(), "ceo@customer.com");
        assert!(attest(home.path(), &db, "ceo@customer.com", &alice.id).is_err());
        // An address that cannot be compared refuses.
        db.create_user("ceo@customer.com.", "Odd", "password1", UserRole::Employee).unwrap();
        assert_eq!(attest(home.path(), &db, "ceo@customer.com", &alice.id).unwrap_err(), USERS_UNAVAILABLE);
    }

    /// SR4 round 3 (N1): the request names the holder the Admin was shown,
    /// and nothing else; the acknowledgement is an optional boolean.
    #[test]
    fn sr4r3_attestation_request_requires_the_shown_holder() {
        let ok = json!({"agent_id": "bot", "subject": "alice@customer.com", "user_id": "alice-id",
            "csrf_token": vec![7u8; 32]});
        let parsed = serde_json::from_value::<AttestRequest>(ok.clone()).unwrap();
        assert_eq!((parsed.user_id.as_str(), parsed.acknowledge_self_set), ("alice-id", false));
        let mut missing = ok.clone();
        missing.as_object_mut().unwrap().remove("user_id");
        assert!(serde_json::from_value::<AttestRequest>(missing).is_err(), "user_id is required");
        let mut acknowledged = ok.clone();
        acknowledged["acknowledge_self_set"] = json!(true);
        assert!(serde_json::from_value::<AttestRequest>(acknowledged).unwrap().acknowledge_self_set);
        let mut loose = ok.clone();
        loose["acknowledge_self_set"] = json!("yes");
        assert!(serde_json::from_value::<AttestRequest>(loose).is_err(), "a boolean only");
        for extra in ["admin", "step_up_valid", "email", "employee_id", "holder"] {
            let mut forged = ok.clone();
            forged[extra] = json!("x");
            assert!(serde_json::from_value::<AttestRequest>(forged).is_err(), "{extra}");
        }
        assert!(serde_json::from_value::<RevokeRequest>(json!({"agent_id": "bot", "csrf_token": vec![7u8; 32]})).is_ok());
    }

    /// SR4 round 3 (N1): another holder than the one shown refuses
    /// (`attest_holder_mismatch`); a self-set address needs the explicit
    /// acknowledgement, which the reply (and so the audit event) carries.
    #[test]
    fn sr4r3_attest_subject_refuses_unlisted_holder_and_unacknowledged_self_set() {
        let (home, db) = fixture();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let mallory = db.create_user("mallory@customer.com", "Mallory", "password1", UserRole::Employee).unwrap();
        delegate(home.path(), "alice@customer.com");
        assert_eq!(attest(home.path(), &db, "alice@customer.com", &mallory.id).unwrap_err(),
            duduclaw_auth::google_delegate_attestation::HOLDER_MISMATCH);
        assert_eq!(attest(home.path(), &db, "alice@customer.com", " ").unwrap_err(), REQUEST_INVALID);
        assert!(db.google_delegate_attestation("bot").unwrap().is_none());
        // Mallory set her own login email to the CEO's address.
        db.change_own_email(&mallory.id, "ceo@customer.com").unwrap();
        delegate(home.path(), "ceo@customer.com");
        assert_eq!(attest(home.path(), &db, "ceo@customer.com", &mallory.id).unwrap_err(),
            duduclaw_auth::google_delegate_attestation::SELF_SET_UNACKNOWLEDGED);
        let reply = attest_subject(home.path(), &db, &Attest { agent: "bot", subject: "ceo@customer.com",
            user_id: &mallory.id, acknowledge_self_set: true }, EMP, "admin-id").unwrap();
        assert_eq!((reply["email_self_set"].as_bool(), reply["acknowledged_self_set"].as_bool()), (Some(true), Some(true)));
        assert!(reply["attestation"].as_str().unwrap().contains("「Mallory」"), "the copy names the listed person");
        // A different employee incarnation is not bound by it.
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("ceo@customer.com", &mallory.id, &emp_hex(), 0));
        assert!(!state.binds("ceo@customer.com", &mallory.id, &emp_hex(), 1));
        let _ = alice;
    }

    #[tokio::test]
    async fn sr4_attestation_refuses_non_admin_and_system() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let params = json!({"agent_id": "bot", "subject": "alice@customer.com", "user_id": "alice-id",
            "csrf_token": vec![7u8; 32]});
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
