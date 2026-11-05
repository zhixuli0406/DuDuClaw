//! Whether team grants (a Manager calling or assigning to a report's AI
//! employee, `duduclaw_auth::org`) apply in a home.
//!
//! Reporting lines are an Enterprise feature. The Personal edition blocks
//! every `users.*` write, but an Enterprise → Personal downgrade leaves
//! `users.manager_user_id` rows behind; those must not take effect again.
//! The gateway records the edition it resolves for its home
//! ([`record`], called from `MethodHandler::resolve_edition_profile`, which
//! the boot path and the 60 s edition watcher both run), and every place
//! that builds a caller's access drops team grants unless the recorded
//! edition is Enterprise ([`team_grants_enabled`]). Nothing recorded (an
//! early message before boot finished, a CLI process, a unit test that did
//! not opt in) means no team grants: the gate only narrows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::sync::atomic::{AtomicU8, Ordering};

use duduclaw_core::EditionProfile;

#[derive(Debug, Default)]
struct EditionState {
    edition: Option<EditionProfile>,
    epoch: u64,
}

#[derive(Debug, Default)]
struct EditionCell {
    current: RwLock<EditionState>,
    // Legacy boolean queries can occur inside the final delivery callback.
    // Do not recursively acquire the RwLock: a queued writer could deadlock it.
    // 0 = unresolved, 1 = Enterprise, 2 = Personal. Stored under the write lock.
    published: AtomicU8,
}

// Entries are never replaced or removed: existing snapshots must keep fencing
// against the same lock, including Enterprise -> Personal -> Enterprise (ABA).
fn edition_state(home: &Path) -> Result<Arc<EditionCell>, String> {
    static EDITIONS: OnceLock<Mutex<HashMap<PathBuf, Arc<EditionCell>>>> = OnceLock::new();
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let mut editions = EDITIONS.get_or_init(|| Mutex::new(HashMap::new()))
        .lock().map_err(|_| "edition registry poisoned")?;
    Ok(editions.entry(home).or_default().clone())
}

/// Edition and epoch captured together, used both to gate the ACL snapshot
/// and to fence its eventual delivery. No lock is held across async work.
#[derive(Debug, Clone)]
pub(crate) struct EditionSnapshot {
    state: Arc<EditionCell>,
    edition: Option<EditionProfile>,
    epoch: u64,
}

pub(crate) fn snapshot(home: &Path) -> Result<EditionSnapshot, String> {
    let state = edition_state(home)?;
    let current = state.current.read().map_err(|_| "edition state poisoned")?;
    let snapshot = EditionSnapshot { state: state.clone(), edition: current.edition, epoch: current.epoch };
    Ok(snapshot)
}

impl EditionSnapshot {
    pub(crate) fn is_current(&self) -> bool {
        self.with_current(|| ()).is_ok()
    }

    /// Nonblocking final fence. Keep the read guard through the synchronous
    /// callback, so record cannot complete a downgrade between check and write.
    /// The callback must not await, record an edition, or capture a new snapshot.
    /// Legacy boolean edition queries use the separately published atomic value.
    pub(crate) fn with_current<T>(&self, write: impl FnOnce() -> T) -> Result<T, String> {
        let current = self.state.current.try_read().map_err(|_| "edition state unavailable")?;
        if current.epoch != self.epoch { return Err("edition changed".into()); }
        Ok(write())
    }

    pub(crate) fn gate_access(&self, mut access: duduclaw_auth::EffectiveAccess) -> duduclaw_auth::EffectiveAccess {
        if self.edition != Some(EditionProfile::Enterprise) { access.team.clear(); }
        access
    }
}

