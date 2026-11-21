//! Per-call Codex directories (v1.84 security review H1).
//!
//! Every per-call Codex directory — the employee's owned `CODEX_HOME` with
//! its `AGENTS.md` and the `auth.json` link, and the utility working
//! directory — lives under the gateway-private `<DUDUCLAW_HOME>/run/codex/`.
//! It used to live in the user temp directory under a fixed prefix, and the
//! employee permission profile grants `:tmpdir = write` and `/tmp = write`
//! to every Codex employee's tools (all employees share one OS user), so one
//! employee could replace another call's instructions before Codex read them.
//!
//! * The parent is created 0700, must be a real directory (never a symbolic
//!   link) owned by the gateway user, and is re-restricted to 0700 on use.
//! * Each call gets a fresh `call-<32 hex>` / `utility-<32 hex>` directory
//!   named from the OS random source, created exclusively (0700).
//! * The directory is removed when its [`OwnedCallDir`] guard drops: success,
//!   error, timeout and cancellation (a dropped future drops its locals).
//! * [`sweep_stale`] removes what a crashed gateway left behind: at gateway
//!   start and at most once an hour afterwards, only entries of the parent
//!   that match the naming scheme, are real directories (symbolic links are
//!   ignored, never followed), are not in use by this process and are older
//!   than [`STALE_AFTER`].
//!
//! The employee permission profile additionally marks the parent read-only
//! for tools (see `employee_permission_args` in `codex.rs`), so the location
//! stays protected even when an operator points a working root at
//! `<DUDUCLAW_HOME>` or one of its ancestors.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Name prefix of an employee call's directory (holds `codex-home/`).
pub(crate) const CALL_PREFIX: &str = "call-";
/// Name prefix of a utility call's directory (its working directory; holds `codex-home/`).
pub(crate) const UTILITY_PREFIX: &str = "utility-";
/// Hex characters after the prefix (16 random bytes).
const NAME_HEX_LEN: usize = 32;
/// An entry this old that this process is not using belongs to no running call.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// Minimum time between two sweeps of one parent.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// `<home>/run/codex`.
pub(crate) fn run_parent(home_dir: &Path) -> PathBuf {
    home_dir.join("run").join("codex")
}

/// The host Codex credential store directory: the gateway process's
/// `$CODEX_HOME`, or `~/.codex`.
pub(crate) fn host_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

/// Directories the employee permission profile must keep away from tools:
/// the owned-home parent and the host credential store, each also in its
/// canonical form when that differs (a link in the path must not let the
/// other spelling escape the entry). The profile is Unix-only.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn protected_roots(home_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_absolute() && !roots.contains(&path) { roots.push(path); }
    };
    let parent = run_parent(home_dir);
    if let Ok(canonical) = std::fs::canonicalize(&parent) { push(canonical); }
    push(parent);
    if let Some(host) = host_codex_home() {
        if let Ok(auth) = std::fs::canonicalize(host.join("auth.json")) {
            if let Some(dir) = auth.parent() { push(dir.to_path_buf()); }
        }
        if let Ok(canonical) = std::fs::canonicalize(&host) { push(canonical); }
        push(host);
    }
    roots
}

fn private_dir_builder() -> std::fs::DirBuilder {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// Create `dir` (0700) when absent; refuse a link, a non-directory or a
/// directory owned by another user.
fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    match private_dir_builder().create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create Codex run directory: {e}")),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect Codex run directory: {e}"))?;
    if !meta.file_type().is_dir() {
        return Err("Codex run directory is not a plain directory — refusing to use it".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != nix::unistd::geteuid().as_raw() {
            return Err("Codex run directory is not owned by the gateway user — refusing to use it".into());
        }
    }
    Ok(())
}

/// `<home>/run/codex`, created and restricted to 0700.
pub(crate) fn ensure_run_parent(home_dir: &Path) -> Result<PathBuf, String> {
    ensure_private_dir(&home_dir.join("run"))?;
    let parent = run_parent(home_dir);
    ensure_private_dir(&parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict Codex run directory: {e}"))?;
    }
    Ok(parent)
}

fn random_name(prefix: &str) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; NAME_HEX_LEN / 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

