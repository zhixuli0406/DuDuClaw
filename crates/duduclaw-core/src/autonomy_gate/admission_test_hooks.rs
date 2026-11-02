//! Home-scoped, one-shot observation of actual file-lock contention in tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::sync::oneshot;

static WAITERS: OnceLock<Mutex<HashMap<PathBuf, oneshot::Sender<()>>>> = OnceLock::new();

/// Remove an unused registration even if the observing test fails.
pub struct WaitObserver(PathBuf);

impl Drop for WaitObserver {
    fn drop(&mut self) {
        WAITERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&self.0);
    }
}

/// The receiver fires only after a lock attempt for this home is contended.
pub fn observe_wait(home: &Path) -> (WaitObserver, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    let home = home.to_path_buf();
    assert!(
        WAITERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(home.clone(), tx)
            .is_none(),
        "duplicate admission wait observer"
    );
    (WaitObserver(home), rx)
}

pub(super) fn notify_wait(home: &Path) {
    let sender = WAITERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(home);
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_is_home_scoped_and_cleans_up_unused_registration() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (observer, mut waiting) = observe_wait(home.path());
        notify_wait(other.path());
        assert_eq!(waiting.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        notify_wait(home.path());
        assert_eq!(waiting.try_recv(), Ok(()));
        drop(observer);

        let (observer, mut waiting) = observe_wait(home.path());
        drop(observer);
        assert_eq!(waiting.try_recv(), Err(oneshot::error::TryRecvError::Closed));
        let (_observer, _) = observe_wait(home.path());
    }
}
