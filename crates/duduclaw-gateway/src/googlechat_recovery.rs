//! Bounded, lock-holder-only recovery. Never POST a restart notice.
use std::{future::Future, path::Path, sync::Arc, time::Duration};
use crate::googlechat::{GoogleChatCreds, update_message};
use crate::googlechat_inbox::{Inbox, PendingPlaceholder};

const SWEEP_BUDGET: Duration = Duration::from_secs(30);
const REQUEST_BUDGET: Duration = Duration::from_secs(5);
const MAX_AGE: i64 = 24 * 60 * 60;

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts { attempted: usize, deleted: usize, failed: usize }

/// Called before any Google Chat transport starts accepting inbound messages.
pub(crate) async fn startup(home: &Path) {
    if !duduclaw_core::gateway_instance::held(home) { return; }
    // No channel state yet: do not create a database just for maintenance.
    if !home.join("googlechat_inbox.db").exists() { return; }
    let deadline = tokio::time::Instant::now() + SWEEP_BUDGET;
    let store = match Inbox::shared(home) {
        Ok(store) => store,
        Err(_) => { tracing::warn!(failed = 1, "Google Chat placeholder recovery failed"); return; }
    };
    let text = crate::trigger_route::texts::googlechat_interrupted(crate::trigger_route::texts::lang_for(home));
    let result = sweep(&store, chrono::Utc::now().timestamp(), deadline, |row| async move {
        if !duduclaw_core::gateway_instance::held(home) { return Err(None); }
        // Fail closed on a corrupt reference: only the recorded message's text
        // endpoint is eligible, never another resource under the API origin.
        let parts: Vec<_> = row.message_name.split('/').collect();
        if parts.len() != 4 || parts[0] != "spaces" || parts[2] != "messages"
            || parts[1].is_empty() || parts[3].is_empty()
            || row.space != format!("spaces/{}", parts[1])
            || ![parts[1], parts[3]].iter().all(|id| id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))) {
            return Err(None);
        }
        let _binding = crate::agent_channels::binding_lock().await;
        let creds = match row.app_agent.as_deref() {
            Some(id) => crate::googlechat_agents::agent_creds(home, id).await,
            None => GoogleChatCreds::from_config(home).await.map(Arc::new),
        }.ok_or(None)?;
        // Removing/rebinding an employee app must never edit with a different app.
        if creds.client_email() != row.app_email { return Err(None); }
        update_message(&creds, &row.message_name, text).await.map_err(|e| e.status)
    }).await;
    // Counts only: no content, identifiers, resource names or error bodies.
    match result {
        Ok(counts) if counts.attempted > 0 => tracing::warn!(attempted = counts.attempted, deleted = counts.deleted, failed = counts.failed,
            "Google Chat interrupted placeholder recovery"),
        Err(()) => tracing::warn!(failed = 1, "Google Chat placeholder recovery failed"),
        _ => {},
    }
}

/// Timeout includes credential resolution/token exchange as well as PATCH.
async fn sweep<F, Fut>(store: &Inbox, now: i64, deadline: tokio::time::Instant, mut edit: F) -> Result<Counts, ()>
where F: FnMut(PendingPlaceholder) -> Fut, Fut: Future<Output = Result<(), Option<u16>>> {
    let rows = store.stale_placeholders(now).map_err(|_| ())?;
    let mut counts = Counts::default();
    for row in rows {
        if tokio::time::Instant::now() >= deadline { break; }
        counts.attempted += 1;
        let request_deadline = deadline.min(tokio::time::Instant::now() + REQUEST_BUDGET);
        let result = tokio::time::timeout_at(request_deadline, edit(row.clone())).await;
        let remove = match result {
            Ok(Ok(())) | Ok(Err(Some(403 | 404))) => true,
            _ => { counts.failed += 1; now.saturating_sub(row.created_at) > MAX_AGE }
        };
        if remove {
            if store.remove_placeholder(&row.message_name).is_ok() { counts.deleted += 1; }
            else { counts.failed += 1; }
        }
    }
    Ok(counts)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn row(index: usize, age: i64) -> PendingPlaceholder {
        PendingPlaceholder {
            message_name: format!("spaces/A/messages/{index}"), space: "spaces/A".into(),
            app_agent: Some("sales".into()), app_email: "sales@example.invalid".into(),
            employee_id: "sales".into(), session_id: "googlechat:spaces/A:sales".into(),
            thread_name: Some("spaces/A/threads/T".into()), created_at: 100_000 - age,
        }
    }

    #[tokio::test]
    async fn sweep_edits_and_deletes_success_and_gone_but_keeps_transient_until_expired() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        for (i, age) in [(0, 10), (1, 10), (2, 10), (3, 10), (4, MAX_AGE + 1), (5, 5)] {
            store.insert_placeholder(&row(i, age)).unwrap();
        }
        let counts = sweep(&store, 100_000, tokio::time::Instant::now() + SWEEP_BUDGET, |row| async move {
            match row.message_name.rsplit('/').next().unwrap() {
                "0" => Ok(()), "1" => Err(Some(404)), "2" => Err(Some(403)), _ => Err(Some(503)),
            }
        }).await.unwrap();
        assert_eq!(counts, Counts { attempted: 5, deleted: 4, failed: 2 });
        assert_eq!(store.stale_placeholders(100_001).unwrap().iter().map(|r| r.message_name.clone()).collect::<Vec<_>>(),
            vec![row(3, 10).message_name, row(5, 5).message_name]);
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_caps_rows_and_bounds_each_request_and_whole_startup() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        for i in 0..205 { store.insert_placeholder(&row(i, 10)).unwrap(); }
        let calls = AtomicUsize::new(0);
        let counts = sweep(&store, 100_000, tokio::time::Instant::now() + SWEEP_BUDGET, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }).await.unwrap();
        assert_eq!(counts.deleted, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 200);
        for i in 0..10 { store.insert_placeholder(&row(300 + i, 10)).unwrap(); }
        let start = tokio::time::Instant::now();
        let counts = sweep(&store, 100_000, start + SWEEP_BUDGET, |_| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        }).await.unwrap();
        assert_eq!(tokio::time::Instant::now() - start, SWEEP_BUDGET);
        assert_eq!(counts.attempted, 6);
        assert_eq!(counts.failed, 6);
        assert_eq!(counts.deleted, 0);
    }

    #[tokio::test]
    async fn startup_without_gateway_lock_does_not_touch_database() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        store.insert_placeholder(&row(0, MAX_AGE + 1)).unwrap();
        startup(home.path()).await;
        assert_eq!(store.stale_placeholders(100_000).unwrap(), vec![row(0, MAX_AGE + 1)]);
    }
}