/// Does `name` follow this module's naming scheme exactly?
pub(crate) fn matches_scheme(name: &str) -> bool {
    [CALL_PREFIX, UTILITY_PREFIX].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|rest| rest.len() == NAME_HEX_LEN
            && rest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    })
}

fn live() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static LIVE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    LIVE.get_or_init(Default::default)
}

fn last_sweeps() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Instant>> {
    static LAST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Instant>>> = std::sync::OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// Claim the sweep slot of `parent` when the last sweep is at least
/// [`SWEEP_INTERVAL`] old (or there was none).
fn claim_sweep(parent: &Path) -> bool {
    let mut last = last_sweeps().lock().unwrap_or_else(|e| e.into_inner());
    match last.get(parent) {
        Some(at) if at.elapsed() < SWEEP_INTERVAL => false,
        _ => {
            last.insert(parent.to_path_buf(), Instant::now());
            true
        }
    }
}

/// One call's private directory under `<home>/run/codex`; removed on drop.
#[derive(Debug)]
pub(crate) struct OwnedCallDir {
    path: PathBuf,
}

impl OwnedCallDir {
    pub(crate) fn create(home_dir: &Path, prefix: &str) -> Result<Self, String> {
        let parent = ensure_run_parent(home_dir)?;
        if claim_sweep(&parent) {
            sweep_older_than(&parent, STALE_AFTER);
        }
        for _ in 0..8 {
            let path = parent.join(random_name(prefix));
            // `create`, not `create_all`: an existing entry (or a planted
            // link) is never reused.
            match private_dir_builder().create(&path) {
                Ok(()) => {
                    live().lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone());
                    let guard = Self { path };
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&guard.path, std::fs::Permissions::from_mode(0o700))
                            .map_err(|e| format!("cannot restrict Codex call directory: {e}"))?;
                    }
                    return Ok(guard);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("cannot create Codex call directory: {e}")),
            }
        }
        Err("cannot create a unique Codex call directory".into())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnedCallDir {
    fn drop(&mut self) {
        // `remove_dir_all` removes a symbolic link inside the tree (such as
        // `codex-home/auth.json`) as a link; it never follows it.
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "could not remove a Codex call directory; the next sweep retries");
            }
        }
        live().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.path);
    }
}

/// Remove call directories a stopped gateway left behind. Called at gateway
/// start; returns how many were removed.
pub fn sweep_stale(home_dir: &Path) -> usize {
    let parent = run_parent(home_dir);
    last_sweeps().lock().unwrap_or_else(|e| e.into_inner()).insert(parent.clone(), Instant::now());
    sweep_older_than(&parent, STALE_AFTER)
}

