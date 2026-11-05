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

// These files are advisory diagnostics in the service UID's home. Only the
// root-managed systemd start limit is authoritative against same-UID tampering.
#[path = "root_key_recovery_files.rs"]
mod files;
use files::StateDir;

macro_rules! diagnostic_enum {
    ($name:ident { $($variant:ident => $code:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $code)] $variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $code),+ } }
        }
        // Internal callers use constants. Unknown runtime codes are represented
        // by a fixed value; untrusted JSON must match an explicit serde variant.
        impl From<&str> for $name {
            fn from(value: &str) -> Self { match value { $($code => Self::$variant),+, _ => Self::Unknown } }
        }
    };
}

diagnostic_enum! { RevocationReason {
    UserNamespace => "user_namespace",
    ContainerEnvironment => "container_environment",
    KeyStatePoisoned => "key_state_poisoned",
    ReattestUnavailable => "reattest_unavailable",
    Mode => "mode",
    NotSystemdChild => "not_systemd_child",
    CgroupV1 => "cgroup_v1",
    CgroupUnit => "cgroup_unit",
    ExeStat => "exe_stat",
    ExeType => "exe_type",
    ExeOwner => "exe_owner",
    ExeWritable => "exe_writable",
    ExeAncestorType => "exe_ancestor_type",
    ExeAncestorOwner => "exe_ancestor_owner",
    ExeAncestorWritable => "exe_ancestor_writable",
    SupplementaryGroups => "supplementary_groups",
    EngineSocketReachable => "engine_socket_reachable",
    EngineSocket => "engine_socket",
    MountScanIncomplete => "mount_scan_incomplete",
    Inactive => "inactive",
    FdContract => "fd_contract",
    FdStat => "fd_stat",
    FdType => "fd_type",
    FdOwner => "fd_owner",
    FdMode => "fd_mode",
    FdLinks => "fd_links",
    FdSize => "fd_size",
    FdRead => "fd_read",
    NotPidOne => "not_pid_one",
    UserIds => "user_ids",
    GroupIds => "group_ids",
    ProcStatus => "proc_status",
    NoNewPrivs => "no_new_privs",
    CapInh => "cap_inh",
    CapPrm => "cap_prm",
    CapEff => "cap_eff",
    CapBnd => "cap_bnd",
    CapAmb => "cap_amb",
    Dumpable => "dumpable",
    KeyPathAccessible => "key_path_accessible",
    DockerSocket => "docker_socket",
    DockerHost => "docker_host",
    Unknown => "unknown",
} }

diagnostic_enum! { RecoveryState {
    Monitoring => "monitoring",
    Waiting => "waiting",
    Disabled => "disabled",
    BudgetExhausted => "budget_exhausted",
    RestartRequested => "restart_requested",
    StateUnavailable => "state_unavailable",
    NonTransient => "non_transient",
    Unknown => "unknown",
} }

/// Escape terminal controls, bidi/format characters and other non-printables.
/// Rust's Unicode-aware escape_debug covers non-printable categories, including
/// unassigned characters; printable CJK text remains readable. This is idempotent.
pub fn escape_diagnostic_text(input: &str) -> String {
    let mut result = String::new();
    for ch in input.chars() {
        let escaped = ch.escape_debug().to_string();
        if ch.is_control() || escaped.starts_with("\\u{")
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            result.push_str(&format!("\\u{{{:x}}}", ch as u32));
        } else {
            result.push(ch);
        }
    }
    result
}

/// Bound both input work and the escaped payload to 512 UTF-8 bytes.
pub fn sanitize_diagnostic(input: &str) -> String {
    let escaped = escape_diagnostic_text(crate::truncate_bytes(input, 512));
    crate::truncate_bytes(&escaped, 512).to_owned()
}

