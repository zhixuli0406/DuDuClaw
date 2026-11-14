//! Bounded waiting of the ACL delivery fence: contention is not a refusal.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

const THREADS: usize = 8;

fn test_db() -> (UserDb, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let db = UserDb::new(tmp.path()).unwrap();
    (db, tmp)
}

/// Half of the callers use a second pool on the same file, so the waits cover
/// both the in-process slot and SQLite's cross-connection write reservation.
fn run_concurrently(db: &UserDb, other: &UserDb, generation: u64, delivered: &AtomicUsize)
    -> Vec<Result<(), AclDeliveryError>> {
    let start = std::sync::Barrier::new(THREADS);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS).map(|index| {
            let target = if index % 2 == 0 { db } else { other };
            let start = &start;
            scope.spawn(move || {
                start.wait();
                target.with_acl_generation(generation, || {
                    // A synchronous delivery decision that takes a moment,
                    // such as a socket write, while holding the reservation.
                    std::thread::sleep(Duration::from_millis(5));
                    delivered.fetch_add(1, Ordering::SeqCst);
                })
            })
        }).collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    })
}

#[test]
fn simultaneous_deliveries_with_current_generation_all_run() {
    let (db, tmp) = test_db();
    let other = UserDb::new(tmp.path()).unwrap();
    let generation = db.acl_generation().unwrap();
    let delivered = AtomicUsize::new(0);
    let results = run_concurrently(&db, &other, generation, &delivered);
    assert!(results.iter().all(Result::is_ok), "contention must wait, not refuse: {results:?}");
    assert_eq!(delivered.load(Ordering::SeqCst), THREADS);
}

#[test]
fn simultaneous_deliveries_with_changed_generation_are_all_refused() {
    let (db, tmp) = test_db();
    let other = UserDb::new(tmp.path()).unwrap();
    let generation = db.acl_generation().unwrap();
    Connection::open(tmp.path()).unwrap()
        .execute("UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", []).unwrap();
    let delivered = AtomicUsize::new(0);
    let results = run_concurrently(&db, &other, generation, &delivered);
    assert!(results.iter().all(|result| matches!(result, Err(AclDeliveryError::Changed))), "{results:?}");
    assert_eq!(delivered.load(Ordering::SeqCst), 0, "a real change must never enter the callback");
}

#[test]
fn both_pool_slots_held_past_the_budget_refuse_as_contention() {
    let (db, _tmp) = test_db();
    let generation = db.acl_generation().unwrap();
    let _first = db.pool[0].lock().unwrap();
    let _second = db.pool[1].lock().unwrap();
    let started = Instant::now();
    let result = db.with_acl_generation(generation, || panic!("contention past the budget must not deliver"));
    assert_eq!(result, Err(AclDeliveryError::Contended));
    let waited = started.elapsed();
    assert!(waited >= ACL_DELIVERY_WAIT, "refused before the budget ran out: {waited:?}");
    assert!(waited < ACL_DELIVERY_WAIT * 20, "the wait must stay bounded: {waited:?}");
}

#[test]
fn write_reservation_held_briefly_is_waited_for_and_held_past_the_budget_is_contention() {
    let (db, tmp) = test_db();
    let generation = db.acl_generation().unwrap();
    // Held shorter than the budget: the delivery waits and then proceeds.
    let (held, holding) = std::sync::mpsc::channel();
    let path = tmp.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        held.send(()).unwrap();
        std::thread::sleep(ACL_DELIVERY_WAIT / 5);
        other.execute_batch("COMMIT").unwrap();
    });
    holding.recv().unwrap();
    let mut delivered = 0;
    db.with_acl_generation(generation, || delivered += 1).unwrap();
    assert_eq!(delivered, 1);
    writer.join().unwrap();
    // Held for longer than the budget: refused as contention, not as a change.
    let other = Connection::open(tmp.path()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = db.with_acl_generation(generation, || panic!("busy past the budget must deny"));
    assert_eq!(result, Err(AclDeliveryError::Contended));
    other.execute_batch("ROLLBACK").unwrap();
    db.with_acl_generation(generation, || delivered += 1).unwrap();
    assert_eq!(delivered, 2, "the refused delivery left the connection usable");
}
