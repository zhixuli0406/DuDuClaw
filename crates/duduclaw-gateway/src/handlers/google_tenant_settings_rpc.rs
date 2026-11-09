//! Google tenant-source settings for protected mode (native Linux isolation
//! v1.81, F5 UI-RPC; rule D-5 of the work breakdown, E4).
//!
//! `google.delegation.tenant_settings_set {service_mailboxes, tenant_scopes,
//! csrf_token}` writes `config.toml [integrations.google_identity]
//! service_mailboxes` and `tenant_scopes` — the two keys E4's tenant source
//! (`google_identity/tenant_source.rs`) reads — through the format-preserving
//! config writer (`config_commit::commit_table_locked_with_guard`); every
//! other key of the file and of the section is left as it is.
//!
//! Dashboard-only (never in the AI tool catalog), Enterprise-only, Admin
//! re-read from users.db, the `system` context refused, and the same
//! step-up as the isolation connector RPCs: the migration dashboard proof
//! (`isolation.connector_step_up` / `migration.confirmations.step_up`,
//! password + TOTP, five minutes, bound to this session and credential
//! epoch) and its CSRF token. The proof and the users.db email rule are
//! checked again inside the config file lock just before the write.
//!
//! Validation (fail closed, nothing is silently fixed):
//! - `service_mailboxes`: at most [`MAX_SERVICE_MAILBOXES`]; each normalized
//!   with the same `normalize_email` the tenant source and the D-5 subject
//!   rule use; no duplicates after normalization; none may equal (exact
//!   normalized equality) the email of ANY users.db account, whatever its
//!   status — a person's mailbox is never a service mailbox. SR5 item 4: nor
//!   any address an account EVER held (`account_email_history`: changed
//!   emails and removed accounts included).
//!
//! SR5 item 4 (authority): Admin re-read from users.db
//! (`mfa_require_current_admin`) + the migration step-up proof, which is only
//! issued after password + the second factor (TOTP) of an MFA-enrolled Admin
//! and is fresh for five minutes; an audit row is written inside the config
//! lock BEFORE the write (a failed audit refuses) and again after it.
//! Residual: an address that never belonged to a users.db account (a
//! colleague without a DuDuClaw account) cannot be recognised; designating it
//! still needs the Admin + MFA step-up and leaves both audit rows, and the
//! D-5 rule admits a designated mailbox for any actor by design.
//! - `tenant_scopes`: at most [`MAX_TENANT_SCOPES`]; each exactly one of the
//!   scopes the tenant mint requests (`google_workspace::REQUIRED_SCOPES`, the
//!   set `tenant_source::read` admits); no duplicates. Empty ⇒ the tenant
//!   source serves `google_status` only.
//!
//! A change alters every bridge's pinned tenant-source digest, so running
//! bridges refuse until they are rebuilt (E4). The reply and the audit carry
//! the saved lists only — no key material.
use super::*;

use duduclaw_core::google_identity as gi;
use duduclaw_security::audit::{AuditEvent, Severity};

use super::isolation_connectors_rpc::authorize_step_up;

pub(super) const METHOD: &str = "google.delegation.tenant_settings_set";
/// Upper bound of designated service mailboxes.
pub(crate) const MAX_SERVICE_MAILBOXES: usize = 64;
/// Upper bound of tenant scopes (never more than the minted set).
pub(crate) const MAX_TENANT_SCOPES: usize = crate::google_workspace::REQUIRED_SCOPES.len();

const NOT_ALLOWED: &str = "不允許此設定操作。";
const ADMIN_REQUIRED: &str = "請以管理員帳號重新登入後再操作。";
const NOT_PROTECTED: &str = "這個環境未啟用受保護模式；Google 租戶來源設定只在受保護模式使用。";
const USERS_UNAVAILABLE: &str = "無法讀取帳號資料，已拒絕儲存。";
const SESSION_EXPIRED: &str = "登入憑證已失效，請重新登入。";
const REQUEST_INVALID: &str = "設定內容不正確：請提供服務信箱清單、權限範圍清單與驗證權杖。";
const CONFIG_UNREADABLE: &str = "目前的 config.toml 或 Google 身分設定無法辨識，請先修正後再儲存。";
const WRITE_FAILED: &str = "無法寫入 config.toml，設定未變更。";
/// SR5 item 4: the pre-write audit row could not be written; nothing saved.
const AUDIT_FAILED: &str = "無法寫入稽核紀錄，已拒絕儲存。";
/// SR4 item 2(iii): a stored login email cannot be compared.
pub(crate) const EMAIL_UNRECOGNISED: &str =
    "帳號資料中有無法辨識的登入 email（現在或過去），無法確認服務信箱不屬於任何同仁，已拒絕儲存。請先修正該帳號的 email。";