fn deserialize_detail<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(|value| sanitize_diagnostic(&value)))
}
fn serialize_detail<S: serde::Serializer>(value: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    value.as_deref().map(sanitize_diagnostic).serialize(s)
}
fn deserialize_timestamp<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&value).map_err(serde::de::Error::custom)?;
    Ok(timestamp.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
fn serialize_timestamp<S: serde::Serializer>(value: &str, s: S) -> Result<S::Ok, S::Error> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(value).map_err(serde::ser::Error::custom)?;
    timestamp.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true).serialize(s)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub reason: RevocationReason,
    #[serde(default, deserialize_with = "deserialize_detail", serialize_with = "serialize_detail")]
    pub detail: Option<String>,
    #[serde(deserialize_with = "deserialize_timestamp", serialize_with = "serialize_timestamp")]
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryStatus {
    pub latest_revocation: Option<Revocation>,
    pub recovery: RecoveryState,
}

#[derive(Default)]
struct Runtime {
    home: Option<PathBuf>,
    state_dir: Option<StateDir>,
    state_ready: bool,
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
            reason: reason.into(), detail: detail.map(sanitize_diagnostic),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }),
        recovery: if recoverable(reason) { "waiting" } else { "non_transient" }.into(),
    });
    persist_status(&mut rt);
}

