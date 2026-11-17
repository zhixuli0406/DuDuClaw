//! Bounded, lock-holder-only recovery. Never POST a restart notice.
use std::{future::Future, path::Path, sync::Arc, time::Duration};
use crate::googlechat::{GoogleChatCreds, update_message, cleanup_placeholder, PlaceholderResolution};
use crate::googlechat_inbox::{Inbox, PendingPlaceholder};

const SWEEP_BUDGET: Duration = Duration::from_secs(30);
const REQUEST_BUDGET: Duration = Duration::from_secs(5);
const MAX_AGE: i64 = 24 * 60 * 60;

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts { attempted: usize, deleted: usize, failed: usize }

/// Called before any Google Chat transport starts accepting inbound messages.
pub(crate) async fn startup(home: &Path) { recover(home, 5).await; }

/// Started once by the lock-holding gateway, including before Chat is configured.
pub(crate) fn spawn(home: std::path::PathBuf) -> Option<tokio::task::JoinHandle<()>> {
    if !duduclaw_core::gateway_instance::held(&home) { return None; }
    Some(tokio::spawn(async move {
        let interval = Duration::from_secs(60);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if !duduclaw_core::gateway_instance::held(&home) { break; }
            recover(&home, 60).await;
        }
    }))
}

async fn recover(home: &Path, minimum_age: i64) {
    if !duduclaw_core::gateway_instance::held(home) { return; }
    // No channel state yet: do not create a database just for maintenance.
    if !home.join("googlechat_inbox.db").exists() { return; }
    let deadline = tokio::time::Instant::now() + SWEEP_BUDGET;
    let store = match Inbox::shared(home) {
        Ok(store) => store,
        Err(_) => { tracing::warn!(failed = 1, "Google Chat placeholder recovery failed"); return; }
    };
    let lang = crate::trigger_route::texts::lang_for(home);
    let text = crate::trigger_route::texts::googlechat_interrupted(lang);
    let cleanup_text = crate::trigger_route::texts::googlechat_replied(lang);
    let result = sweep(&store, chrono::Utc::now().timestamp(), minimum_age, deadline, |row| async move {
        if !duduclaw_core::gateway_instance::held(home) { return Err(None); }
        let _binding = crate::agent_channels::binding_lock().await;
        let creds = match row.app_agent.as_deref() {
            Some(id) => crate::googlechat_agents::agent_creds(home, id).await,
            None => GoogleChatCreds::from_config(home).await.map(Arc::new),
        }.ok_or(None)?;
        // Removing/rebinding an employee app must never edit with a different app.
        if creds.client_email() != row.app_email { return Err(None); }
        if row.state == "cleanup_only" {
            match cleanup_placeholder(&creds, &row.message_name, cleanup_text).await {
                PlaceholderResolution::Terminal => Ok(()),
                PlaceholderResolution::Retry(status) => Err(status),
            }
        } else {
            update_message(&creds, &row.message_name, text).await.map_err(|e| e.status)
        }
    }).await;
    // Counts only: no content, identifiers, resource names or error bodies.
    match result {
        Ok(counts) if counts.attempted > 0 => tracing::warn!(attempted = counts.attempted, deleted = counts.deleted, failed = counts.failed,
            "Google Chat placeholder recovery"),
        Err(()) => tracing::warn!(failed = 1, "Google Chat placeholder recovery failed"),
        _ => {},
    }
}