/// SR4 item 2: what saving a service mailbox means (reply + docs).
pub(crate) const SERVICE_MAILBOX_ATTESTATION: &str =
    "儲存服務信箱即表示管理員確認：這些信箱不屬於任何個人（不是任何同仁的信箱），任何經授權的請求者都可透過共用員工使用。若之後有同仁把自己的登入 email 改成其中一個信箱，該信箱會停止使用並在設定頁顯示衝突。";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    service_mailboxes: Vec<String>,
    tenant_scopes: Vec<String>,
    csrf_token: [u8; 32],
}

/// The validated lists, in the order given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantSettings {
    pub(crate) service_mailboxes: Vec<String>,
    pub(crate) tenant_scopes: Vec<String>,
}

fn shown(value: &str) -> String { duduclaw_core::truncate_chars(value, 80).to_string() }

/// Every users.db account email, normalized like the subject rule
/// (`tenant_source::subject_accounts`, same [`stored_email`]).
///
/// SR5 item 4: plus every address any account EVER held
/// (`account_email_history`, trigger-written in the transaction of each
/// email change, seeded with the current addresses): a colleague's former
/// login address (changed email, removed account) is never designatable. An
/// unreadable history refuses.
///
/// SR4 item 2(iii): a stored address the normalizer rejects refuses the save
/// ([`EMAIL_UNRECOGNISED`]) instead of being skipped — unless it provably
/// names no mailbox (no `@`, or a dotless domain such as `admin@local`).
///
/// [`stored_email`]: crate::google_identity::tenant_source::stored_email
pub(crate) fn account_emails(db: &UserDb) -> Result<Vec<String>, &'static str> {
    let users = db.list_users().map_err(|_| USERS_UNAVAILABLE)?;
    let history = db.account_email_history().map_err(|_| USERS_UNAVAILABLE)?;
    let mut emails = Vec::new();
    for email in users.into_iter().map(|user| user.email).chain(history) {
        match crate::google_identity::tenant_source::stored_email(&email) {
            Ok(Some(email)) => emails.push(email),
            Ok(None) => {}
            Err(_) => return Err(EMAIL_UNRECOGNISED),
        }
    }
    emails.sort();
    emails.dedup();
    Ok(emails)
}

/// Pure validation of the requested lists against the users.db emails.
pub(crate) fn validate(mailboxes: &[String], scopes: &[String], account_emails: &[String])
    -> Result<TenantSettings, String> {
    if mailboxes.len() > MAX_SERVICE_MAILBOXES {
        return Err(format!("服務信箱最多 {MAX_SERVICE_MAILBOXES} 個。"));
    }
    if scopes.len() > MAX_TENANT_SCOPES {
        return Err(format!("權限範圍最多 {MAX_TENANT_SCOPES} 個。"));
    }
    let mut service_mailboxes: Vec<String> = Vec::with_capacity(mailboxes.len());
    for raw in mailboxes {
        let mailbox = gi::normalize_email(raw).map_err(|_| format!("「{}」不是有效的 email。", shown(raw)))?;
        if service_mailboxes.contains(&mailbox) {
            return Err(format!("服務信箱「{}」重複了。", shown(&mailbox)));
        }
        if account_emails.iter().any(|email| *email == mailbox) {
            return Err(format!("「{}」是同仁的登入帳號，不能設為服務信箱（同仁本人的信箱只能由本人透過委派使用）。",
                shown(&mailbox)));
        }
        service_mailboxes.push(mailbox);
    }
    let mut tenant_scopes: Vec<String> = Vec::with_capacity(scopes.len());
    for scope in scopes {
        // Exact equality with a minted scope: no trimming or case folding.
        if !crate::google_workspace::REQUIRED_SCOPES.contains(&scope.as_str()) {
            return Err(format!("「{}」不是可使用的 Google 權限範圍。", shown(scope)));
        }
        if tenant_scopes.contains(scope) {
            return Err(format!("權限範圍「{}」重複了。", shown(scope)));
        }
        tenant_scopes.push(scope.clone());
    }
    Ok(TenantSettings { service_mailboxes, tenant_scopes })
}

