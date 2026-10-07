//! Making waiting LINE events visible (review I-HIGH-4 (2), F5 N4/N5).
//!
//! Every event that ends up needing a person is *recorded* in the inbox's
//! own `ingress_alert_queue` ([`record`], one cheap insert). The maintenance
//! task *flushes* the queue ([`flush`]): per kind and reason and per
//! ten-minute window, the first flush writes one Activity Feed row with the
//! count and the first few event ids, and pushes one notice through the
//! existing operator path (`goal_notify::notify_agent_plain`, the main AI
//! employee's `[proactive]` target); anything else in the same window is
//! summed into one follow-up row when the window closes. Window and push
//! keys live in `ingress_alerts`, so a restart neither repeats nor loses
//! them. Texts carry 12-character id prefixes, a state and a fixed reason
//! code only: never message text, a LINE user id or a token. They point at
//! the entries that exist today: the terminal `duduclaw ops channel-ingress`
//! and the dashboard's pending approvals (there is no inbox page yet).

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;

use super::IngressStore;

/// Why an alert was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AlertKind {
    /// The turn may have run and no receipt exists.
    Uncertain,
    /// Route/authority changed, could not be read, or payload expired.
    Quarantined,
    /// The turn ran but the answer was not delivered.
    Undelivered,
    /// `line_late_reply = "fail"` and the event was already late.
    LateReplyFailed,
    /// A conversation's messages have waited too long.
    Stuck,
    /// `channel_ingress.db` passed `capacity_alert_mb`.
    Capacity,
    /// The relay delivered a LINE webhook the gateway did not accept.
    RelayRejected,
    /// Waiting events from a restored backup were held for an operator.
    RestoredFromBackup,
}

const ALL: [AlertKind; 8] = [
    AlertKind::Uncertain,
    AlertKind::Quarantined,
    AlertKind::Undelivered,
    AlertKind::LateReplyFailed,
    AlertKind::Stuck,
    AlertKind::Capacity,
    AlertKind::RelayRejected,
    AlertKind::RestoredFromBackup,
];

/// Seconds per summary window.
pub(crate) const WINDOW_SECS: i64 = 600;
/// Event ids listed in one row.
const IDS_SHOWN: usize = 5;

impl AlertKind {
    pub(crate) fn event_type(self) -> &'static str {
        match self {
            Self::Uncertain => "channel_ingress_uncertain",
            Self::Quarantined => "channel_ingress_quarantined",
            Self::Undelivered => "channel_ingress_undelivered",
            Self::LateReplyFailed => "channel_ingress_late_reply_failed",
            Self::Stuck => "channel_ingress_stuck",
            Self::Capacity => "channel_ingress_capacity",
            Self::RelayRejected => "relay_line_rejected",
            Self::RestoredFromBackup => "channel_ingress_restored_held",
        }
    }

    fn from_event_type(s: &str) -> Option<Self> {
        ALL.into_iter().find(|k| k.event_type() == s)
    }

    fn headline(self) -> &'static str {
        match self {
            Self::Uncertain => "LINE 訊息結果不明：回合可能已執行但沒有送達回執，需要人工確認",
            Self::Quarantined => {
                "LINE 訊息已隔離：路由、授權或帳號已變更，或多次讀不到設定，沒有執行"
            }
            Self::Undelivered => "LINE 訊息已執行但回覆沒有送達",
            Self::LateReplyFailed => {
                "LINE 訊息逾期未送達：輪到它時回覆期限已過，依設定 line_late_reply = \"fail\" 沒有執行"
            }
            Self::Stuck => "LINE 對話卡住：有訊息等候過久",
            Self::Capacity => "LINE 收件匣資料庫超過容量告警值",
            Self::RelayRejected => "relay 轉來的 LINE webhook 沒有被收下，這些訊息不會重送",
            Self::RestoredFromBackup => {
                "從備份還原後，收件匣裡排隊中的 LINE 訊息已暫停（quarantined／restored_from_backup）：原裝置可能已經處理過它們，請檢視後結案，或確認重複風險後重新執行"
            }
        }
    }
}

