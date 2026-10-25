//! C1 cfg(test) one-shot owned-home boundary hooks. All tests UNRUN.
use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
type Hook = Box<dyn FnOnce() + Send>;
struct Entry {
    remaining: usize,
    hook: Hook,
    visits: Arc<AtomicUsize>,
    fired: Arc<AtomicUsize>,
}
type Hooks = HashMap<(PathBuf, i32), Entry>;
fn hooks() -> &'static Mutex<Hooks> {
    static HOOKS: OnceLock<Mutex<Hooks>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
pub(super) struct Guard {
    home: PathBuf,
    phase: i32,
    visits: Arc<AtomicUsize>,
    fired: Arc<AtomicUsize>,
}
impl Guard {
    pub(super) fn at(home: &Path, phase: i32, hook: impl FnOnce() + Send + 'static) -> Self {
        Self::at_nth(home, phase, 1, hook)
    }
    pub(super) fn at_nth(
        home: &Path,
        phase: i32,
        nth: usize,
        hook: impl FnOnce() + Send + 'static,
    ) -> Self {
        assert!(nth > 0);
        let home = home.canonicalize().unwrap();
        let visits = Arc::new(AtomicUsize::new(0));
        let fired = Arc::new(AtomicUsize::new(0));
        assert!(
            hooks()
                .lock()
                .unwrap()
                .insert(
                    (home.clone(), phase),
                    Entry {
                        remaining: nth,
                        hook: Box::new(hook),
                        visits: visits.clone(),
                        fired: fired.clone()
                    }
                )
                .is_none()
        );
        Self {
            home,
            phase,
            visits,
            fired,
        }
    }
    pub(super) fn visits(&self) -> usize {
        self.visits.load(Ordering::SeqCst)
    }
    pub(super) fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        hooks()
            .lock()
            .unwrap()
            .remove(&(self.home.clone(), self.phase));
    }
}
pub(super) fn boundary(home: &Path, phase: i32) {
    let selected = {
        let mut hooks = hooks().lock().unwrap();
        let key = (home.to_owned(), phase);
        if let Some(entry) = hooks.get_mut(&key) {
            entry.visits.fetch_add(1, Ordering::SeqCst);
            entry.remaining -= 1;
            if entry.remaining == 0 {
                hooks.remove(&key)
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(entry) = selected {
        entry.fired.fetch_add(1, Ordering::SeqCst);
        (entry.hook)();
    }
}

struct RemoteClock {
    start: tokio::time::Instant,
    elapsed: Arc<AtomicU64>,
}
fn remote_clocks() -> &'static Mutex<HashMap<PathBuf, RemoteClock>> {
    static CLOCKS: OnceLock<Mutex<HashMap<PathBuf, RemoteClock>>> = OnceLock::new();
    CLOCKS.get_or_init(Default::default)
}
/// A single owned test home can advance only its synchronous budget checks.
/// Tokio remains paused for asynchronous waits; the owned keepalive prevents
/// automatic time advancement while the separate loopback thread is running.
pub(super) struct RemoteClockGuard {
    home: PathBuf,
    elapsed: Arc<AtomicU64>,
    keepalive: tokio::task::JoinHandle<()>,
}
impl RemoteClockGuard {
    pub(super) fn fixed(home: &Path) -> Self {
        let home = home.canonicalize().unwrap();
        let elapsed = Arc::new(AtomicU64::new(0));
        assert!(
            remote_clocks()
                .lock()
                .unwrap()
                .insert(
                    home.clone(),
                    RemoteClock {
                        start: tokio::time::Instant::now(),
                        elapsed: elapsed.clone(),
                    }
                )
                .is_none()
        );
        let keepalive = tokio::spawn(async {
            // A real-wall leak cap also bounds a stuck owned fixture while
            // Tokio's per-test clock is deliberately paused.
            let stop_at = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while std::time::Instant::now() < stop_at {
                tokio::task::yield_now().await;
            }
        });
        Self {
            home,
            elapsed,
            keepalive,
        }
    }
    pub(super) fn elapsed_handle(&self) -> Arc<AtomicU64> {
        self.elapsed.clone()
    }
}
impl Drop for RemoteClockGuard {
    fn drop(&mut self) {
        remote_clocks().lock().unwrap().remove(&self.home);
        self.keepalive.abort();
    }
}
pub(super) fn remote_now(home: &Path) -> tokio::time::Instant {
    home.canonicalize()
        .ok()
        .and_then(|home| {
            remote_clocks().lock().unwrap().get(&home).map(|clock| {
                clock.start + std::time::Duration::from_millis(clock.elapsed.load(Ordering::SeqCst))
            })
        })
        .unwrap_or_else(tokio::time::Instant::now)
}
#[tokio::test(start_paused = true)]
async fn remote_deadline_clock_is_owned_by_one_home_and_restored_on_drop() {
    let first = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let original = tokio::time::Instant::now();
    let guard = RemoteClockGuard::fixed(first.path());
    guard.elapsed_handle().store(80, Ordering::SeqCst);
    assert_eq!(
        remote_now(first.path()),
        original + std::time::Duration::from_millis(80)
    );
    assert_eq!(remote_now(other.path()), original);
    drop(guard);
    assert_eq!(remote_now(first.path()), original);
}
