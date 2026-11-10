//! Is the native-Linux isolation architecture installed on this host?
//!
//! Protected mode ([`crate::protected_mode::active`]) and the isolation
//! architecture (duduclaw-spawnd, egress, duduclaw-updated, the v1.79.3
//! migration) are separate facts. Until the isolation bootstrap is installable
//! (planned v1.82.0), every protected host is the v1.80 single-binary service:
//! employees run under the gateway's own service account with the v1.80
//! credential-protection controls. The isolation path (spawnd routing,
//! isolation MCP authorization, Personal employees, the proposal-only employee
//! lifecycle) applies only where [`installed`] is true; see
//! `execution_gate::isolation_required`.
//!
//! [`installed`] is true when ANY of [`FACTS`] exists, or cannot be proven
//! absent (a `symlink_metadata` error other than NotFound, a dangling symlink,
//! an entry of any type). The facts are only root-owned, fixed paths: the
//! spawnd control socket and its units, the spawnd state directory, the
//! updater-managed generation selector and state, the durable migration
//! import manifest and the fixed-path isolation binaries of the installer
//! layout. Every one of them lives below a root-owned directory that the
//! gateway's service account cannot write (`/run`, `/var/lib`, `/etc`,
//! `/etc/systemd/system`, `/opt/duduclaw-isolated`, `/opt/duduclaw/bin`,
//! `/usr/lib`, `/usr/libexec`), so an employee running as that account can
//! neither create one (which could only narrow: isolation required and
//! refused) nor remove one (which would widen). No fact is read from the
//! environment or from the gateway-owned home. Inside an employee runtime
//! launched by spawnd, `/usr` is the host's read-only clone, so the trampoline
//! `/usr/lib/duduclaw/duduclaw-run` is visible and a sandboxed `duduclaw
//! mcp-server` sees the install too (tested in duduclaw-spawnd). The v1.82
//! bootstrap must create one of these facts before any employee runs on the
//! isolation path; until then employees run single-binary. Once true, the answer latches
//! for the life of the process, so it only ever narrows. Only Linux consults
//! the system paths; elsewhere the architecture does not exist and the answer
//! is false (protected mode is off there as well).
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// `duduclaw_migration::import_manifest::DURABLE_IMPORT_MANIFEST`, duplicated
/// because duduclaw-core cannot depend on the migration crate (equality is
/// tested in duduclaw-cli and duduclaw-gateway, which see both).
pub const DURABLE_IMPORT_MANIFEST: &str = "/etc/duduclaw/migration-import.json";

/// The root-owned facts of an isolation install. The installer's fixed-path
/// binaries (`protected_linux_layout::BINARIES`) are all included (tested in
/// duduclaw-cli).
pub const FACTS: &[&str] = &[
    duduclaw_isolation::ids::SPAWND_SOCKET,
    "/etc/systemd/system/duduclaw-spawnd.socket",
    "/etc/systemd/system/duduclaw-spawnd.service",
    "/var/lib/duduclaw-spawnd",
    "/opt/duduclaw-isolated/current",
    "/var/lib/duduclaw-updated/state.json",
    DURABLE_IMPORT_MANIFEST,
    "/usr/lib/duduclaw/duduclaw-spawnd",
    "/usr/lib/duduclaw/duduclaw-run",
    "/opt/duduclaw/bin/duduclaw-egress",
    "/usr/libexec/duduclaw/duduclaw-parse-worker",
    "/usr/lib/duduclaw/current/duduclaw-import",
    "/usr/lib/duduclaw/current/duduclaw-migrate",
    "/usr/lib/duduclaw/current/duduclaw-mcp-bridge",
    "/usr/libexec/duduclaw-updated-launch",
];

static LATCHED: AtomicBool = AtomicBool::new(false);
static SYSTEM_OFF: crate::protected_mode::OffCache = crate::protected_mode::OffCache::new();

/// Only NotFound proves absence. Permission errors, ENOTDIR, a dangling
/// symlink or any existing entry count as present (fail closed).
fn present(path: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// [`FACTS`] re-rooted under `root` (`/` in production; a scratch tree in
/// tests). True when any fact is present.
pub fn installed_under(root: &Path) -> bool {
    FACTS.iter().any(|fact| present(&root.join(fact.trim_start_matches('/'))))
}

thread_local! {
    static THREAD_ROOTS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with the facts under `root` consulted IN ADDITION to the system
/// paths on this thread only (never latched process-wide). It can only turn
/// [`installed`] on, so it is safe in production; tests use it next to
/// `protected_mode::with_thread_sources`.
#[doc(hidden)]
pub fn with_thread_root<R>(root: PathBuf, f: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) { THREAD_ROOTS.with(|stack| { stack.borrow_mut().pop(); }); }
    }
    THREAD_ROOTS.with(|stack| stack.borrow_mut().push(root));
    let _pop = Pop;
    f()
}

