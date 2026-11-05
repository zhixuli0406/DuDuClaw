//! Restart requests only; this module can never install or resurrect a key.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use serde::{Deserialize, Serialize};

const STATUS_FILE: &str = "root-key-status.json";
const BUDGET_FILE: &str = "root-key-recovery.json";
const MAX_RESTARTS: usize = 4;
static REQUESTED: AtomicBool = AtomicBool::new(false);
static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revocation {
    pub reason: String,
    pub detail: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryStatus {
    pub latest_revocation: Option<Revocation>,
    pub recovery: String,
}

#[derive(Default)]
struct Runtime {
    home: Option<PathBuf>,
    status: Option<RecoveryStatus>,
}

fn runtime() -> std::sync::MutexGuard<'static, Runtime> {
    RUNTIME.get_or_init(|| Mutex::new(Runtime::default())).lock().unwrap_or_else(|p| p.into_inner())
}

pub fn restart_requested() -> bool { REQUESTED.load(Ordering::Acquire) }

/// An original non-transient refusal is never eligible, even if later probes pass.
pub(super) fn recoverable(reason: &str) -> bool { reason == "engine_socket_reachable" }

#[derive(Default)]
pub(super) struct ConsecutivePasses { passes: u8, triggered: bool }
impl ConsecutivePasses {
    pub(super) fn check(&mut self, passed: bool) -> bool {
        if !passed { self.passes = 0; return false; }
        if self.triggered { return false; }
        self.passes += 1;
        if self.passes == 3 { self.triggered = true; true } else { false }
    }
    pub(super) fn retry(&mut self) { self.passes = 0; self.triggered = false; }
}

pub(super) fn record_refusal(reason: &str, detail: Option<&str>) {
    let mut rt = runtime();
    rt.status = Some(RecoveryStatus {
        latest_revocation: Some(Revocation {
            reason: reason.into(), detail: detail.map(str::to_owned),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }),
        recovery: if recoverable(reason) { "waiting" } else { "non_transient" }.into(),
    });
    persist_status(&mut rt);
}

fn persist_status(rt: &mut Runtime) -> bool {
    let (Some(home), Some(status)) = (&rt.home, &rt.status) else { return false; };
    if let Err(error) = write_json(&home.join("state").join(STATUS_FILE), status) {
        tracing::warn!(%error, "Root key recovery status could not be persisted; automatic restart disabled");
        if let Some(status) = &mut rt.status { status.recovery = "state_unavailable".into(); }
        return false;
    }
    true
}

/// Called after the gateway owns its instance lock and has applied pending restore.
/// No files are created by CLI status calls or ordinary unprotected installs.
pub fn initialize(home: &Path) {
    if super::state_kind() == super::StateKind::NotOffered
        || std::env::var_os(super::MODE_ENV).as_deref() != Some(std::ffi::OsStr::new("native-systemd")) {
        return;
    }
    let mut rt = runtime();
    if rt.home.is_some() { return; }
    rt.home = Some(home.to_path_buf());
    let state_dir = home.join("state");
    let prepared = std::fs::create_dir_all(&state_dir).and_then(|()| initialize_budget(&state_dir));
    if rt.status.is_none() {
        rt.status = read_json(&state_dir.join(STATUS_FILE)).ok();
        if let Some(status) = &mut rt.status { status.recovery = "monitoring".into(); }
    }
    if let Err(error) = prepared {
        tracing::warn!(%error, "Root key recovery state unavailable; automatic restart disabled");
        let status = rt.status.get_or_insert(RecoveryStatus { latest_revocation: None, recovery: String::new() });
        status.recovery = "state_unavailable".into();
    }
    if rt.status.is_some() { persist_status(&mut rt); }
}

/// Persisted events are history, never evidence that this reader holds a key.
pub fn status(home: &Path) -> Option<RecoveryStatus> {
    let rt = runtime();
    if rt.home.as_deref() == Some(home) { return rt.status.clone(); }
    drop(rt);
    read_json(&home.join("state").join(STATUS_FILE)).ok()
}

