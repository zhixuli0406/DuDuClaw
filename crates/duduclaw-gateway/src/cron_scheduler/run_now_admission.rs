//! Process-local scoped manual-run admission. Monotonic sliding windows reset
//! when the gateway restarts; outstanding reservations live until task drop.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_PER_ROW: usize = 3;
const MAX_PER_USER: usize = 10;
const MAX_USER_OUTSTANDING: usize = 2;
const MAX_GLOBAL_OUTSTANDING: usize = super::MAX_CONCURRENT_CRON;
// Fail closed instead of allowing many distinct principals/rows to grow memory.
const MAX_TRACKED_KEYS: usize = 4096;
const BUSY: &str = "Automation busy; retry after an active run finishes";
const UNAVAILABLE: &str = "Automation admission unavailable; try again later";

#[derive(Default)]
struct UserWindow {
    accepted: VecDeque<Instant>,
    outstanding: usize,
}

#[derive(Default)]
struct State {
    users: HashMap<String, UserWindow>,
    rows: HashMap<String, VecDeque<Instant>>,
    outstanding: usize,
}

#[derive(Default)]
pub(super) struct RunNowAdmission {
    state: Mutex<State>,
}

/// Holding this value owns both admission and a real cron execution slot.
/// Cancellation, early returns and panics release both through Drop.
pub(super) struct RunNowPermit {
    admission: Arc<RunNowAdmission>,
    user: String,
    _execution: OwnedSemaphorePermit,
}

fn prune(window: &mut VecDeque<Instant>, now: Instant) {
    while window.front().is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW) {
        window.pop_front();
    }
}

impl RunNowAdmission {
    pub(super) fn admit(
        self: &Arc<Self>, user: &str, row: &str, execution: &Arc<Semaphore>,
    ) -> Result<RunNowPermit, String> {
        self.admit_at(user, row, execution, Instant::now())
    }

    fn admit_at(
        self: &Arc<Self>, user: &str, row: &str, execution: &Arc<Semaphore>, now: Instant,
    ) -> Result<RunNowPermit, String> {
        if user.is_empty() || row.is_empty() || user.len() > 256 || row.len() > 256 {
            return Err("Invalid automation admission identity".into());
        }
        let mut state = self.state.lock().map_err(|_| UNAVAILABLE)?;
        state.users.retain(|_, window| {
            prune(&mut window.accepted, now);
            window.outstanding > 0 || !window.accepted.is_empty()
        });
        state.rows.retain(|_, window| {
            prune(window, now);
            !window.is_empty()
        });
        if state.outstanding >= MAX_GLOBAL_OUTSTANDING
            || state.users.get(user).is_some_and(|w| w.outstanding >= MAX_USER_OUTSTANDING) {
            return Err(BUSY.into());
        }
        if state.rows.get(row).is_some_and(|w| w.len() >= MAX_PER_ROW) {
            return Err("Automation run limit reached (3 per schedule per 10 minutes)".into());
        }
        if state.users.get(user).is_some_and(|w| w.accepted.len() >= MAX_PER_USER) {
            return Err("Automation run limit reached (10 per user per 10 minutes)".into());
        }
        if (!state.users.contains_key(user) && state.users.len() >= MAX_TRACKED_KEYS)
            || (!state.rows.contains_key(row) && state.rows.len() >= MAX_TRACKED_KEYS) {
            return Err(BUSY.into());
        }
        // No await, no task spawn, and no receipts on failure. Closed semaphores
        // are also rejected: a failed acquire must never become unbounded work.
        let execution = execution.clone().try_acquire_owned().map_err(|_| BUSY)?;
        let window = state.users.entry(user.into()).or_default();
        window.outstanding += 1;
        window.accepted.push_back(now);
        state.rows.entry(row.into()).or_default().push_back(now);
        state.outstanding += 1;
        Ok(RunNowPermit { admission: self.clone(), user: user.into(), _execution: execution })
    }
}

impl Drop for RunNowPermit {
    fn drop(&mut self) {
        // A poisoned state stays closed to future admission. The semaphore
        // permit is still released by its own Drop even in that case.
        if let Ok(mut state) = self.admission.state.lock() {
            if let Some(window) = state.users.get_mut(&self.user) {
                window.outstanding = window.outstanding.saturating_sub(1);
            }
            state.outstanding = state.outstanding.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_run_now_row_window_is_shared_across_users_and_expires() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(4));
        let now = Instant::now();
        for user in ["a", "b", "c"] {
            drop(admission.admit_at(user, "row", &execution, now).unwrap());
        }
        assert!(admission.admit_at("d", "row", &execution, now + WINDOW - Duration::from_nanos(1))
            .err().unwrap().contains("3 per schedule"));
        drop(admission.admit_at("d", "row", &execution, now + WINDOW).unwrap());
        assert_eq!(admission.state.lock().unwrap().users.len(), 1);
    }