/// Where an operator actually acts today (review N5).
const HOW_TO_ACT: &str = "處理：終端機 `duduclaw ops channel-ingress list` 或 `show <編號>` 檢視；要結案、重試或重新執行，用 `resolve`／`rerun`／`batch` 送出申請，再到儀表板的待辦清單核准（目前沒有收件匣頁面）。";

/// The text of one summary row.
pub(crate) fn alert_text(
    kind: AlertKind,
    reason: &str,
    count: usize,
    subjects: &[String],
) -> String {
    let ids = subjects
        .iter()
        .take(IDS_SHOWN)
        .map(|s| duduclaw_core::truncate_chars(s, 12).to_string())
        .collect::<Vec<_>>()
        .join("、");
    let more = if count > IDS_SHOWN {
        format!(" 等 {count} 則")
    } else {
        String::new()
    };
    format!(
        "{}。\n數量：{count}\n編號：{ids}{more}\n原因代碼：{}\n{HOW_TO_ACT}",
        kind.headline(),
        duduclaw_core::truncate_chars(reason, 48),
    )
}

/// Queue one alert. Cheap; never fails the caller (errors are logged).
pub(crate) async fn record(store: &IngressStore, kind: AlertKind, subject: &str, reason: &str) {
    record_at(store, kind, subject, reason, chrono::Utc::now().timestamp()).await;
}

/// [`record`] with an explicit timestamp, so tests can place alerts in a
/// known window instead of depending on the wall clock.
async fn record_at(store: &IngressStore, kind: AlertKind, subject: &str, reason: &str, now: i64) {
    let result = store.connection().lock().await.execute(
        "INSERT INTO ingress_alert_queue(kind,reason,subject,at) VALUES (?1,?2,?3,?4)",
        params![
            kind.event_type(),
            duduclaw_core::truncate_chars(reason, 48),
            duduclaw_core::truncate_chars(subject, 64),
            now
        ],
    );
    if let Err(e) = result {
        tracing::warn!(
            kind = kind.event_type(),
            "LINE ingress alert not queued: {e}"
        );
    }
}

/// Write one Activity row now, without the queue: for callers that may have
/// no inbox (the relay path when the store is unavailable). The caller
/// throttles. No push.
pub(crate) async fn raise_now(home: &Path, kind: AlertKind, subject: &str, reason: &str) {
    let subjects = [subject.to_string()];
    let text = alert_text(kind, reason, 1, &subjects);
    write_activity(home, None, kind, reason, &text, &subjects).await;
}

type Group = (String, String, i64);

/// Summarize queued alerts into the Activity Feed and push (see module doc).
pub(crate) async fn flush(store: &IngressStore, home: &Path, notify_agent: Option<&str>, now: i64) {
    let rows: Vec<(i64, String, String, String, i64)> = {
        let conn = store.connection().lock().await;
        let Ok(mut stmt) =
            conn.prepare("SELECT id,kind,reason,subject,at FROM ingress_alert_queue ORDER BY id")
        else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        }) else {
            return;
        };
        rows.filter_map(Result::ok).collect()
    };
    let mut groups: BTreeMap<Group, (Vec<i64>, Vec<String>)> = BTreeMap::new();
    for (id, kind, reason, subject, at) in rows {
        let g = groups.entry((kind, reason, at / WINDOW_SECS)).or_default();
        g.0.push(id);
        g.1.push(subject);
    }
    let current = now / WINDOW_SECS;
    for ((kind_s, reason, bucket), (ids, subjects)) in groups {
        let Some(kind) = AlertKind::from_event_type(&kind_s) else {
            drop_ids(store, &ids).await;
            continue;
        };
        let key = format!("activity:{kind_s}:{reason}:{bucket}");
        let first = store.claim_alert(&key, now).await.unwrap_or(false);
        if !first && bucket >= current {
            continue; // summed when the window closes
        }
        let mut text = alert_text(kind, &reason, subjects.len(), &subjects);
        if !first {
            text = format!("（同一時段的補充）{text}");
        }
        write_activity(home, notify_agent, kind, &reason, &text, &subjects).await;
        drop_ids(store, &ids).await;
        let push_key = format!("push:{kind_s}:{}", now / WINDOW_SECS);
        if first && store.claim_alert(&push_key, now).await.unwrap_or(false) {
            if let Some(agent) = notify_agent.filter(|a| !a.is_empty()) {
                let _ = crate::goal_notify::notify_agent_plain(
                    home,
                    agent,
                    crate::notify_governance::NotifyLevel::Act,
                    "channel_ingress",
                    &text,
                )
                .await;
            }
        }
    }
}

