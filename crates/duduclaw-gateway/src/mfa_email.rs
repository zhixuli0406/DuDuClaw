//! System security mail. SMTP takes precedence over the licensed code-only relay.
use std::path::Path;
use duduclaw_auth::UserDb;

pub(crate) const UNAVAILABLE: &str = "伺服器未設定寄信，且此安裝沒有可用的授權代寄。請管理員設定 SMTP 寄件伺服器與寄件地址。";
const DELIVERY_FAILED: &str = "驗證碼無法寄出，請稍後再試或聯絡管理員。";
pub(crate) const RATE_LIMITED: &str = "寄送太頻繁，請稍後再試。";

fn relay_enabled(home: &Path) -> bool {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    relay_config_enabled(&text)
}

fn relay_config_enabled(text: &str) -> bool {
    let Ok(config) = text.parse::<toml::Value>() else { return false; };
    match config.get("mfa") {
        None => true,
        Some(mfa) if mfa.is_table() => match mfa.get("email_relay") {
            None => true,
            Some(value) => value.as_bool() == Some(true),
        },
        _ => false,
    }
}

fn availability_reason(smtp: bool, relay_on: bool, eligibility: Result<(), &'static str>) -> Option<&'static str> {
    if smtp { None }
    else if !relay_on { Some("relay_disabled") }
    else { eligibility.err() }
}

pub(crate) fn unavailable_reason(home: &Path) -> Option<&'static str> {
    let smtp = crate::mail_worker::resolve_smtp_config(home).is_some();
    let enabled = relay_enabled(home);
    let eligibility = if smtp || !enabled { Ok(()) }
        else if !cfg!(any(unix, windows)) { Err("no_smtp") }
        else { crate::license_runtime::mfa_relay_license(home).map(|_| ()) };
    availability_reason(smtp, enabled, eligibility)
}

pub(crate) fn available(home: &Path) -> bool { unavailable_reason(home).is_none() }

mod transport;
mod store;
mod relay;

/// Resolve an eligible, key-bound relay license BEFORE touching the relay
/// store. Unbound or mismatched installation keys never reach the network
/// and never consume the enrollment budget.
fn prepare_relay(home: &Path,
    resolve: impl FnOnce(&Path) -> Result<crate::license_runtime::RelayLicense, &'static str>)
    -> Result<(store::Store, crate::license_runtime::RelayLicense), &'static str> {
    let relay = resolve(home).map_err(|_| DELIVERY_FAILED)?;
    let store = store::open_wait(home, transport::TIMEOUT).map_err(|_| DELIVERY_FAILED)?;
    Ok((store, relay))
}

async fn send_relay(home: &Path, email: &str, code: &str) -> Result<(), &'static str> {
    if !relay_enabled(home) { return Err(DELIVERY_FAILED); }
    let home = home.to_path_buf();
    tokio::time::timeout(transport::TIMEOUT, async {
        let (store, relay) = tokio::task::spawn_blocking(move ||
            prepare_relay(&home, crate::license_runtime::mfa_relay_license))
            .await.map_err(|_| DELIVERY_FAILED)??;
        relay::deliver(&store, &relay, email, code,
            chrono::Utc::now().timestamp(), transport::exchange).await
    }).await.map_err(|_| DELIVERY_FAILED)?
}

/// Inline (no blocking pool) variant so tests can inject the license resolver
/// and a recording exchange.
#[cfg(test)]
pub(crate) async fn relay_send_for_test<R, F, Fut>(home: &Path, email: &str, code: &str, now: i64,
    resolve: R, exchange: F) -> Result<(), &'static str>
where R: FnOnce(&Path) -> Result<crate::license_runtime::RelayLicense, &'static str>,
      F: FnMut(transport::Call) -> Fut, Fut: std::future::Future<Output = Result<transport::Reply, &'static str>> {
    let (store, relay) = prepare_relay(home, resolve)?;
    relay::deliver(&store, &relay, email, code, now, exchange).await
}

fn recipient_digest(home: &Path, email: &str) -> String {
    crate::trigger_route::access::keyed_digest_full(home, "mfa-relay-recipient", &email.trim().to_lowercase())
}

fn finish_delivery(home: &Path, db: &UserDb, token: &str, email: &str, code: &str, result: Result<(), &'static str>) -> Result<(), String> {
    if let Err(message) = result {
        // Keyed, domain-separated recipient digest; no unkeyed fallback.
        tracing::warn!(recipient_hash = %recipient_digest(home, email),
            outcome = if message == RATE_LIMITED { "rate_limited" } else { "delivery_failed" }, "MFA email delivery failed");
        db.mfa_email_delivery_failed(token, code)?;
        return Err(message.into());
    }
    Ok(())
}