    #[test]
    fn scoped_run_now_user_window_covers_different_rows() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(4));
        let now = Instant::now();
        for i in 0..MAX_PER_USER {
            drop(admission.admit_at("user", &format!("row{i}"), &execution, now).unwrap());
        }
        assert!(admission.admit_at("user", "extra", &execution, now).err().unwrap().contains("10 per user"));
        drop(admission.admit_at("other", "extra", &execution, now).unwrap());
        drop(admission.admit_at("user", "extra", &execution, now + WINDOW).unwrap());
    }

    #[test]
    fn scoped_run_now_outstanding_limits_release_on_drop() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(8));
        let a = admission.admit("a", "a1", &execution).unwrap();
        let b = admission.admit("a", "a2", &execution).unwrap();
        assert_eq!(admission.admit("a", "a3", &execution).err().unwrap(), BUSY);
        let c = admission.admit("b", "b1", &execution).unwrap();
        let d = admission.admit("b", "b2", &execution).unwrap();
        assert_eq!(admission.admit("c", "c1", &execution).err().unwrap(), BUSY);
        drop(a);
        let e = admission.admit("c", "c1", &execution).unwrap();
        drop((b, c, d, e));
        assert_eq!(execution.available_permits(), 8);
        assert_eq!(admission.state.lock().unwrap().outstanding, 0);
    }

    #[test]
    fn scoped_run_now_semaphore_busy_or_closed_does_not_consume_admission() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(1));
        let occupied = execution.clone().try_acquire_owned().unwrap();
        assert_eq!(admission.admit("user", "row", &execution).err().unwrap(), BUSY);
        assert!(admission.state.lock().unwrap().users.is_empty());
        drop(occupied);
        drop(admission.admit("user", "row", &execution).unwrap());
        execution.close();
        assert_eq!(admission.admit("user", "row", &execution).err().unwrap(), BUSY);
        assert_eq!(admission.state.lock().unwrap().users["user"].accepted.len(), 1);
    }

    #[test]
    fn scoped_run_now_concurrent_requests_cannot_exceed_outstanding_cap() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(8));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8).map(|i| {
            let (admission, execution, barrier) = (admission.clone(), execution.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                admission.admit("user", &format!("row{i}"), &execution)
            })
        }).collect();
        // Retain all accepted guards until every contender has finished.
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), MAX_USER_OUTSTANDING);
        drop(results);
        assert_eq!(execution.available_permits(), 8);
    }

    #[tokio::test]
    async fn scoped_run_now_abort_and_failed_work_release_reservations() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(4));
        let permit = admission.admit("user", "aborted", &execution).unwrap();
        let task = tokio::spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(admission.state.lock().unwrap().outstanding, 0);
        let permit = admission.admit("user", "failed", &execution).unwrap();
        let result = tokio::spawn(async move {
            let _permit = permit;
            Err::<(), _>("execution refused")
        }).await.unwrap();
        assert!(result.is_err());
        assert_eq!(admission.state.lock().unwrap().outstanding, 0);
        assert_eq!(execution.available_permits(), 4);
    }

    #[test]
    fn scoped_run_now_poisoned_state_fails_closed() {
        let admission = Arc::new(RunNowAdmission::default());
        let poison = admission.clone();
        assert!(std::thread::spawn(move || {
            let _state = poison.state.lock().unwrap();
            panic!("test poisoned admission");
        }).join().is_err());
        let execution = Arc::new(Semaphore::new(4));
        assert_eq!(admission.admit("user", "row", &execution).err().unwrap(), UNAVAILABLE);
        assert_eq!(execution.available_permits(), 4);
    }

    #[test]
    fn scoped_run_now_key_capacity_is_bounded_and_stale_rows_are_pruned() {
        let admission = Arc::new(RunNowAdmission::default());
        let execution = Arc::new(Semaphore::new(4));
        let now = Instant::now();
        for i in 0..MAX_TRACKED_KEYS {
            admission.state.lock().unwrap().rows.insert(format!("row{i}"), VecDeque::from([now]));
        }
        assert_eq!(admission.admit_at("user", "new", &execution, now).err().unwrap(), BUSY);
        drop(admission.admit_at("user", "new", &execution, now + WINDOW).unwrap());
        assert_eq!(admission.state.lock().unwrap().rows.len(), 1);
    }

    #[tokio::test]
    async fn scoped_run_now_scheduler_rejects_busy_before_spawning() {
        use crate::cron_scheduler::CronScheduler;
        use crate::cron_store::{CronStore, CronTaskRow};
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(CronStore::open(home.path()).unwrap());
        let row = CronTaskRow::new("manual".into(), "manual".into(), "worker".into(), "0 9 * * *".into(), "work".into());
        store.insert(&row).await.unwrap();
        let scheduler = CronScheduler::new(home.path().into(), store.clone(),
            Arc::new(tokio::sync::RwLock::new(duduclaw_agent::registry::AgentRegistry::new(home.path().join("agents")))));
        let occupied = scheduler.semaphore.clone().try_acquire_many_owned(super::super::MAX_CONCURRENT_CRON as u32).unwrap();
        assert_eq!(scheduler.run_now_scoped(row.clone(), "user:test").await.unwrap_err(), BUSY);
        assert!(scheduler.run_now_admission.state.lock().unwrap().users.is_empty());
        assert_eq!(store.get(&row.id).await.unwrap().unwrap().run_count, 0);
        drop(occupied);
        scheduler.semaphore.close();
        assert_eq!(scheduler.run_now_scoped(row, "user:test").await.unwrap_err(), BUSY);
        assert_eq!(scheduler.semaphore.available_permits(), super::super::MAX_CONCURRENT_CRON);
    }
}