fn system_decides() -> bool {
    #[cfg(target_os = "linux")]
    { installed_under(Path::new("/")) }
    #[cfg(not(target_os = "linux"))]
    { false }
}

/// The isolation architecture is installed on this host (see the module
/// docs). "On" latches; an "off" answer is reused for at most
/// `protected_mode::OFF_CACHE_WINDOW`, so a fact that appears later is seen
/// within that window. Thread roots are never cached and never latch.
pub fn installed() -> bool {
    if LATCHED.load(Ordering::Acquire) {
        return true;
    }
    if THREAD_ROOTS.with(|stack| stack.borrow().iter().any(|root| installed_under(root))) {
        return true;
    }
    decide_latched(&LATCHED, &SYSTEM_OFF, Instant::now(), system_decides)
}

/// A positive probe sets `latch` for good; a negative one is cached briefly.
fn decide_latched(latch: &AtomicBool, cache: &crate::protected_mode::OffCache, now: Instant,
    probe: impl FnOnce() -> bool) -> bool {
    if latch.load(Ordering::Acquire) {
        return true;
    }
    if cache.decide(now, probe) {
        latch.store(true, Ordering::Release);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(root: &Path, fact: &str) -> PathBuf {
        let path = root.join(fact.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        path
    }

    #[test]
    fn isolation_install_absent_without_any_fact() {
        let root = tempfile::tempdir().unwrap();
        assert!(!installed_under(root.path()));
        // The v1.80 single-binary protected layout is not an isolation install.
        for single_binary in [crate::protected_mode::MODE_MARKER, "/opt/duduclaw/bin/duduclaw",
            "/usr/local/libexec/duduclaw/protected-launcher", "/etc/systemd/system/duduclaw-gateway.service",
            "/var/lib/duduclaw-protected/version-floor", "/var/lib/duduclaw/.duduclaw/config.toml"] {
            place(root.path(), single_binary);
        }
        assert!(!installed_under(root.path()));
    }

    #[test]
    fn isolation_install_each_fact_alone_counts() {
        for fact in FACTS {
            let root = tempfile::tempdir().unwrap();
            place(root.path(), fact);
            assert!(installed_under(root.path()), "{fact}");
        }
        // A directory in a fact's place counts too.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("var/lib/duduclaw-spawnd")).unwrap();
        assert!(installed_under(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn isolation_install_dangling_symlink_counts() {
        for fact in FACTS {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(fact.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap();
            assert!(installed_under(root.path()), "dangling {fact}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn isolation_install_unprovable_absence_counts() {
        // ENOTDIR: a file where a fact's parent directory should be.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("run")).unwrap();
        std::fs::write(root.path().join("run/duduclaw-spawnd"), b"").unwrap();
        assert!(installed_under(root.path()));
        // EACCES: an unsearchable parent directory (meaningless as root).
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let state = root.path().join("var/lib/duduclaw-updated");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o000)).unwrap();
            let unreadable = installed_under(root.path());
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(unreadable);
        }
    }

    #[test]
    fn isolation_install_facts_are_fixed_absolute_paths() {
        for fact in FACTS {
            assert!(fact.starts_with('/') && !fact.contains(".."), "{fact}");
        }
        assert!(FACTS.contains(&"/run/duduclaw-spawnd/control.sock"));
        assert!(FACTS.contains(&DURABLE_IMPORT_MANIFEST));
    }

    #[test]
    fn isolation_install_thread_root_only_narrows_and_never_latches() {
        let root = tempfile::tempdir().unwrap();
        let before = installed();
        assert_eq!(with_thread_root(root.path().to_path_buf(), installed), before);
        place(root.path(), duduclaw_isolation::ids::SPAWND_SOCKET);
        assert!(with_thread_root(root.path().to_path_buf(), installed));
        // Popped again; the process-wide answer did not change.
        assert_eq!(installed(), before);
    }

    #[test]
    fn isolation_install_latch_holds_once_observed() {
        let root = tempfile::tempdir().unwrap();
        let (latch, cache) = (AtomicBool::new(false), crate::protected_mode::OffCache::new());
        let window = crate::protected_mode::OFF_CACHE_WINDOW;
        let start = Instant::now();
        let probe = || installed_under(root.path());
        assert!(!decide_latched(&latch, &cache, start, probe));
        let fact = place(root.path(), "/opt/duduclaw-isolated/current");
        // Inside the window the cached "off" is reused; after it the fact is seen.
        assert!(!decide_latched(&latch, &cache, start + window / 2, probe));
        assert!(decide_latched(&latch, &cache, start + window, probe));
        std::fs::remove_file(fact).unwrap();
        assert!(!probe());
        assert!(decide_latched(&latch, &cache, start + window * 3, probe), "removing the fact later never widens");
    }
}