/// The config table with only the two keys replaced (empty list ⇒ key
/// removed, which the strict parser reads as empty). Refuses an unparsable
/// file or a `[integrations.google_identity]` section the strict parser
/// rejects: a broken section is never silently rewritten. Returns the new
/// table and the previous lists (for the audit).
pub(crate) fn apply_to_config(original: &str, settings: &TenantSettings)
    -> Result<(toml::Table, TenantSettings), &'static str> {
    let previous = gi::parse_home_config(original).map_err(|_| CONFIG_UNREADABLE)?;
    let mut table: toml::Table = original.parse().map_err(|_| CONFIG_UNREADABLE)?;
    let had_integrations = table.contains_key("integrations");
    let section = super::config_commit::table_at_mut(&mut table, &["integrations", gi::HOME_SECTION])
        .map_err(|_| CONFIG_UNREADABLE)?;
    for (key, values) in [("service_mailboxes", &settings.service_mailboxes), ("tenant_scopes", &settings.tenant_scopes)] {
        if values.is_empty() {
            section.remove(key);
        } else {
            section.insert(key.into(), toml::Value::Array(values.iter().cloned().map(toml::Value::String).collect()));
        }
    }
    if section.is_empty()
        && let Some(integrations) = table.get_mut("integrations").and_then(toml::Value::as_table_mut)
    {
        integrations.remove(gi::HOME_SECTION);
        // Never leave an empty `[integrations]` this call created.
        if integrations.is_empty() && !had_integrations { table.remove("integrations"); }
    }
    // The written section must read back exactly as validated.
    let rendered = toml::to_string(&table).map_err(|_| CONFIG_UNREADABLE)?;
    let readback = gi::parse_home_config(&rendered).map_err(|_| CONFIG_UNREADABLE)?;
    if readback.service_mailboxes != settings.service_mailboxes || readback.tenant_scopes != settings.tenant_scopes {
        return Err(CONFIG_UNREADABLE);
    }
    Ok((table, TenantSettings { service_mailboxes: previous.service_mailboxes, tenant_scopes: previous.tenant_scopes }))
}

/// Step-up for this exact user, session and epoch, read fresh from users.db.
/// Shared with the SR4 delegate-subject attestation RPC.
pub(super) fn check_step_up(db: &UserDb, user: &str, session: &str, epoch: u64, csrf: &[u8; 32]) -> Result<(), &'static str> {
    let now = chrono::Utc::now().timestamp();
    let proof = db.migration_confirmation_session(user, session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
    authorize_step_up(proof.as_ref(), user, csrf, now)
}

