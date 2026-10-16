//! Push notices to every Admin (design §6.1, M8, R2-L10).
//!
//! Plain text on each Admin's verified channel links, through the same DM
//! token candidates and sender the install sign-off uses
//! (`install_notify::send_plain_text`). Not `notify_push::push`: that path
//! renders decision buttons and defers cards during quiet hours, and a
//! maintenance notice is neither a decision nor something to hold back (its
//! whole value is reaching the Admin while the window is open).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use duduclaw_auth::{UserDb, UserRole, UserStatus};
use tracing::{info, warn};

mod fanout;

/// Per-request timeout of one notice send.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Includes token lookup and every fallback attempt for one verified link.
/// This stays below the lifecycle's 30 s overall safety limit.
const TARGET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Where maintenance notices go.
#[async_trait]
pub trait AdminNotifier: Send + Sync {
    /// Send `text` to every active Admin; returns how many sends succeeded.
    async fn notify_admins(&self, text: &str) -> usize;
    /// Whether at least one active Admin has a verified channel link with a
    /// configured bot token, i.e. whether a notice can reach anyone. The
    /// dashboard switches the fingerprint-check copy on this (R2-L10).
    async fn channel_available(&self) -> bool;
}

/// No user database (or a test): nothing is ever delivered.
pub struct NoNotifier;

#[async_trait]
impl AdminNotifier for NoNotifier {
    async fn notify_admins(&self, _text: &str) -> usize {
        0
    }
    async fn channel_available(&self) -> bool {
        false
    }
}

/// The real notifier over the dashboard user database.
pub struct UserDbAdminNotifier {
    home_dir: PathBuf,
    db: Arc<UserDb>,
}

impl UserDbAdminNotifier {
    pub fn new(home_dir: PathBuf, db: Arc<UserDb>) -> Self {
        Self { home_dir, db }
    }

    /// `(channel, chat_id)` of every active Admin's verified links.
    fn admin_targets(&self) -> Vec<(String, String)> {
        let users = match self.db.list_users() {
            Ok(u) => u,
            Err(e) => {
                warn!(error = %e, "maintenance notice: cannot list users");
                return Vec::new();
            }
        };
        let mut targets = Vec::new();
        for admin in users
            .iter()
            .filter(|u| u.role == UserRole::Admin && u.status == UserStatus::Active)
        {
            match self.db.verified_channels_for_user(&admin.id) {
                Ok(links) => {
                    targets.extend(links.into_iter().map(|l| (l.channel, l.channel_user_id)))
                }
                Err(e) => {
                    warn!(user = %admin.id, error = %e, "maintenance notice: channel lookup failed")
                }
            }
        }
        targets
    }
}

#[async_trait]
impl AdminNotifier for UserDbAdminNotifier {
    async fn notify_admins(&self, text: &str) -> usize {
        // A hung channel must not hold up the caller (the supervisor's tick).
        let http = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .build()
            .unwrap_or_default();
        let delivered = fanout::send_to_targets(
            self.admin_targets(),
            TARGET_TIMEOUT,
            |(channel, chat_id)| {
                let http = &http;
                async move {
                    let candidates =
                        crate::config_crypto::channel_dm_token_candidates(&self.home_dir, &channel)
                            .await;
                    for token in &candidates {
                        if crate::channel_sender::send_plain_text(
                            &self.home_dir,
                            http,
                            &channel,
                            token,
                            &chat_id,
                            text,
                        )
                        .await
                        {
                            return true;
                        }
                    }
                    warn!(%channel, "maintenance notice: not delivered on this link");
                    false
                }
            },
        )
        .await;
        info!(delivered, "maintenance notice pushed to admins");
        delivered
    }

    async fn channel_available(&self) -> bool {
        for (channel, _) in self.admin_targets() {
            if !crate::config_crypto::channel_dm_token_candidates(&self.home_dir, &channel)
                .await
                .is_empty()
            {
                return true;
            }
        }
        false
    }
}

// ── Notice texts (zh-TW, like every other channel notice) ──

fn local_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

/// Sent when an SSH window opened: both fingerprints, so the Admin and the
/// technician can check them over a channel a same-network attacker cannot
/// rewrite (M8).
pub fn opened_text(
    opened_by: &str,
    expires_at: &str,
    key_fingerprint: &str,
    key_label: Option<&str>,
    host_key_fingerprints: &[String],
) -> String {
    let label = key_label
        .filter(|l| !l.is_empty())
        .map(|l| format!("（{l}）"))
        .unwrap_or_default();
    let host = if host_key_fingerprints.is_empty() {
        "（系統沒有回報）".to_string()
    } else {
        host_key_fingerprints.join("、")
    };
    format!(
        "維修模式已開啟遠端技術支援連線\n\
         開啟者：{opened_by}\n\
         到期：{expires}\n\
         技術人員的金鑰指紋：{key_fingerprint}{label}\n\
         本機指紋：{host}\n\
         請和技術人員核對這兩個指紋。如果不是你開啟的，請立刻到儀表板關閉維修模式。",
        expires = local_time(expires_at),
    )
}

/// Sent when a close could not be confirmed.
pub fn close_unconfirmed_text(window_id: &str) -> String {
    format!(
        "無法確認遠端連線已關閉：裝置的系統服務沒有回應或回報了問題。\
         請重新開機，或聯繫技術支援。（視窗 {}）",
        duduclaw_core::truncate_chars(window_id, 8)
    )
}

/// Sent when boot reconciliation could not reach sysd at all.
pub fn reconcile_unreachable_text() -> String {
    "閘道重新啟動後連不到裝置的系統服務，無法確認遠端連線都已關閉。請重新開機，或聯繫技術支援。"
        .to_string()
}

/// Sent when an SSH key was found installed without an open window here.
pub fn orphan_closed_text(close_ok: bool) -> String {
    if close_ok {
        "發現一個沒有對應維修視窗的遠端登入金鑰，已經關閉並移除。".to_string()
    } else {
        "發現一個沒有對應維修視窗的遠端登入金鑰，但無法確認已關閉。請重新開機，或聯繫技術支援。"
            .to_string()
    }
}

/// Sent when the fifth wrong password locked opening maintenance mode.
pub fn reauth_locked_text(email: &str, minutes: i64) -> String {
    format!(
        "帳號 {email} 開啟維修模式時連續輸入錯誤密碼，{minutes} 分鐘內無法再開啟維修模式。\
         如果不是你本人操作，請檢查帳號安全。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_text_carries_both_fingerprints_and_the_opener() {
        let text = opened_text(
            "admin@local",
            "2026-10-16T10:00:00+00:00",
            "SHA256:key",
            Some("tech@laptop"),
            &["SHA256:host".to_string()],
        );
        assert!(text.contains("SHA256:key"));
        assert!(text.contains("tech@laptop"));
        assert!(text.contains("SHA256:host"));
        assert!(text.contains("admin@local"));
    }

    #[tokio::test]
    async fn no_notifier_reports_no_channel() {
        assert!(!NoNotifier.channel_available().await);
        assert_eq!(NoNotifier.notify_admins("x").await, 0);
    }
}
