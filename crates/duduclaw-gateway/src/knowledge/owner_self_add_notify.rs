//! Source-keyed notification outbox, committed with each new auto-import.
//! Started with the lock-holding knowledge worker; drains on wake and every 30 s.
//! Failures/no verified destination retain the row for retry; a lease prevents
//! concurrent drainers from sending it together. Sent rows stay for
//! deduplication. Like the existing push outboxes, provider delivery is at-least-
//! once across a crash between sending and acknowledging (no provider-wide
//! idempotency key exists). No document body or preview is stored here.
use std::path::Path;

use duduclaw_knowledge::{KbResult, schema, sources::Source};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(super) fn enqueue(tx: &Transaction<'_>, source: &Source, owner: &str) -> KbResult<()> {
    tx.execute("INSERT INTO owner_self_add_notices(source_id, owner_id, title) VALUES(?1, ?2, ?3)
        ON CONFLICT(source_id) DO NOTHING", params![source.id, owner, source.title])?;
    Ok(())
}

pub(super) struct Notice {
    pub source_id: String,
    pub owner_id: String,
    pub title: String,
    pub attempt: i64,
}

/// Claim atomically, including recovery of a lease left by a stopped gateway.
pub(super) fn claim(conn: &Connection, now: i64) -> KbResult<Option<Notice>> {
    Ok(conn.query_row("UPDATE owner_self_add_notices SET attempts=attempts+1, next_attempt_at=?1+300
        WHERE source_id=(SELECT source_id FROM owner_self_add_notices
            WHERE delivered=0 AND next_attempt_at<=?1 ORDER BY next_attempt_at, source_id LIMIT 1)
        RETURNING source_id, owner_id, title, attempts", [now], |r| Ok(Notice {
            source_id: r.get(0)?, owner_id: r.get(1)?, title: r.get(2)?, attempt: r.get(3)?,
        })).optional()?)
}

pub(super) fn finish(conn: &Connection, notice: &Notice) -> KbResult<()> {
    conn.execute("UPDATE owner_self_add_notices SET delivered=1 WHERE source_id=?1 AND attempts=?2",
        params![notice.source_id, notice.attempt])?;
    Ok(())
}

pub(super) fn body(notice: &Notice, channel: &str) -> String {
    format!("你的 AI 已免核准將「{}」加入知識庫。來源：{}",
        crate::markdown_render::untrusted_preview(channel, &notice.title, 120), notice.source_id)
}

pub(super) fn targets(home: &Path, owner_id: &str) -> Vec<(String, String)> {
    let Ok(Some(db)) = crate::decision_notify::open_user_db_read_only(home) else { return vec![]; };
    let Ok(Some(user)) = db.get_user(owner_id) else { return vec![]; };
    if user.status != duduclaw_auth::UserStatus::Active { return vec![]; }
    let Ok(links) = db.verified_dm_destinations_for_user(owner_id) else { return vec![]; };
    let mut targets = links.into_iter().filter(|d|
        super::add_source_chat::private_destination(&d.channel, &d.channel_user_id))
        .map(|d| (d.channel, d.channel_user_id)).collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
}

async fn deliver(home: &Path, notice: &Notice) -> bool {
    let quiet = crate::notify_governance::QuietPolicy {
        window: crate::notify_governance::load_global_window(home),
        tz: crate::notify_governance::NotifyTz::System,
    };
    // Keep quiet-hour notices in this outbox, not the unbound plain queue:
    // recipient links and active status must be read again at actual delivery.
    if quiet.decide(crate::notify_governance::NotifyLevel::Confirm, chrono::Utc::now()).is_some() {
        return false;
    }
    let targets = targets(home, &notice.owner_id);
    // One notice per source, to the first reachable verified Owner DM. Never
    // substitute an Admin, an employee destination, or the originating group.
    for (channel, destination) in targets {
        let receipt = crate::notify_push::push_plain_notice_now(home, "knowledge.owner_self_add",
            &body(notice, &channel), vec![(channel, destination)]).await;
        if receipt.outcome != crate::decision_notify::DeliverOutcome::Failed { return true; }
    }
    false
}

pub(super) async fn drain(home: &Path) {
    for _ in 0..10 {
        let h = home.to_path_buf();
        let notice = super::blocking(move || {
            claim(&schema::open_home(&h)?, chrono::Utc::now().timestamp())
        }).await;
        let notice = match notice {
            Ok(Some(notice)) => notice,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "knowledge: self-add notice claim failed");
                break;
            }
        };
        // Bound the send below the lease. A failed/aborted attempt remains due
        // after the lease; switching off imports does not discard owed notices.
        if tokio::time::timeout(std::time::Duration::from_secs(45), deliver(home, &notice))
            .await.unwrap_or(false) {
            let h = home.to_path_buf();
            if let Err(e) = super::blocking(move || finish(&schema::open_home(&h)?, &notice)).await {
                tracing::warn!(error = %e, "knowledge: self-add notice acknowledgement failed");
            }
        }
    }
}

fn wake() -> &'static tokio::sync::Notify {
    static WAKE: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    WAKE.get_or_init(tokio::sync::Notify::new)
}

pub(super) fn notify() {
    wake().notify_one();
}

/// Separate from ingestion: an unreachable channel must not stall source jobs.
/// Called only with the gateway instance lock, alongside the knowledge worker.
pub(super) fn spawn(home: std::path::PathBuf) {
    tokio::spawn(async move {
        loop {
            drain(&home).await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(30), wake().notified()).await;
        }
    });
}
