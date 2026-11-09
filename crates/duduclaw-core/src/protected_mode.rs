//! SR5 round 3 N3: the one native-Linux protected-mode latch.
//!
//! Before this round three switches (`execution_gate::isolation_required`,
//! the gateway's `runtime_readiness::is_protected_linux` and
//! `isolation_label_stores::protected_active`) each read only the process
//! environment (`DUDUCLAW_PROTECTED_KEY_MODE=native-systemd`). An
//! employee-editable environment — for example a per-server `env` block in an
//! employee's `.mcp.json` that starts `duduclaw mcp-server` without the flag —
//! could turn protected rules off for that process.
//!
//! Now every switch routes through [`active`]: protected mode is on when
//!
//! - the environment flag says so (unchanged), **or**
//! - a host-owned durable fact exists ([`durable_fact`]):
//!   - the root-owned mode marker [`MODE_MARKER`], written by
//!     `duduclaw service install --protected-linux` and kept on uninstall
//!     (removing it is a deliberate root action), or
//!   - the protected service home's confirmed employee index
//!     ([`SERVICE_HOME`]`/migration-authority/apply.db`) holds any row.
//!
//! Neither fact is under an employee's control (both live outside every
//! employee tree and are not derived from the process environment), and a
//! fact that cannot be ruled out (unreadable marker path, an index that exists
//! but cannot be read by this uid) counts as present: the latch only ever
//! narrows. Once on, the process stays protected ([`active`] latches).
//! Only Linux consults the system sources; elsewhere protected mode is off,
//! as before.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Root-owned marker written at protected install (0644, root:root).
pub const MODE_MARKER: &str = "/etc/duduclaw/protected-mode";
/// The protected service's fixed DuDuClaw home (`HOME=/var/lib/duduclaw`).
pub const SERVICE_HOME: &str = "/var/lib/duduclaw/.duduclaw";
/// The environment value that selects protected mode.
pub const MODE_VALUE: &str = "native-systemd";

static LATCHED: AtomicBool = AtomicBool::new(false);

/// The inputs of one protected-mode decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// The value of [`crate::root_key::MODE_ENV`] (`None` = unset).
    pub env: Option<OsString>,
    /// The root-owned mode marker path.
    pub marker: PathBuf,
    /// The protected service home whose confirmed employee index is read.
    pub service_home: PathBuf,
}

impl Sources {
    /// This host's sources.
    pub fn system() -> Self {
        Self {
            env: std::env::var_os(crate::root_key::MODE_ENV),
            marker: PathBuf::from(MODE_MARKER),
            service_home: PathBuf::from(SERVICE_HOME),
        }
    }

    /// Protected when the environment says so OR a durable fact exists.
    pub fn decide(&self) -> bool {
        self.env.as_deref() == Some(OsStr::new(MODE_VALUE)) || durable_fact(&self.marker, &self.service_home)
    }
}

/// A host-owned durable fact of protected mode: the marker path exists (any
/// entry; a stat error other than "not found" also counts), or the service
/// home's confirmed employee index holds a row (an index that exists but
/// cannot be read safely also counts).
pub fn durable_fact(marker: &Path, service_home: &Path) -> bool {
    match std::fs::symlink_metadata(marker) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return true,
    }
    !matches!(crate::identity_token::any_confirmed_employee(service_home), Ok(false))
}

thread_local! {
    static THREAD_SOURCES: std::cell::RefCell<Vec<Sources>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with `sources` consulted IN ADDITION to the system sources on this
/// thread only (never latched process-wide). It can only turn protected mode
/// on, so it is safe in production; tests use it to exercise the durable
/// facts without touching the process environment.
#[doc(hidden)]
pub fn with_thread_sources<R>(sources: Sources, f: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) { THREAD_SOURCES.with(|stack| { stack.borrow_mut().pop(); }); }
    }
    THREAD_SOURCES.with(|stack| stack.borrow_mut().push(sources));
    let _pop = Pop;
    f()
}

fn system_decides() -> bool {
    #[cfg(target_os = "linux")]
    { Sources::system().decide() }
    #[cfg(not(target_os = "linux"))]
    { false }
}

/// SR5 round 3 follow-up R3-4: how long an "off" answer from the system
/// sources is reused before the marker / index are probed again. Only the
/// negative answer is cached; "on" latches forever (see [`active`]).
pub const OFF_CACHE_WINDOW: Duration = Duration::from_secs(5);

/// A short negative cache keyed on the monotonic clock: while a recent probe
/// said "off" (younger than [`OFF_CACHE_WINDOW`]), the answer is reused
/// without re-statting the marker or re-opening SQLite. A positive probe
/// clears it. The lock is held across the probe so concurrent callers do not
/// stampede the probe.
struct OffCache(Mutex<Option<Instant>>);

impl OffCache {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn decide(&self, now: Instant, probe: impl FnOnce() -> bool) -> bool {
        let mut off_until = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if (*off_until).is_some_and(|until| now < until) {
            return false;
        }
        let on = probe();
        // `checked_add` failing (absurd clock) simply disables the cache.
        *off_until = if on { None } else { now.checked_add(OFF_CACHE_WINDOW) };
        on
    }
}