pub(crate) async fn send_code(home: &Path, db: &UserDb, token: &str, email: &str, code: &str) -> Result<(), String> {
    let result = if let Some(config) = crate::mail_worker::resolve_smtp_config(home) {
        crate::email::send_email(&config, email, "DuDuClaw Email 驗證碼",
            &format!("您的兩步驟驗證碼為：{code}\n\n此驗證碼於 10 分鐘後失效，且只能使用一次。若您未要求此驗證碼，請忽略這封信。"))
            .await.map_err(|_| DELIVERY_FAILED)
    } else { send_relay(home, email, code).await };
    finish_delivery(home, db, token, email, code, result)
}

/// A notification failure does not undo an already committed security change.
/// Record that failure without leaking the destination or SMTP credentials.
pub(crate) fn notify_change(home: &Path, db: &std::sync::Arc<UserDb>, user: &str, action: &str) {
    let home=home.to_path_buf();
    let db=db.clone();
    let user=user.to_owned();
    let action=action.to_owned();
    // Deliver after returning the committed replacement session. SMTP must not
    // hold the only copy of recovery codes past the dashboard RPC timeout.
    tokio::spawn(async move {
    let Some(config)=crate::mail_worker::resolve_smtp_config(&home) else {return;};
    let Ok(Some(account))=db.get_user(&user) else {return;};
    let body=format!("您的 DuDuClaw 帳號兩步驟驗證設定已變更：{action}。\n\n若這不是您本人操作，請立即變更密碼並聯絡管理員。");
    if crate::email::send_email(&config,&account.email,"DuDuClaw 帳號安全通知",&body).await.is_err() {
        let _=db.log_action(Some(&user),"mfa.notification_failed",Some(&user),None,None);
        tracing::warn!("MFA security notification delivery failed");
    }
    });
}

/// RM-136: deliver the actor's own reset confirmation code (passkey-only or
/// email-only Administrators). SMTP sends an explanatory message; otherwise
/// the licensed relay sends its code-only message. A failure drops the code.
pub(crate) async fn send_reset_code(home: &Path, db: &UserDb, actor: &str, mail: &duduclaw_auth::mfa::AdminResetEmail) -> Result<(), String> {
    let result = if let Some(config) = crate::mail_worker::resolve_smtp_config(home) {
        crate::email::send_email(&config, &mail.email, "DuDuClaw 重設兩步驟驗證確認碼",
            &format!("你正在重設另一位成員的兩步驟驗證。確認碼：{}\n\n此確認碼 5 分鐘內有效，且只能使用一次。若這不是你本人操作，請立即變更密碼並聯絡其他管理員。", mail.code))
            .await.map_err(|_| DELIVERY_FAILED)
    } else { send_relay(home, &mail.email, &mail.code).await };
    if let Err(message) = result {
        tracing::warn!(recipient_hash = %recipient_digest(home, &mail.email), "MFA reset confirmation delivery failed");
        db.mfa_admin_reset_email_discard(actor, &mail.reset_token)?;
        return Err(message.into());
    }
    Ok(())
}

/// `auth_audit_log` action (and security audit event type) of a reset whose
/// notice reached the affected person by no route.
pub(crate) const RESET_NOTICE_FAILED: &str = "mfa.reset_notification_failed";

/// One delivery route of the reset notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeRoute {
    /// Sent now, or durably queued for quiet hours.
    Sent,
    /// Email: no SMTP server configured (the relay only carries codes).
    NotConfigured,
    /// No address: no account email / no verified channel link.
    NoTarget,
    /// Tried and failed (or the destinations could not be read).
    Failed,
}

impl NoticeRoute {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Sent => "sent", Self::NotConfigured => "not_configured", Self::NoTarget => "no_target", Self::Failed => "failed" }
    }
}

const RESET_NOTICE_TEXT: &str = "你的 DuDuClaw 帳號兩步驟驗證已由管理員重設。下次登入時需要重新設定。\n\n若你沒有提出這個要求，請立即聯絡其他管理員並變更密碼。";

/// RM-136: tell the affected person their second factor was reset, by email
/// (SMTP) **and** by their verified channel links. Returns each route's
/// outcome; never contains the address or a secret.
pub(crate) async fn deliver_reset_notice(home: &Path, db: &UserDb, target: &str) -> (NoticeRoute, NoticeRoute) {
    let account = match db.get_user(target) { Ok(Some(account)) => account, _ => return (NoticeRoute::NoTarget, NoticeRoute::NoTarget) };
    let email = match crate::mail_worker::resolve_smtp_config(home) {
        None => NoticeRoute::NotConfigured,
        Some(_) if account.email.trim().is_empty() => NoticeRoute::NoTarget,
        Some(config) => match crate::email::send_email(&config, &account.email, "DuDuClaw 帳號安全通知", RESET_NOTICE_TEXT).await {
            Ok(()) => NoticeRoute::Sent,
            Err(_) => NoticeRoute::Failed,
        },
    };
    let channel = match db.verified_channels_for_user(target) {
        Err(_) => NoticeRoute::Failed,
        Ok(links) if links.is_empty() => NoticeRoute::NoTarget,
        Ok(links) => {
            let targets = links.into_iter().take(16).map(|link| (link.channel, link.channel_user_id)).collect();
            let receipt = crate::notify_push::push_plain_notice(home, "mfa.reset", RESET_NOTICE_TEXT, targets).await;
            if receipt.delivered.is_some() { NoticeRoute::Sent } else { NoticeRoute::Failed }
        }
    };
    (email, channel)
}

