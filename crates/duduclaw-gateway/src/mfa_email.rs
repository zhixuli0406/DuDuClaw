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
        else if !cfg!(any(unix, windows)) { Err("no_smtp_no_relay") }
        else { crate::license_runtime::mfa_relay_license(home).map(|_| ()) };
    availability_reason(smtp, enabled, eligibility)
}

pub(crate) fn available(home: &Path) -> bool { unavailable_reason(home).is_none() }

mod transport;
mod store;
mod relay;

async fn send_relay(home: &Path, email: &str, code: &str) -> Result<(), &'static str> {
    if !relay_enabled(home) { return Err(DELIVERY_FAILED); }
    let home = home.to_path_buf();
    tokio::time::timeout(transport::TIMEOUT, async {
        let (store, license, fingerprint) = tokio::task::spawn_blocking(move || {
            let (license, fingerprint) = crate::license_runtime::mfa_relay_license(&home)
                .map_err(|_| DELIVERY_FAILED)?;
            let store = store::open_wait(&home, transport::TIMEOUT).map_err(|_| DELIVERY_FAILED)?;
            Ok::<_, &'static str>((store, license, fingerprint))
        }).await.map_err(|_| DELIVERY_FAILED)??;
        relay::deliver(&store, license, &fingerprint, email, code,
            chrono::Utc::now().timestamp(), transport::exchange).await
    }).await.map_err(|_| DELIVERY_FAILED)?
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

#[cfg(test)]
#[path = "mfa_email/tests.rs"]
mod tests;