async fn drop_ids(store: &IngressStore, ids: &[i64]) {
    let conn = store.connection().lock().await;
    for id in ids {
        let _ = conn.execute("DELETE FROM ingress_alert_queue WHERE id=?1", [id]);
    }
}

async fn write_activity(
    home: &Path,
    notify_agent: Option<&str>,
    kind: AlertKind,
    reason: &str,
    text: &str,
    subjects: &[String],
) {
    tracing::warn!(
        kind = kind.event_type(),
        count = subjects.len(),
        "LINE ingress needs attention"
    );
    let Ok(store) = crate::task_store::TaskStore::open(home) else {
        return;
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: kind.event_type().to_string(),
        agent_id: notify_agent.unwrap_or("gateway").to_string(),
        task_id: None,
        summary: text.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: serde_json::to_string(&serde_json::json!({
            "reason": reason,
            "count": subjects.len(),
            "subjects": subjects.iter().take(50).collect::<Vec<_>>(),
        }))
        .ok(),
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!("channel ingress alert: activity append failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn activity(home: &Path, kind: AlertKind) -> Vec<crate::task_store::ActivityRow> {
        let store = crate::task_store::TaskStore::open(home).unwrap();
        store
            .list_activity(None, Some(kind.event_type()), 50, 0)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn a_burst_becomes_one_row_with_a_count_and_points_at_real_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = IngressStore::open(dir.path()).unwrap();
        // Every alert and flush below sits in one window. With the wall clock
        // a slow run could cross a ten-minute boundary between the batches
        // and turn the second batch into a new window's first row.
        let now = 3_000_000 * WINDOW_SECS + WINDOW_SECS / 2;
        for i in 0..500 {
            record_at(
                &store,
                AlertKind::Quarantined,
                &format!("{i:064x}"),
                "account_route_authorization_changed",
                now - 1,
            )
            .await;
        }
        flush(&store, dir.path(), None, now).await;
        let rows = activity(dir.path(), AlertKind::Quarantined).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].summary.contains("數量：500"), "{}", rows[0].summary);
        assert!(rows[0].summary.contains("duduclaw ops channel-ingress"));
        assert!(
            !rows[0].summary.contains("收件紀錄"),
            "no page that does not exist"
        );
        // More of the same in the same window wait for the window to close.
        for i in 0..3 {
            record_at(
                &store,
                AlertKind::Quarantined,
                &format!("{i:064x}"),
                "account_route_authorization_changed",
                now + 1,
            )
            .await;
        }
        flush(&store, dir.path(), None, now).await;
        assert_eq!(activity(dir.path(), AlertKind::Quarantined).await.len(), 1);
        // A restarted gateway (new store, same file) does not repeat the row.
        let reopened = IngressStore::open(dir.path()).unwrap();
        flush(&reopened, dir.path(), None, now).await;
        assert_eq!(activity(dir.path(), AlertKind::Quarantined).await.len(), 1);
        // After the window the remainder becomes one follow-up row.
        flush(&reopened, dir.path(), None, now + WINDOW_SECS).await;
        let rows = activity(dir.path(), AlertKind::Quarantined).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.summary.contains("數量：3")));
    }

    #[tokio::test]
    async fn push_throttle_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = IngressStore::open(dir.path()).unwrap();
        let now = 10 * WINDOW_SECS;
        assert!(
            store
                .claim_alert(&format!("push:x:{}", now / WINDOW_SECS), now)
                .await
                .unwrap()
        );
        let reopened = IngressStore::open(dir.path()).unwrap();
        assert!(
            !reopened
                .claim_alert(&format!("push:x:{}", now / WINDOW_SECS), now)
                .await
                .unwrap()
        );
    }
}