/// When no route reached the person, leave a record an Administrator sees:
/// an `auth_audit_log` row (read by `mfa.overview`) and a hash-chained
/// security audit event. Neither carries an address, code or credential.
/// Returns whether a failure was recorded.
pub(crate) fn record_reset_notice(home: &Path, db: &UserDb, target: &str, email: NoticeRoute, channel: NoticeRoute) -> bool {
    if email == NoticeRoute::Sent || channel == NoticeRoute::Sent { return false; }
    let detail = format!("email={};channel={}", email.as_str(), channel.as_str());
    if db.log_action(Some(target), RESET_NOTICE_FAILED, Some(target), Some(&detail), None).is_err() {
        tracing::warn!("MFA reset notice failure could not be written to the account audit log");
    }
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        RESET_NOTICE_FAILED, "dashboard", duduclaw_security::audit::Severity::Warning,
        serde_json::json!({"target_user_id": target, "email": email.as_str(), "channel": channel.as_str()}),
    ));
    tracing::warn!(email = email.as_str(), channel = channel.as_str(), "MFA reset notification could not be delivered");
    true
}

/// Deliver after the committed reset returns; the reset is never undone.
pub(crate) fn notify_reset(home: &Path, db: &std::sync::Arc<UserDb>, target: &str) {
    let home = home.to_path_buf();
    let db = db.clone();
    let target = target.to_owned();
    tokio::spawn(async move {
        let (email, channel) = deliver_reset_notice(&home, &db, &target).await;
        record_reset_notice(&home, &db, &target, email, channel);
    });
}

/// Reset notices that reached nobody in the last 30 days, newest first, for
/// the Administrator overview. A read error is returned, never "none".
pub(crate) fn recent_reset_notice_failures(db: &UserDb) -> Result<Vec<serde_json::Value>, String> {
    let since = chrono::Utc::now() - chrono::Duration::days(30);
    Ok(db.query_audit_log(None, Some(RESET_NOTICE_FAILED), 50)?.into_iter()
        .filter(|entry| chrono::DateTime::parse_from_rfc3339(&entry.timestamp).is_ok_and(|at| at.with_timezone(&chrono::Utc) >= since))
        .filter_map(|entry| entry.target.or(entry.user_id).map(|user| serde_json::json!({"user_id": user, "at": entry.timestamp})))
        .collect())
}

#[cfg(test)]
#[path = "mfa_email/tests.rs"]
mod tests;

#[cfg(test)]
mod reset_notice_tests {
    use super::*;

    #[tokio::test]
    async fn reset_notice_without_smtp_or_channel_leaves_audit_records() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("person@example.test", "Person", "password-test", duduclaw_auth::UserRole::Admin).unwrap();
        let (email, channel) = deliver_reset_notice(home.path(), &db, &user.id).await;
        assert_eq!((email, channel), (NoticeRoute::NotConfigured, NoticeRoute::NoTarget));
        assert!(record_reset_notice(home.path(), &db, &user.id, email, channel));
        let rows = db.query_audit_log(Some(&user.id), Some(RESET_NOTICE_FAILED), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].detail.as_deref(), Some("email=not_configured;channel=no_target"));
        let events = duduclaw_security::audit::read_recent_events(home.path(), 10);
        let event = events.iter().find(|e| e.event_type == RESET_NOTICE_FAILED).expect("security audit event");
        let text = serde_json::to_string(&event.details).unwrap();
        assert!(!text.contains("person@example.test"), "no address in the audit event");
        let notices = recent_reset_notice_failures(&db).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["user_id"], serde_json::json!(user.id));
    }

    #[test]
    fn delivered_notice_records_nothing() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        assert!(!record_reset_notice(home.path(), &db, "user", NoticeRoute::Sent, NoticeRoute::NoTarget));
        assert!(!record_reset_notice(home.path(), &db, "user", NoticeRoute::NotConfigured, NoticeRoute::Sent));
        assert!(db.query_audit_log(None, Some(RESET_NOTICE_FAILED), 10).unwrap().is_empty());
        assert!(duduclaw_security::audit::read_recent_events(home.path(), 10).is_empty());
    }
}