impl MethodHandler {
    /// `google.delegation.tenant_settings_set`.
    pub(crate) async fn handle_google_tenant_settings_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.google_tenant_settings_set(params, ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(message) => WsFrame::error_response("", &message),
        }
    }

    async fn google_tenant_settings_set(&self, params: Value, ctx: &UserContext) -> Result<Value, String> {
        if ctx.user_id == "system" || ctx.user_id.is_empty() { return Err(NOT_ALLOWED.into()); }
        if ctx.requires_password_change() { return Err(ADMIN_REQUIRED.into()); }
        if acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err() {
            return Err(ADMIN_REQUIRED.into());
        }
        let session = crate::google_personal::session_for(&ctx.user_id).map_err(|_| ADMIN_REQUIRED)?;
        if !crate::runtime_readiness::is_protected_linux() { return Err(NOT_PROTECTED.into()); }
        let db = self.user_db.read().await.clone().ok_or(USERS_UNAVAILABLE)?;
        let input: Request = serde_json::from_value(params).map_err(|_| REQUEST_INVALID)?;
        let (user, epoch, csrf) = (ctx.user_id.clone(), ctx.credential_epoch, input.csrf_token);
        let home = self.home_dir.clone();
        let config_path = home.join("config.toml");
        // Step-up, users.db emails, validation and the table edit on the
        // blocking pool (SQLite and file reads).
        let prepared = {
            let (db, user, session, path) = (db.clone(), user.clone(), session.clone(), config_path.clone());
            tokio::task::spawn_blocking(move || -> Result<_, String> {
                check_step_up(&db, &user, &session, epoch, &csrf)?;
                let settings = validate(&input.service_mailboxes, &input.tenant_scopes, &account_emails(&db)?)?;
                let original = super::config_commit::read_text_or_empty(&path).map_err(|_| CONFIG_UNREADABLE)?;
                let (table, previous) = apply_to_config(&original, &settings)?;
                Ok((settings, previous, table, super::config_commit::content_hash(&original)))
            }).await.map_err(|_| WRITE_FAILED)??
        };
        let (settings, previous, table, expected_hash) = prepared;
        // Inside the config lock: the proof is still fresh and no account
        // has since taken one of the mailboxes.
        // SR5 item 4: the audit row is written before the write, inside the
        // lock (a failed audit refuses), and again after the commit.
        let intent = json!({
            "phase": "intent",
            "before": { "service_mailboxes": previous.service_mailboxes, "tenant_scopes": previous.tenant_scopes },
            "after": { "service_mailboxes": settings.service_mailboxes, "tenant_scopes": settings.tenant_scopes },
        })
        .to_string();
        let guard = {
            let (db, user, session, mailboxes) = (db.clone(), user.clone(), session.clone(), settings.service_mailboxes.clone());
            move || -> Result<(), String> {
                check_step_up(&db, &user, &session, epoch, &csrf)?;
                let emails = account_emails(&db)?;
                if mailboxes.iter().any(|mailbox| emails.contains(mailbox)) {
                    return Err("服務信箱與同仁的登入帳號相同，已拒絕儲存。".into());
                }
                db.log_action(Some(&user), METHOD, None, Some(&intent), None).map_err(|_| AUDIT_FAILED.to_owned())?;
                Ok(())
            }
        };
        super::config_commit::commit_table_locked_with_guard(&config_path, expected_hash, &table, guard)
            .await.map_err(|error| if error.contains("同仁") || error.contains("驗證") || error.contains("登入") {
                error
            } else {
                WRITE_FAILED.to_owned()
            })?;
        let detail = json!({
            "before": { "service_mailboxes": previous.service_mailboxes, "tenant_scopes": previous.tenant_scopes },
            "after": { "service_mailboxes": settings.service_mailboxes, "tenant_scopes": settings.tenant_scopes },
        });
        let _ = db.log_action(Some(&user), METHOD, None, Some(&detail.to_string()), None);
        let mut audit = detail.clone();
        audit["actor_user_id"] = json!(user);
        audit["actor_email"] = json!(ctx.email);
        crate::security_autopilot::audit_and_emit(&home,
            &AuditEvent::new("google_tenant_settings_set", "dashboard", Severity::Warning, audit));
        Ok(json!({
            "service_mailboxes": settings.service_mailboxes,
            "tenant_scopes": settings.tenant_scopes,
            "status_only": settings.tenant_scopes.is_empty(),
            "readback_required": true,
            // SR4 item 2: designating a mailbox is the Admin's attestation.
            "attestation": SERVICE_MAILBOX_ATTESTATION,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GMAIL: &str = "https://www.googleapis.com/auth/gmail.readonly";
    const DRIVE: &str = "https://www.googleapis.com/auth/drive.readonly";

    fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|v| (*v).to_owned()).collect() }

    #[test]
    fn f5_tenant_settings_validation_is_strict() {
        let people = strings(&["alice@customer.com"]);
        let ok = validate(&strings(&[" Support@Customer.com ", "billing@customer.com"]), &strings(&[GMAIL, DRIVE]), &people)
            .unwrap();
        assert_eq!(ok.service_mailboxes, strings(&["support@customer.com", "billing@customer.com"]));
        assert_eq!(ok.tenant_scopes, strings(&[GMAIL, DRIVE]));
        // Empty is allowed: status only.
        assert_eq!(validate(&[], &[], &people).unwrap(), TenantSettings { service_mailboxes: vec![], tenant_scopes: vec![] });

        // A person's account, in any case/spacing, is never a service mailbox.
        for person in ["alice@customer.com", "ALICE@customer.com", " alice@Customer.COM"] {
            let error = validate(&strings(&[person]), &[], &people).unwrap_err();
            assert!(error.contains("同仁的登入帳號"), "{person}: {error}");
        }
        // Exact equality only: a look-alike is not a person's account.
        assert!(validate(&strings(&["alice@customer.com.evil.io", "xalice@customer.com"]), &[], &people).is_ok());
        // Duplicates after normalization, malformed emails, too many.
        assert!(validate(&strings(&["a@customer.com", "A@customer.com"]), &[], &people).unwrap_err().contains("重複"));
        for bad in ["", "not-an-email", "a@b", "a@@customer.com", "a b@customer.com"] {
            assert!(validate(&strings(&[bad]), &[], &people).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_SERVICE_MAILBOXES).map(|i| format!("box{i}@customer.com")).collect();
        assert!(validate(&many, &[], &people).is_err());
        assert!(validate(&many[..MAX_SERVICE_MAILBOXES], &[], &people).is_ok());

        // Scopes: exactly a minted scope; no trimming, no case folding, no dupes.
        for bad in ["https://www.googleapis.com/auth/admin.directory.user", " https://www.googleapis.com/auth/gmail.readonly",
            "HTTPS://WWW.GOOGLEAPIS.COM/AUTH/GMAIL.READONLY", "gmail.readonly", ""] {
            assert!(validate(&[], &strings(&[bad]), &people).is_err(), "{bad}");
        }
        assert!(validate(&[], &strings(&[GMAIL, GMAIL]), &people).unwrap_err().contains("重複"));
        let all: Vec<String> = crate::google_workspace::REQUIRED_SCOPES.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(validate(&[], &all, &people).unwrap().tenant_scopes, all);
    }

    #[test]
    fn f5_tenant_settings_request_carries_no_authority_claims() {
        let ok = json!({"service_mailboxes": [], "tenant_scopes": [], "csrf_token": vec![7u8; 32]});
        assert!(serde_json::from_value::<Request>(ok.clone()).is_ok());
        for extra in ["admin", "step_up_valid", "allowed_subject_domains", "default_identity", "tenant_id", "key_json"] {
            let mut forged = ok.clone();
            forged[extra] = json!(true);
            assert!(serde_json::from_value::<Request>(forged).is_err(), "{extra}");
        }
        for missing in ["service_mailboxes", "tenant_scopes", "csrf_token"] {
            let mut partial = ok.clone();
            partial.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<Request>(partial).is_err(), "{missing}");
        }
    }

    #[test]
    fn f5_tenant_settings_edit_only_the_two_keys() {
        let original = "# operator note\n[gateway]\nport = 1\n\n[integrations.google_identity]\n\
            allowed_subject_domains = [\"customer.com\"] # keep\nservice_mailboxes = [\"old@customer.com\"]\n";
        let settings = TenantSettings { service_mailboxes: strings(&["support@customer.com"]), tenant_scopes: strings(&[GMAIL]) };
        let (table, previous) = apply_to_config(original, &settings).unwrap();
        assert_eq!(previous.service_mailboxes, strings(&["old@customer.com"]));
        assert!(previous.tenant_scopes.is_empty());
        let rendered = super::super::config_commit::render_preserving(original, &table).unwrap();
        assert!(rendered.contains("# operator note") && rendered.contains("# keep"), "{rendered}");
        let parsed = gi::parse_home_config(&rendered).unwrap();
        assert_eq!(parsed.allowed_subject_domains, strings(&["customer.com"]));
        assert_eq!((&parsed.service_mailboxes, &parsed.tenant_scopes), (&settings.service_mailboxes, &settings.tenant_scopes));
        assert_eq!(rendered.parse::<toml::Table>().unwrap()["gateway"]["port"].as_integer(), Some(1));

        // Clearing both keys removes them; the rest of the section stays.
        let empty = TenantSettings { service_mailboxes: vec![], tenant_scopes: vec![] };
        let (table, _) = apply_to_config(&rendered, &empty).unwrap();
        let cleared = super::super::config_commit::render_preserving(&rendered, &table).unwrap();
        assert!(!cleared.contains("service_mailboxes") && !cleared.contains("tenant_scopes"), "{cleared}");
        assert!(cleared.contains("allowed_subject_domains"));
        // Nothing else configured: no empty section is left behind.
        let (table, _) = apply_to_config("", &empty).unwrap();
        assert!(table.is_empty(), "{table:?}");

        // A broken file or section is refused, never rewritten.
        for broken in ["not = [toml", "[integrations.google_identity]\nunknown_key = 1\n",
            "[integrations.google_identity]\nservice_mailboxes = \"x@customer.com\"\n", "integrations = 3\n"] {
            assert_eq!(apply_to_config(broken, &settings).err(), Some(CONFIG_UNREADABLE), "{broken}");
        }
    }

    #[test]
    fn f5_tenant_settings_account_emails_cover_every_status() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        db.create_user("Alice@Customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@customer.com", "Bob", "password1", UserRole::Employee).unwrap();
        db.set_user_status(&bob.id, duduclaw_auth::models::UserStatus::Suspended).unwrap();
        let emails = account_emails(&db).unwrap();
        assert!(emails.contains(&"alice@customer.com".to_owned()));
        // A suspended person is still a person: never a service mailbox.
        assert!(validate(&strings(&["bob@customer.com"]), &[], &emails).is_err());
    }

    #[test]
    fn sr5_a_mailbox_any_account_ever_held_is_refused() {
        use crate::google_identity::tenant_source::{admit_subject, subject_accounts, Actor, TenantSource, TenantSourceKind};
        let home = crate::fs_safe::private_test_home().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let carol = db.create_user("carol@customer.com", "Carol", "password1", UserRole::Employee).unwrap();
        // Alice changes her login address; Carol's account is removed.
        db.change_own_email(&alice.id, "alice.new@customer.com").unwrap();
        rusqlite::Connection::open(home.path().join("users.db")).unwrap()
            .execute("DELETE FROM users WHERE id=?1", [&carol.id]).unwrap();
        let emails = account_emails(&db).unwrap();
        for former in ["alice@customer.com", "ALICE@customer.com", "carol@customer.com"] {
            let error = validate(&strings(&[former]), &[], &emails).unwrap_err();
            assert!(error.contains("同仁的登入帳號"), "{former}: {error}");
        }
        assert!(validate(&strings(&["alice.new@customer.com"]), &[], &emails).is_err(), "the current address too");
        assert!(validate(&strings(&["support@customer.com"]), &[], &emails).is_ok());
        // Use time: a designated former login address is refused for every actor.
        for former in ["alice@customer.com", "carol@customer.com"] {
            let accounts = subject_accounts(&db, "bot", former).unwrap();
            assert_eq!(accounts.len(), 1, "{former}");
            assert!(!accounts[0].active && accounts[0].user_id.is_empty());
            let source = TenantSource::fixture(TenantSourceKind::Delegate, former, &[former], &[]);
            for actor in [Actor::System, Actor::Human(&alice.id)] {
                assert!(admit_subject(&source, actor, &accounts).is_err(), "{former}");
            }
        }
        assert!(subject_accounts(&db, "bot", "support@customer.com").unwrap().is_empty());
        // Every change of a login address is audited in its own transaction.
        let changed = db.query_audit_log(None, Some(duduclaw_auth::db::EMAIL_CHANGED_ACTION), 10).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].target.as_deref(), Some(alice.id.as_str()));
    }

    #[tokio::test]
    async fn f5_tenant_settings_refuse_non_admin_and_system() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let params = json!({"service_mailboxes": ["support@customer.com"], "tenant_scopes": [GMAIL], "csrf_token": vec![7u8; 32]});
        let system = UserContext::admin_fallback();
        let mut employee = UserContext::admin_fallback();
        employee.user_id = "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10".into();
        employee.role = UserRole::Employee;
        for caller in [&system, &employee] {
            let frame = handler.handle(METHOD, params.clone(), caller).await;
            assert!(!matches!(frame, WsFrame::Response { ok: true, .. }));
        }
        let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap_or_default();
        assert!(!config.contains("service_mailboxes") && !config.contains("tenant_scopes"), "nothing written: {config}");
    }
}