/// Remember the edition resolved for `home`. Repeated watcher observations do
/// not invalidate snapshots; every actual change increments the epoch, even
/// when users.db's ACL generation stays unchanged. Only this lock is held.
pub fn record(home: &Path, edition: EditionProfile) {
    let Ok(state) = edition_state(home) else { return; };
    let Ok(mut current) = state.current.write() else { return; };
    if current.edition != Some(edition) {
        // Never wrap and revive a stale lease. Exhaustion poisons this state.
        current.epoch = current.epoch.checked_add(1).expect("edition epoch exhausted");
        current.edition = Some(edition);
        state.published.store(match edition {
            EditionProfile::Enterprise => 1, EditionProfile::Personal => 2,
        }, Ordering::Release);
    }
}

/// Personal behaviour applies only when the gateway resolved this home as Personal.
pub(crate) fn is_personal(home: &Path) -> bool {
    edition_state(home).is_ok_and(|state| state.published.load(Ordering::Acquire) == 2)
}

/// Team grants apply only when `home` was last resolved as Enterprise.
pub fn team_grants_enabled(home: &Path) -> bool {
    edition_state(home).is_ok_and(|state| state.published.load(Ordering::Acquire) == 1)
}

/// `access` with its team grants dropped unless they apply in `home`.
pub fn gate_access(
    home: &Path,
    access: duduclaw_auth::EffectiveAccess,
) -> duduclaw_auth::EffectiveAccess {
    if access.team.is_empty() || team_grants_enabled(home) {
        access
    } else {
        duduclaw_auth::EffectiveAccess {
            team: Default::default(),
            ..access
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_recorded_enterprise_home_keeps_team_grants() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert!(!team_grants_enabled(a.path()), "nothing recorded");
        record(a.path(), EditionProfile::Enterprise);
        record(b.path(), EditionProfile::Personal);
        assert!(team_grants_enabled(a.path()));
        assert!(!team_grants_enabled(b.path()));
        record(a.path(), EditionProfile::Personal);
        assert!(!team_grants_enabled(a.path()), "a downgrade takes effect");
    }

    #[test]
    fn edition_epoch_tracks_changes_and_rejects_roundtrip_snapshots() {
        let home = tempfile::tempdir().unwrap();
        let unresolved = snapshot(home.path()).unwrap();
        assert_eq!(unresolved.epoch, 0);
        record(home.path(), EditionProfile::Enterprise);
        let enterprise = snapshot(home.path()).unwrap();
        assert_eq!(enterprise.epoch, 1);
        assert!(!unresolved.is_current());
        record(home.path(), EditionProfile::Enterprise);
        assert!(enterprise.is_current(), "watcher refresh is not a change");
        record(home.path(), EditionProfile::Personal);
        let personal = snapshot(home.path()).unwrap();
        assert_eq!(personal.epoch, 2);
        let called = std::cell::Cell::new(false);
        assert!(enterprise.with_current(|| called.set(true)).is_err());
        assert!(!called.get());
        record(home.path(), EditionProfile::Enterprise);
        assert_eq!(snapshot(home.path()).unwrap().epoch, 3);
        assert!(!enterprise.is_current(), "ABA must not revive old permits");
        assert!(!personal.is_current());
    }

    #[test]
    fn edition_lease_is_nonblocking_and_holds_writer_out_through_callback() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        record(home.path(), EditionProfile::Enterprise);
        let edition = snapshot(home.path()).unwrap();
        edition.with_current(|| {
            assert!(edition.state.current.try_write().is_err());
            // These are called by existing credential checks under the fence.
            assert!(team_grants_enabled(home.path()));
            assert!(!is_personal(home.path()));
            record(other.path(), EditionProfile::Personal);
        }).unwrap();
        let writer = edition.state.current.try_write().unwrap();
        let called = std::cell::Cell::new(false);
        assert!(edition.with_current(|| called.set(true)).is_err());
        assert!(!called.get(), "writer contention must deny without waiting");
        assert!(team_grants_enabled(home.path()), "legacy read must not acquire the held lock");
        drop(writer);
        assert!(edition.is_current());
    }

}