static SYSTEM_OFF: OffCache = OffCache::new();

/// The protected-mode switch. Every protected-mode decision in the product
/// must use this (directly or through `execution_gate::isolation_required`).
///
/// "On" (from the system sources) latches for the life of the process; an
/// "off" answer is reused for at most [`OFF_CACHE_WINDOW`] (R3-4), so a
/// source that appears later is seen within that window. Thread sources are
/// never cached and never latch.
pub fn active() -> bool {
    if LATCHED.load(Ordering::Acquire) {
        return true;
    }
    if THREAD_SOURCES.with(|stack| stack.borrow().iter().any(Sources::decide)) {
        return true;
    }
    if SYSTEM_OFF.decide(Instant::now(), system_decides) {
        LATCHED.store(true, Ordering::Release);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(dir: &Path) -> Sources {
        Sources { env: None, marker: dir.join("etc/protected-mode"), service_home: dir.join("home") }
    }

    fn confirmed_index(home: &Path, rows: usize) {
        let directory = home.join(crate::identity_token::APPLY_DB_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        let db = rusqlite::Connection::open(directory.join(crate::identity_token::APPLY_DB_FILE)).unwrap();
        db.execute_batch("CREATE TABLE confirmed_employees(label TEXT PRIMARY KEY, receipt TEXT NOT NULL)").unwrap();
        for index in 0..rows {
            db.execute("INSERT INTO confirmed_employees VALUES (?1,'{}')", [format!("e{index}")]).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.join(crate::identity_token::APPLY_DB_FILE),
                std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn sr5r3_env_absent_without_durable_fact_is_unprotected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!sources(dir.path()).decide());
        // An index without rows is no fact.
        confirmed_index(&dir.path().join("home"), 0);
        assert!(!sources(dir.path()).decide());
    }

    #[test]
    fn sr5r3_env_flag_alone_still_turns_protected_mode_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = sources(dir.path());
        s.env = Some(MODE_VALUE.into());
        assert!(s.decide());
        s.env = Some("anything-else".into());
        assert!(!s.decide());
    }

    #[test]
    fn sr5r3_marker_keeps_protected_mode_with_env_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        std::fs::create_dir_all(s.marker.parent().unwrap()).unwrap();
        std::fs::write(&s.marker, b"").unwrap();
        assert!(s.decide(), "the env var is absent but the root marker exists");
        // Turning the env "off" explicitly does not help either.
        let mut off = s.clone();
        off.env = Some("".into());
        assert!(off.decide());
    }

    #[test]
    fn sr5r3_confirmed_index_keeps_protected_mode_with_env_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        confirmed_index(&s.service_home, 1);
        assert!(s.decide());
    }

    #[cfg(unix)]
    #[test]
    fn sr5r3_unreadable_index_counts_as_protected() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        let directory = s.service_home.join(crate::identity_token::APPLY_DB_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        // A symlink in place of the store: not safely readable ⇒ protected.
        std::os::unix::fs::symlink("/dev/null", directory.join(crate::identity_token::APPLY_DB_FILE)).unwrap();
        assert!(s.decide());
    }

    #[test]
    fn sr5r3_thread_sources_only_narrow_and_never_latch() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        std::fs::create_dir_all(s.marker.parent().unwrap()).unwrap();
        std::fs::write(&s.marker, b"").unwrap();
        let before = active();
        assert!(with_thread_sources(s.clone(), active));
        assert!(with_thread_sources(s, crate::execution_gate::isolation_required));
        // Popped again; the process-wide answer did not change.
        assert_eq!(active(), before);
    }

    #[test]
    fn sr5r3f_off_answer_is_cached_briefly_and_a_later_source_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        let cache = OffCache::new();
        let start = Instant::now();
        let probes = std::cell::Cell::new(0);
        let probe = || { probes.set(probes.get() + 1); s.decide() };
        assert!(!cache.decide(start, probe));
        assert_eq!(probes.get(), 1);
        // The marker appears; inside the window the cached "off" is reused
        // (no re-stat / SQLite open).
        std::fs::create_dir_all(s.marker.parent().unwrap()).unwrap();
        std::fs::write(&s.marker, b"").unwrap();
        assert!(!cache.decide(start + Duration::from_secs(1), probe));
        assert_eq!(probes.get(), 1, "a fresh off answer is not re-probed");
        // After the window the new source is seen.
        assert!(cache.decide(start + OFF_CACHE_WINDOW, probe));
        assert_eq!(probes.get(), 2);
        // A positive answer is never cached as "off".
        assert!(cache.decide(start + OFF_CACHE_WINDOW + Duration::from_millis(1), probe));
    }

    #[test]
    fn sr5r3f_confirmed_index_appearing_after_window_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path());
        let cache = OffCache::new();
        let start = Instant::now();
        assert!(!cache.decide(start, || s.decide()));
        confirmed_index(&s.service_home, 1);
        assert!(!cache.decide(start + OFF_CACHE_WINDOW - Duration::from_millis(1), || s.decide()));
        assert!(cache.decide(start + OFF_CACHE_WINDOW + Duration::from_millis(1), || s.decide()));
    }
}
