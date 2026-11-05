//! System SMTP delivery for account security, separate from employee mail approval.
use std::path::Path;
use duduclaw_auth::UserDb;

pub(crate) const UNAVAILABLE: &str = "Email 驗證碼目前無法使用，請管理員設定系統寄件伺服器與寄件地址。";

pub(crate) fn available(home: &Path) -> bool {
    crate::mail_worker::resolve_smtp_config(home).is_some()
}

pub(crate) async fn send_code(home: &Path, db: &UserDb, token: &str, email: &str, code: &str) -> Result<(),String> {
    let config=crate::mail_worker::resolve_smtp_config(home).ok_or(UNAVAILABLE)?;
    if crate::email::send_email(&config,email,"DuDuClaw Email 驗證碼",
        &format!("您的兩步驟驗證碼為：{code}\n\n此驗證碼於 10 分鐘後失效，且只能使用一次。若您未要求此驗證碼，請忽略這封信。"))
        .await.is_err() {
        db.mfa_email_delivery_failed(token,code)?;
        return Err("驗證碼無法寄出，請稍後再試或聯絡管理員。".into());
    }
    Ok(())
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