fn auto_recover(home: &Path) -> bool {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => parse_auto_recover(&text),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn parse_auto_recover(text: &str) -> bool {
    let Ok(value) = text.parse::<toml::Value>() else { return false; };
    let Some(section) = value.get("credential_protection") else { return true; };
    let Some(section) = section.as_table() else { return false; };
    match section.get("auto_recover") { None => true, Some(value) => value.as_bool() == Some(true) }
}

/// Called with the key state lock held, so Held cannot race a restart request.
/// A budget reservation is durable before the request is made visible.
pub(super) fn tick(kind: super::StateKind, reason: Option<&str>, passed: bool, counter: &mut ConsecutivePasses) {
    if kind != super::StateKind::Refused || !reason.is_some_and(recoverable) || restart_requested() {
        counter.check(false);
        return;
    }
    let mut rt = runtime();
    let Some(home) = rt.home.clone() else { counter.check(false); return; };
    let desired = if !auto_recover(&home) {
        counter.check(false);
        "disabled"
    } else if !counter.check(passed) {
        "waiting"
    } else {
        // A missing/corrupt/unwritable budget cannot be reinitialized here.
        match reserve_restart(&home.join("state"), chrono::Utc::now().timestamp()) {
            Ok(true) => "restart_requested",
            Ok(false) => { counter.retry(); "budget_exhausted" }
            Err(error) => {
                tracing::warn!(%error, "Root key recovery budget unavailable; staying refused");
                counter.retry();
                "state_unavailable"
            }
        }
    };
    let Some(status) = &mut rt.status else { counter.retry(); return; };
    // Avoid erasing a blocked diagnosis on the intervening two checks.
    if desired != "waiting" || !matches!(status.recovery.as_str(), "budget_exhausted" | "state_unavailable") {
        status.recovery = desired.into();
    }
    if persist_status(&mut rt) && desired == "restart_requested" {
        REQUESTED.store(true, Ordering::Release);
        tracing::warn!(exit_code = super::RECOVERY_RESTART_EXIT_CODE, "Root key recovery requests graceful restart and fresh root intake");
    } else if desired == "restart_requested" { counter.retry(); }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget { restarts: Vec<i64> }

fn budget_lock(dir: &Path) -> std::io::Result<std::fs::File> {
    use fs2::FileExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(dir.join("root-key-recovery.lock"))?;
    if !file.metadata()?.is_file() { return Err(std::io::Error::other("invalid recovery lock")); }
    file.try_lock_exclusive()?;
    Ok(file)
}

fn initialize_budget(dir: &Path) -> std::io::Result<()> {
    let _lock = budget_lock(dir)?;
    let path = dir.join(BUDGET_FILE);
    // Only a genuinely absent file on boot may start a new budget.
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => write_json(&path, &Budget::default()),
        Err(e) => Err(e),
        Ok(_) => read_json::<Budget>(&path).map(|_| ()),
    }
}

fn reserve_restart(dir: &Path, now: i64) -> std::io::Result<bool> {
    let _lock = budget_lock(dir)?;
    let path = dir.join(BUDGET_FILE);
    let mut budget: Budget = read_json(&path)?;
    // Future timestamps remain charged if the wall clock moves backwards.
    budget.restarts.retain(|timestamp| *timestamp > now.saturating_sub(3600));
    if budget.restarts.len() >= MAX_RESTARTS { return Ok(false); }
    budget.restarts.push(now);
    write_json(&path, &budget)?;
    Ok(true)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() { return Err(std::io::Error::other("invalid recovery state file")); }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 { return Err(std::io::Error::other("recovery state file too large")); }
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| std::io::Error::other("missing state directory"))?;
    let temp = parent.join(format!(".root-key-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(value).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        // Production recovery runs on Linux. Keep the pure logic fixtures
        // portable where opening a directory as a File is not supported.
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_key_consecutive_passes_reset_and_trigger_once() {
        let mut counter = ConsecutivePasses::default();
        for passed in [true, true, false, true, true] { assert!(!counter.check(passed)); }
        assert!(counter.check(true));
        for passed in [true, false, true, true, true] { assert!(!counter.check(passed)); }
    }
    #[test]
    fn root_key_held_and_nontransient_never_request_restart() {
        let mut counter = ConsecutivePasses::default();
        for _ in 0..4 {
            tick(super::super::StateKind::Held, Some("engine_socket_reachable"), true, &mut counter);
            tick(super::super::StateKind::Refused, Some("exe_owner"), true, &mut counter);
        }
        assert_eq!(counter.passes, 0);
        assert!(!counter.triggered);
        assert!(!restart_requested());
    }

    #[cfg(unix)]
    #[test]
    fn root_key_budget_read_and_write_permission_failures_are_closed() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 { return; }
        let dir = tempfile::tempdir().unwrap();
        initialize_budget(dir.path()).unwrap();
        let path = dir.path().join(BUDGET_FILE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(reserve_restart(dir.path(), 1000).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(reserve_restart(dir.path(), 1000).is_err());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(read_json::<Budget>(&path).unwrap().restarts.is_empty());
    }

    #[test]
    fn root_key_reason_allowlist_and_config() {
        assert!(recoverable("engine_socket_reachable"));
        for reason in ["engine_socket", "supplementary_groups", "exe_owner", "exe_writable", "cgroup_unit",
            "user_namespace", "container_environment", "key_path_accessible", "unknown"] {
            assert!(!recoverable(reason));
        }
        assert!(parse_auto_recover(""));
        assert!(parse_auto_recover("[credential_protection]\nauto_recover = true"));
        for config in ["[", "credential_protection = false", "[credential_protection]\nauto_recover = false",
            "[credential_protection]\nauto_recover = 'true'"] { assert!(!parse_auto_recover(config)); }
    }
    #[test]
    fn root_key_hourly_budget_survives_restarts_and_clock_rollback() {
        let dir = tempfile::tempdir().unwrap();
        initialize_budget(dir.path()).unwrap();
        for now in [1000, 1010, 1020, 1030] { assert!(reserve_restart(dir.path(), now).unwrap()); }
        initialize_budget(dir.path()).unwrap();
        assert!(!reserve_restart(dir.path(), 999).unwrap());
        assert!(!reserve_restart(dir.path(), 4599).unwrap());
        assert!(reserve_restart(dir.path(), 4600).unwrap());
        assert!(!reserve_restart(dir.path(), 4600).unwrap());
        assert!(reserve_restart(dir.path(), 4610).unwrap());
    }
    #[test]
    fn root_key_unreadable_or_unwritable_budget_refuses_restart() {
        let dir = tempfile::tempdir().unwrap();
        assert!(reserve_restart(dir.path(), 1000).is_err());
        initialize_budget(dir.path()).unwrap();
        let path = dir.path().join(BUDGET_FILE);
        std::fs::write(&path, "broken").unwrap();
        assert!(initialize_budget(dir.path()).is_err());
        assert!(reserve_restart(dir.path(), 1000).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(reserve_restart(dir.path(), 1000).is_err());
        assert!(write_json(&path, &Budget::default()).is_err());
    }
    #[test]
    fn root_key_recovery_orchestration_in_isolated_process() {
        const CASE: &str = "DUDUCLAW_TEST_KEY_RECOVERY_CASE";
        let Ok(case) = std::env::var(CASE) else {
            for case in ["success", "disabled", "status_unwritable", "budget_unreadable"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "root_key::recovery::tests::root_key_recovery_orchestration_in_isolated_process", "--nocapture"])
                    .env(CASE, case).output().unwrap();
                assert!(output.status.success(), "{case}: {} {}",
                    String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            }
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::fs::create_dir(&state_dir).unwrap();
        initialize_budget(&state_dir).unwrap();
        {
            let mut rt = runtime();
            rt.home = Some(dir.path().to_owned());
            rt.status = Some(RecoveryStatus { latest_revocation: None, recovery: "waiting".into() });
        }
        if case == "disabled" {
            std::fs::write(dir.path().join("config.toml"), "[credential_protection]\nauto_recover = false").unwrap();
        } else if case == "status_unwritable" {
            std::fs::create_dir(state_dir.join(STATUS_FILE)).unwrap();
        } else if case == "budget_unreadable" {
            std::fs::write(state_dir.join(BUDGET_FILE), "broken").unwrap();
        }
        let mut counter = ConsecutivePasses::default();
        for _ in 0..2 {
            tick(super::super::StateKind::Refused, Some("engine_socket_reachable"), true, &mut counter);
            assert!(!restart_requested());
        }
        tick(super::super::StateKind::Refused, Some("engine_socket_reachable"), true, &mut counter);
        assert_eq!(restart_requested(), case == "success");
        if case == "success" {
            assert_eq!(read_json::<Budget>(&state_dir.join(BUDGET_FILE)).unwrap().restarts.len(), 1);
            assert_eq!(read_json::<RecoveryStatus>(&state_dir.join(STATUS_FILE)).unwrap().recovery, "restart_requested");
            for _ in 0..4 { tick(super::super::StateKind::Refused, Some("engine_socket_reachable"), true, &mut counter); }
            assert_eq!(read_json::<Budget>(&state_dir.join(BUDGET_FILE)).unwrap().restarts.len(), 1);
        } else {
            assert_eq!(status(dir.path()).unwrap().recovery,
                if case == "disabled" { "disabled" } else { "state_unavailable" });
        }
    }

    #[test]
    fn root_key_status_roundtrip_has_exact_detail_and_utc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("state")).unwrap();
        let status = RecoveryStatus { latest_revocation: Some(Revocation {
            reason: "engine_socket_reachable".into(), detail: Some("@/containerd-shim/test: peer uid=0 (root)".into()),
            timestamp: "2026-11-05T01:02:03Z".into(),
        }), recovery: "waiting".into() };
        write_json(&dir.path().join("state").join(STATUS_FILE), &status).unwrap();
        let read = super::status(dir.path()).unwrap();
        assert_eq!(read.latest_revocation.unwrap().detail, status.latest_revocation.unwrap().detail);
        assert_eq!(read.recovery, "waiting");
    }
}