pub(crate) fn sweep_older_than(parent: &Path, max_age: Duration) -> usize {
    // The parent itself must be a real directory: a link there is not followed.
    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return 0,
    }
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(matches_scheme) {
            continue;
        }
        let path = entry.path();
        // Links (and plain files) are ignored, never followed or removed.
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_dir() {
            continue;
        }
        if live().lock().unwrap_or_else(|e| e.into_inner()).contains(&path) {
            continue;
        }
        let old = meta.modified().ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if old && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_directory_is_private_random_and_under_the_run_parent() {
        let home = tempfile::tempdir().unwrap();
        let a = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        let b = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        for dir in [&a, &b] {
            assert_eq!(dir.path().parent(), Some(run_parent(home.path()).as_path()));
            let name = dir.path().file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with(CALL_PREFIX) && matches_scheme(name), "{name}");
            assert!(dir.path().is_dir());
        }
        assert_ne!(a.path(), b.path(), "names are random, not sequential");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [a.path(), run_parent(home.path()).as_path()] {
                assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o700, "{path:?}");
            }
        }
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists(), "removed when the guard drops");
    }

    #[test]
    fn naming_scheme_is_exact() {
        let hex = "0123456789abcdef0123456789abcdef";
        assert!(matches_scheme(&format!("call-{hex}")));
        assert!(matches_scheme(&format!("utility-{hex}")));
        for name in [format!("call-{}", &hex[1..]), format!("call-{hex}0"), format!("call-{}", hex.to_uppercase()),
            format!("other-{hex}"), "duduclaw-codex-config-abc".to_string(), format!("call-{hex}/x")] {
            assert!(!matches_scheme(&name), "{name}");
        }
    }

    #[tokio::test]
    async fn cancelled_call_removes_its_directory() {
        let home = tempfile::tempdir().unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let call = {
            let home = home.path().to_path_buf();
            async move {
                let dir = OwnedCallDir::create(&home, CALL_PREFIX).unwrap();
                sender.send(dir.path().to_path_buf()).unwrap();
                std::future::pending::<()>().await;
                drop(dir);
            }
        };
        let task = tokio::spawn(call);
        let path = receiver.await.unwrap();
        assert!(path.is_dir());
        task.abort();
        let _ = task.await;
        assert!(!path.exists(), "a cancelled call leaves nothing behind");
    }

    #[test]
    fn a_planted_parent_link_is_refused() {
        #[cfg(unix)]
        {
            let home = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            std::fs::create_dir(home.path().join("run")).unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), run_parent(home.path())).unwrap();
            assert!(OwnedCallDir::create(home.path(), CALL_PREFIX).is_err());
            assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn sweeper_removes_only_matching_stale_directories_and_ignores_links() {
        let home = tempfile::tempdir().unwrap();
        let parent = ensure_run_parent(home.path()).unwrap();
        let hex = "00112233445566778899aabbccddeeff";
        let stale_call = parent.join(format!("call-{hex}"));
        let stale_utility = parent.join(format!("utility-{hex}"));
        for dir in [&stale_call, &stale_utility] {
            std::fs::create_dir_all(dir.join("codex-home")).unwrap();
            std::fs::write(dir.join("codex-home/AGENTS.md"), "old prompt").unwrap();
        }
        let foreign = parent.join("keep-me");
        std::fs::create_dir(&foreign).unwrap();
        let file = parent.join(format!("call-{}", "f".repeat(32)));
        std::fs::write(&file, "not a directory").unwrap();
        let live_dir = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        #[cfg(unix)]
        let (target, link) = {
            let target = tempfile::tempdir().unwrap();
            std::fs::write(target.path().join("precious"), "x").unwrap();
            let link = parent.join(format!("call-{}", "e".repeat(32)));
            std::os::unix::fs::symlink(target.path(), &link).unwrap();
            (target, link)
        };
        // Fresh entries survive the real threshold.
        assert_eq!(sweep_older_than(&parent, STALE_AFTER), 0);
        assert!(stale_call.exists() && stale_utility.exists());
        // Everything is "old" at a zero threshold: only the matching, non-live directories go.
        assert_eq!(sweep_older_than(&parent, Duration::ZERO), 2);
        assert!(!stale_call.exists() && !stale_utility.exists());
        assert!(foreign.is_dir() && file.is_file() && live_dir.path().is_dir());
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert!(target.path().join("precious").exists(), "a link is never followed");
        }
        // A link as the parent itself is not followed either.
        #[cfg(unix)]
        {
            let other = tempfile::tempdir().unwrap();
            let victim = other.path().join(format!("call-{hex}"));
            std::fs::create_dir(&victim).unwrap();
            let linked_parent = home.path().join("linked");
            std::os::unix::fs::symlink(other.path(), &linked_parent).unwrap();
            assert_eq!(sweep_older_than(&linked_parent, Duration::ZERO), 0);
            assert!(victim.exists());
        }
    }

    #[test]
    fn startup_sweep_claims_the_hourly_slot() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(sweep_stale(home.path()), 0, "absent parent: nothing to do");
        assert!(!claim_sweep(&run_parent(home.path())), "at most once an hour");
        let other = tempfile::tempdir().unwrap();
        assert!(claim_sweep(&run_parent(other.path())), "per parent");
    }

    #[test]
    fn protected_roots_name_the_run_parent_and_the_host_store() {
        let home = tempfile::tempdir().unwrap();
        let roots = protected_roots(home.path());
        assert!(roots.contains(&run_parent(home.path())));
        if let Some(host) = host_codex_home().filter(|h| h.is_absolute()) {
            assert!(roots.contains(&host), "{roots:?}");
        }
        assert!(roots.iter().all(|root| root.is_absolute()));
    }
}