/// Timeout includes credential resolution/token exchange as well as PATCH.
async fn sweep<F, Fut>(store: &Inbox, now: i64, minimum_age: i64, deadline: tokio::time::Instant, mut edit: F) -> Result<Counts, ()>
where F: FnMut(PendingPlaceholder) -> Fut, Fut: Future<Output = Result<(), Option<u16>>> {
    let rows = store.inactive_placeholders_before(now.saturating_sub(minimum_age)).map_err(|_| ())?;
    let mut counts = Counts::default();
    for (rowid, row) in rows {
        if tokio::time::Instant::now() >= deadline { break; }
        // Recheck liveness immediately before any validation/removal or request.
        if row.as_ref().is_some_and(|r| store.placeholder_active(&r.message_name)) { continue; }
        counts.attempted += 1;
        let row = match row.filter(PendingPlaceholder::valid) {
            Some(row) => row,
            None => {
                if store.remove_placeholder_row(rowid).is_ok() { counts.deleted += 1; }
                else { counts.failed += 1; }
                continue;
            }
        };
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
            message_name: format!("spaces/A/messages/client-{index:032x}"), space: "spaces/A".into(),
            app_agent: Some("sales".into()), app_email: "sales@example.invalid".into(),
            employee_id: "sales".into(), session_id: "googlechat:spaces/A:sales".into(),
            thread_name: Some("spaces/A/threads/T".into()), created_at: 100_000 - age, state: "pending".into(),
        }
    }

    #[tokio::test]
    async fn sweep_edits_and_deletes_success_and_gone_but_keeps_transient_until_expired() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        for (i, age) in [(0, 10), (1, 10), (2, 10), (3, 10), (4, MAX_AGE + 1), (5, 5)] {
            store.insert_placeholder(&row(i, age)).unwrap();
        }
        let counts = sweep(&store, 100_000, 5, tokio::time::Instant::now() + SWEEP_BUDGET, |r| async move {
            match r.message_name {
                name if name == row(0, 10).message_name => Ok(()),
                name if name == row(1, 10).message_name => Err(Some(404)),
                name if name == row(2, 10).message_name => Err(Some(403)),
                _ => Err(Some(503)),
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
        let counts = sweep(&store, 100_000, 5, tokio::time::Instant::now() + SWEEP_BUDGET, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }).await.unwrap();
        assert_eq!(counts.deleted, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 200);
        for i in 0..10 { store.insert_placeholder(&row(300 + i, 10)).unwrap(); }
        let start = tokio::time::Instant::now();
        let counts = sweep(&store, 100_000, 5, start + SWEEP_BUDGET, |_| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        }).await.unwrap();
        assert_eq!(tokio::time::Instant::now() - start, SWEEP_BUDGET);
        assert_eq!(counts.attempted, 6);
        assert_eq!(counts.failed, 6);
        assert_eq!(counts.deleted, 0);
    }

    #[tokio::test]
    async fn sweep_deletes_invalid_rows_without_requests() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        let conn = rusqlite::Connection::open(home.path().join("googlechat_inbox.db")).unwrap();
        let invalid_names = [
            "spaces/A/messages/123".to_owned(),
            "spaces/A/messages/status".to_owned(),
            "spaces/A/messages/client-XYZ".to_owned(),
            format!("spaces/A/messages/client-{}", "a".repeat(31)),
            format!("spaces/A/messages/client-{}", "A".repeat(32)),
        ];
        for (i, name) in invalid_names.iter().enumerate() {
            let mut invalid = row(i, 100);
            invalid.message_name = name.clone();
            // Simulate tampered disk state bypassing insert validation.
            conn.execute(
                "INSERT INTO pending_placeholders(message_name,space,app_agent,app_email,employee_id,session_id,thread_name,created_at,state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![invalid.message_name,invalid.space,invalid.app_agent,invalid.app_email,invalid.employee_id,invalid.session_id,invalid.thread_name,invalid.created_at,invalid.state]).unwrap();
        }
        // Keep the valid control at the age cutoff so no provider call is due.
        let valid = row(99, 60);
        assert!(valid.valid());
        store.insert_placeholder(&valid).unwrap();
        for (i, column, value) in [
            (0, "message_name", "spaces/../messages/m".to_owned()),
            (1, "message_name", format!("spaces/A/messages/{}", "m".repeat(129))),
            (2, "space", "spaces/B".into()),
            (3, "app_agent", "a".repeat(129)), (4, "employee_id", "a".repeat(129)),
            (5, "app_email", "e".repeat(321)), (6, "session_id", "s".repeat(513)),
            (7, "thread_name", "t".repeat(513)),
        ] {
            let row = row(i, 100);
            store.insert_placeholder(&row).unwrap();
            // Simulate legacy/corrupt disk state bypassing insert validation.
            conn.execute(&format!("UPDATE pending_placeholders SET {column}=?1 WHERE message_name=?2"),
                rusqlite::params![value, row.message_name]).unwrap();
        }
        let counts = sweep(&store, 100_000, 60, tokio::time::Instant::now() + SWEEP_BUDGET,
            |_| async { panic!("invalid row must never request credentials or make a request") }).await.unwrap();
        assert_eq!(counts, Counts { attempted: 13, deleted: 13, failed: 0 });
        assert_eq!(store.stale_placeholders(100_000).unwrap(), vec![valid]);
    }

    #[tokio::test]
    async fn sweep_never_touches_active_rows_and_drop_enables_recovery() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        let old = row(0, MAX_AGE + 1);
        let registration = crate::googlechat_inbox::PlaceholderRegistration::new(store.clone(), &old).unwrap();
        // Many active rows must not starve the first inactive row under LIMIT 200.
        let mut registrations = Vec::new();
        for i in 1..205 {
            registrations.push(crate::googlechat_inbox::PlaceholderRegistration::new(store.clone(), &row(i, MAX_AGE + 1)).unwrap());
        }
        store.insert_placeholder(&row(300, 61)).unwrap();
        let counts = sweep(&store, 100_000, 60, tokio::time::Instant::now() + SWEEP_BUDGET, |r| async move {
        assert_eq!(r.message_name, row(300, 61).message_name); Ok(())
        }).await.unwrap();
        assert_eq!(counts.attempted, 1);
        drop(registration);
        let counts = sweep(&store, 100_000, 60, tokio::time::Instant::now() + SWEEP_BUDGET, |r| async move {
            assert_eq!(r.message_name, row(0, MAX_AGE + 1).message_name); Ok(())
        }).await.unwrap();
        assert_eq!(counts.deleted, 1);
        assert_eq!(store.stale_placeholders(100_000).unwrap().len(), 200);
    }

    #[tokio::test]
    async fn periodic_sweep_requires_more_than_sixty_seconds_of_age() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        for (index, age) in [(0, 59), (1, 60), (2, 61)] { store.insert_placeholder(&row(index, age)).unwrap(); }
        let counts = sweep(&store, 100_000, 60, tokio::time::Instant::now() + SWEEP_BUDGET, |r| async move {
            assert_eq!(r.message_name, row(2, 61).message_name); Ok(())
        }).await.unwrap();
        assert_eq!(counts, Counts { attempted: 1, deleted: 1, failed: 0 });
        assert_eq!(store.stale_placeholders(100_000).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn startup_without_gateway_lock_does_not_touch_database() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        store.insert_placeholder(&row(0, MAX_AGE + 1)).unwrap();
        startup(home.path()).await;
        assert!(spawn(home.path().to_path_buf()).is_none());
        assert_eq!(store.stale_placeholders(100_000).unwrap(), vec![row(0, MAX_AGE + 1)]);
    }
}