fn persist_status(rt: &mut Runtime) -> bool {
    let (Some(dir), Some(status)) = (&rt.state_dir, &rt.status) else { return false; };
    let result = match dir.read_json::<RecoveryStatus>(STATUS_FILE) {
        Ok(_) => dir.write_json(STATUS_FILE, status),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => dir.write_json(STATUS_FILE, status),
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(error = %sanitize_diagnostic(&error.to_string()), "Root key recovery status could not be persisted; automatic restart disabled");
        rt.state_ready = false;
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
    let prepared = StateDir::open_home(home, true).and_then(|dir| {
        initialize_budget_at(&dir)?;
        let historical = match dir.read_json::<RecoveryStatus>(STATUS_FILE) {
            Ok(status) => Some(status),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if rt.status.is_none() {
            rt.status = historical;
            if let Some(status) = &mut rt.status { status.recovery = "monitoring".into(); }
        }
        rt.state_dir = Some(dir);
        rt.state_ready = true;
        Ok(())
    });
    if let Err(error) = prepared {
        tracing::warn!(error = %sanitize_diagnostic(&error.to_string()), "Root key recovery state unavailable; automatic restart disabled");
        let status = rt.status.get_or_insert(RecoveryStatus { latest_revocation: None, recovery: RecoveryState::StateUnavailable });
        status.recovery = RecoveryState::StateUnavailable;
    }
    if rt.status.is_some() { persist_status(&mut rt); }
}

/// Persisted events are history, never evidence that this reader holds a key.
pub fn status(home: &Path) -> Option<RecoveryStatus> {
    let rt = runtime();
    if rt.home.as_deref() == Some(home) { return rt.status.clone(); }
    drop(rt);
    StateDir::open_home(home, false).and_then(|dir| dir.read_json(STATUS_FILE)).ok()
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
    let desired = if !rt.state_ready {
        counter.check(false);
        "state_unavailable"
    } else if !auto_recover(&home) {
        counter.check(false);
        "disabled"
    } else if !counter.check(passed) {
        "waiting"
    } else {
        // A missing/corrupt/unwritable budget cannot be reinitialized here.
        match rt.state_dir.as_ref().ok_or_else(|| std::io::Error::other("missing pinned state directory"))
            .and_then(|dir| reserve_restart_at(dir, chrono::Utc::now().timestamp())) {
            Ok(true) => "restart_requested",
            Ok(false) => { counter.retry(); "budget_exhausted" }
            Err(error) => {
                tracing::warn!(error = %sanitize_diagnostic(&error.to_string()), "Root key recovery budget unavailable; staying refused");
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

fn initialize_budget_at(dir: &StateDir) -> std::io::Result<()> {
    let _lock = dir.budget_lock()?;
    match dir.read_json::<Budget>(BUDGET_FILE) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => dir.write_json(BUDGET_FILE, &Budget::default()),
        Err(e) => Err(e),
        Ok(_) => Ok(()),
    }
}

fn reserve_restart_at(dir: &StateDir, now: i64) -> std::io::Result<bool> {
    let _lock = dir.budget_lock()?;
    let mut budget: Budget = dir.read_json(BUDGET_FILE)?;
    // This rolling-hour count is advisory. Same-UID file replacement cannot
    // bypass the root-managed systemd unit's authoritative start rate limit.
    budget.restarts.retain(|timestamp| *timestamp > now.saturating_sub(3600));
    if budget.restarts.len() >= MAX_RESTARTS { return Ok(false); }
    budget.restarts.push(now);
    dir.write_json(BUDGET_FILE, &budget)?;
    Ok(true)
}

#[cfg(all(test, unix))]
fn initialize_budget(dir: &Path) -> std::io::Result<()> { initialize_budget_at(&StateDir::open(dir)?) }
#[cfg(all(test, unix))]
fn reserve_restart(dir: &Path, now: i64) -> std::io::Result<bool> { reserve_restart_at(&StateDir::open(dir)?, now) }
#[cfg(all(test, unix))]
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    StateDir::open(path.parent().unwrap())?.read_json(path.file_name().unwrap().to_str().unwrap())
}
#[cfg(all(test, unix))]
fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    StateDir::open(path.parent().unwrap())?.write_json(path.file_name().unwrap().to_str().unwrap(), value)
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    #[test]
    fn root_key_recovery_orchestration_in_isolated_process() {
        const CASE: &str = "DUDUCLAW_TEST_KEY_RECOVERY_CASE";
        let Ok(case) = std::env::var(CASE) else {
            for case in ["success", "disabled", "status_unwritable", "status_unreadable", "budget_unreadable"] {
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
            rt.state_dir = Some(StateDir::open(&state_dir).unwrap());
            rt.state_ready = true;
            rt.status = Some(RecoveryStatus { latest_revocation: None, recovery: "waiting".into() });
        }
        if case == "disabled" {
            std::fs::write(dir.path().join("config.toml"), "[credential_protection]\nauto_recover = false").unwrap();
        } else if case == "status_unwritable" {
            std::fs::create_dir(state_dir.join(STATUS_FILE)).unwrap();
        } else if case == "status_unreadable" {
            write_json(&state_dir.join(STATUS_FILE), &RecoveryStatus { latest_revocation: None, recovery: "waiting".into() }).unwrap();
            std::fs::write(state_dir.join(STATUS_FILE), "broken").unwrap();
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
            assert_eq!(read_json::<RecoveryStatus>(&state_dir.join(STATUS_FILE)).unwrap().recovery.as_str(), "restart_requested");
            for _ in 0..4 { tick(super::super::StateKind::Refused, Some("engine_socket_reachable"), true, &mut counter); }
            assert_eq!(read_json::<Budget>(&state_dir.join(BUDGET_FILE)).unwrap().restarts.len(), 1);
        } else {
            assert_eq!(status(dir.path()).unwrap().recovery.as_str(),
                if case == "disabled" { "disabled" } else { "state_unavailable" });
        }
    }

    #[cfg(unix)]
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
        assert_eq!(read.recovery.as_str(), "waiting");
    }
    fn untrusted_status(detail: &str) -> serde_json::Value {
        serde_json::json!({"latest_revocation": {
            "reason": "engine_socket_reachable", "detail": detail,
            "timestamp": "2026-11-05T09:02:03+08:00"
        }, "recovery": "waiting"})
    }

    #[test]
    fn root_key_diagnostics_validate_codes_timestamp_and_escape_controls() {
        let controls = "保持中文\u{0}\u{7}\u{1b}]52;fake\n\r\t\u{7f}\u{80}\u{9f}\u{61c}\u{200b}\u{200e}\u{202e}\u{2066}\u{2069}\u{2028}\u{2029}";
        let input = untrusted_status(controls);
        let status: RecoveryStatus = serde_json::from_value(input.clone()).unwrap();
        let revocation = status.latest_revocation.unwrap();
        assert_eq!(revocation.timestamp, "2026-11-05T01:02:03Z");
        let detail = revocation.detail.clone().unwrap();
        assert!(detail.starts_with("保持中文\\u{0}"));
        assert!(!detail.chars().any(char::is_control));
        for code in ["1b", "7f", "80", "9f", "61c", "200b", "200e", "202e", "2066", "2069", "2028", "2029"] {
            assert!(detail.contains(&format!("\\u{{{code}}}")), "missing {code}: {detail}");
        }
        assert_eq!(sanitize_diagnostic(&detail), detail);
        // Serialization also sanitizes manually constructed runtime values.
        let raw = Revocation { detail: Some(controls.into()), ..revocation };
        let serialized = serde_json::to_value(raw).unwrap();
        assert_eq!(serialized["detail"], detail);
        for field in ["reason", "timestamp"] {
            for invalid in ["forged", "\u{1b}[2J", "2026-11-05T01:02:03Z\n", "2026-99-05T01:02:03Z"] {
                let mut forged = input.clone();
                forged["latest_revocation"][field] = invalid.into();
                assert!(serde_json::from_value::<RecoveryStatus>(forged).is_err(), "{field}: {invalid:?}");
            }
        }
        let mut forged = input;
        forged["recovery"] = "injected\u{1b}".into();
        assert!(serde_json::from_value::<RecoveryStatus>(forged).is_err());
    }

    #[test]
    fn root_key_diagnostic_detail_is_bounded_for_unicode_and_escape_expansion() {
        for input in ["界".repeat(600), "\u{1b}".repeat(600), "a".repeat(600)] {
            let status: RecoveryStatus = serde_json::from_value(untrusted_status(&input)).unwrap();
            let detail = status.latest_revocation.unwrap().detail.unwrap();
            assert!(detail.len() <= 512);
            assert!(!detail.chars().any(char::is_control));
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_key_state_files_refuse_symlinks_hardlinks_modes_and_wrong_owner() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let outside = home.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let link = home.path().join("linked-state");
        symlink(&state, &link).unwrap();
        assert!(StateDir::open(&link).is_err());
        let dir = StateDir::open(&state).unwrap();
        for name in [BUDGET_FILE, STATUS_FILE, "root-key-recovery.lock"] {
            let path = state.join(name);
            let target = outside.join(name);
            std::fs::write(&target, "{}").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, &path).unwrap();
            assert!(dir.read_json::<serde_json::Value>(name).is_err());
            assert!(dir.write_json(name, &serde_json::json!({})).is_err());
            if name.ends_with(".lock") { assert!(dir.budget_lock().is_err()); }
            std::fs::remove_file(&path).unwrap();
            std::fs::hard_link(&target, &path).unwrap();
            assert!(dir.read_json::<serde_json::Value>(name).is_err());
            assert!(dir.write_json(name, &serde_json::json!({})).is_err());
            if name.ends_with(".lock") { assert!(dir.budget_lock().is_err()); }
            std::fs::remove_file(&path).unwrap();
            dir.write_json(name, &serde_json::json!({})).unwrap();
            for mode in [0o644, 0o666, 0o400, 0o1600] {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
                assert!(dir.read_json::<serde_json::Value>(name).is_err());
                assert!(dir.write_json(name, &serde_json::json!({})).is_err());
                if name.ends_with(".lock") { assert!(dir.budget_lock().is_err()); }
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            // Only root can construct a wrong-owner fixture without privileges.
            if unsafe { libc::geteuid() } == 0 {
                use std::os::unix::ffi::OsStrExt;
                let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::chown(cpath.as_ptr(), 1, 1) }, 0);
                assert!(dir.read_json::<serde_json::Value>(name).is_err());
                assert!(dir.write_json(name, &serde_json::json!({})).is_err());
                if name.ends_with(".lock") { assert!(dir.budget_lock().is_err()); }
            }
        }
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(StateDir::open(&state).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn root_key_state_directory_stays_pinned_across_parent_replacement() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let dir = StateDir::open_home(home.path(), true).unwrap();
        let original = home.path().join("original");
        std::fs::rename(home.path().join("state"), &original).unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), home.path().join("state")).unwrap();
        assert!(StateDir::open_home(home.path(), false).is_err());
        initialize_budget_at(&dir).unwrap();
        assert!(reserve_restart_at(&dir, 1000).unwrap());
        assert!(original.join(BUDGET_FILE).is_file());
        assert!(original.join("root-key-recovery.lock").is_file());
        assert!(!outside.path().join(BUDGET_FILE).exists());
        assert!(!outside.path().join("root-key-recovery.lock").exists());
    }

}
