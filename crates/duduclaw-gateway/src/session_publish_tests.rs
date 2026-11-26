//! WebChat publication waits for benign contention within one bounded budget.
use super::*;
use duduclaw_auth::{AclDeliveryError, UserDb, ACL_DELIVERY_WAIT};
use std::sync::Arc;
use tempfile::NamedTempFile;

const MESSAGES: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publications_of_different_messages_all_publish() {
    let sessions_file = NamedTempFile::new().unwrap();
    let users_home = test_users_home();
    let users_file = NamedTempFile::new_in(users_home.path()).unwrap();
    let mgr = Arc::new(SessionManager::new(sessions_file.path()).unwrap());
    // Two pools on one users.db: both the in-process slots and SQLite's
    // cross-connection write reservation are contended.
    let users = [Arc::new(UserDb::new(users_file.path()).unwrap()), Arc::new(UserDb::new(users_file.path()).unwrap())];
    let generation = users[0].acl_generation().unwrap();
    let mut rows = Vec::new();
    for index in 0..MESSAGES {
        let sid = format!("webchat:owned-{index}");
        mgr.get_or_create(&sid, "worker").await.unwrap();
        let id = mgr.append_delivery_message(&sid, "assistant", &format!("reply {index}"), 1, None, true)
            .await.unwrap();
        rows.push((sid, id));
    }
    let start = Arc::new(tokio::sync::Barrier::new(MESSAGES));
    let tasks: Vec<_> = rows.iter().cloned().enumerate().map(|(index, (sid, id))| {
        let (mgr, users, start) = (mgr.clone(), users[index % 2].clone(), start.clone());
        tokio::spawn(async move {
            let policy = crate::webchat::access::TargetPolicy::test_admin();
            start.wait().await;
            mgr.publish_webchat_message(&sid, id, &users, generation, &policy, || true).await
        })
    }).collect();
    for task in tasks {
        assert_eq!(task.await.unwrap(), Ok(true), "contention must wait, not refuse");
    }
    for (index, (sid, _)) in rows.iter().enumerate() {
        let messages = mgr.get_messages(sid).await.unwrap();
        assert_eq!(messages.len(), 1, "{sid}");
        assert_eq!(messages[0].content, format!("reply {index}"));
    }
}

#[tokio::test]
async fn publication_busy_past_the_budget_is_contention_and_a_real_change_is_not() {
    let sessions_file = NamedTempFile::new().unwrap();
    let users_home = test_users_home();
    let users_file = NamedTempFile::new_in(users_home.path()).unwrap();
    let mgr = SessionManager::new(sessions_file.path()).unwrap();
    let users = UserDb::new(users_file.path()).unwrap();
    let policy = crate::webchat::access::TargetPolicy::test_admin();
    let sid = "webchat:owned";
    mgr.get_or_create(sid, "worker").await.unwrap();
    let id = mgr.append_delivery_message(sid, "assistant", "private reply", 2, None, true).await.unwrap();
    let generation = users.acl_generation().unwrap();

    // users.db reservation held past the whole budget.
    let hold = Connection::open(users_file.path()).unwrap();
    hold.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let result = mgr.publish_webchat_message(sid, id, &users, generation, &policy,
        || panic!("a busy users.db must not enter the publication callback")).await;
    assert_eq!(result, Err(AclDeliveryError::Contended));
    assert!(started.elapsed() >= ACL_DELIVERY_WAIT);
    hold.execute_batch("ROLLBACK").unwrap();
    assert!(mgr.get_messages(sid).await.unwrap().is_empty());

    // sessions.db write lock held past the remaining budget, inside the lease.
    let hold = Connection::open(sessions_file.path()).unwrap();
    hold.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = mgr.publish_webchat_message(sid, id, &users, generation, &policy, || true).await;
    assert_eq!(result, Err(AclDeliveryError::Contended));
    hold.execute_batch("ROLLBACK").unwrap();
    assert!(mgr.get_messages(sid).await.unwrap().is_empty(), "a refused publication stays hidden");

    // A real ACL change is reported as such, not as contention.
    Connection::open(users_file.path()).unwrap()
        .execute("UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", []).unwrap();
    let result = mgr.publish_webchat_message(sid, id, &users, generation, &policy,
        || panic!("a stale generation must not enter the publication callback")).await;
    assert_eq!(result, Err(AclDeliveryError::Changed));
    assert!(mgr.get_messages(sid).await.unwrap().is_empty());

    // Nothing was consumed by the refusals: the current generation publishes.
    let generation = users.acl_generation().unwrap();
    assert_eq!(mgr.publish_webchat_message(sid, id, &users, generation, &policy, || true).await, Ok(true));
    assert_eq!(mgr.get_messages(sid).await.unwrap()[0].content, "private reply");
}
