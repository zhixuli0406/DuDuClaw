//! Startup reconciliation shares the operator lock and never starts agents.
use std::path::Path;
use super::{config::DiscoveryConfig, store::DiscoveryStore};
use duduclaw_core::concurrency_gate::{self, AcquireOutcome};

/// Labels bind a container to one canonical operator home, never a name prefix.
pub fn scope_labels(home: &Path, run_id: &str, role: &str) -> Result<Vec<String>, String> {
    use sha2::{Digest, Sha256};
    if run_id.is_empty() || run_id.len() > 128
        || !run_id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        || !matches!(role, "attempt" | "evaluator" | "policy") {
        return Err("invalid discovery container scope".into());
    }
    let canonical = super::workspace::canonical_real_directory(home).map_err(|e| e.to_string())?;
    let name = canonical.to_str().ok_or("non-UTF8 discovery home")?;
    let fingerprint = format!("{:x}", Sha256::digest(name.as_bytes()));
    Ok(vec!["com.duduclaw.discovery=1".into(),
        format!("com.duduclaw.discovery.home={fingerprint}"),
        format!("com.duduclaw.discovery.run={run_id}"),
        format!("com.duduclaw.discovery.role={role}")])
}

/// Owns discovery's operator slot across CLI preparation and online execution.
/// A revoked or expired token can never be renewed into a new owner's slot.
pub struct OperatorLeaseGuard {
    home: std::path::PathBuf,
    lease: concurrency_gate::Lease,
    state: std::sync::Arc<LeaseState>,
    worker: Option<std::thread::JoinHandle<()>>,
    ttl: u64,
}
struct LeaseState {
    lost: std::sync::atomic::AtomicBool,
    cancelled: tokio_util::sync::CancellationToken,
    stopped: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    budget: std::sync::Mutex<Vec<super::budget::SharedBudget>>,
}
impl LeaseState {
    fn lose(&self, home: &Path, reason: &str) {
        use std::sync::atomic::Ordering;
        let first_loss = !self.lost.swap(true, Ordering::AcqRel);
        self.cancelled.cancel();
        if let Ok(budget) = self.budget.lock() {
            for budget in budget.iter() { budget.cancel(); }
        }
        if first_loss {
            crate::security_autopilot::audit_and_emit(home,
                &duduclaw_security::audit::AuditEvent::new("discovery_operator_lease_lost", "discovery-maintenance",
                    duduclaw_security::audit::Severity::Warning,
                    serde_json::json!({"reason":reason,"work_cancelled":true})));
        }
    }
}
impl OperatorLeaseGuard {
    pub fn acquire(home: &Path) -> Result<Self, String> {
        Self::acquire_with_timing(home, 30, std::time::Duration::from_secs(1))
    }
    fn acquire_with_timing(home: &Path, ttl: u64, interval: std::time::Duration) -> Result<Self, String> {
        Self::acquire_if_available(home, ttl, interval)?
            .ok_or_else(|| "discovery operator lease unavailable".into())
    }
    fn acquire_if_available(home: &Path, ttl: u64, interval: std::time::Duration) -> Result<Option<Self>, String> {
        let home = super::workspace::canonical_real_directory(home).map_err(|e| e.to_string())?;
        let lease = match concurrency_gate::try_acquire_checked(&home, "discovery-operator", Some(1), ttl)
            .map_err(|_| "discovery operator authority cannot be verified")? {
            AcquireOutcome::Admitted(lease) if lease.is_guarded() => lease,
            AcquireOutcome::AtCapacity { .. } => return Ok(None),
            _ => return Err("discovery operator lease unavailable".into()),
        };
        let state = std::sync::Arc::new(LeaseState {
            lost: std::sync::atomic::AtomicBool::new(false),
            cancelled: tokio_util::sync::CancellationToken::new(),
            stopped: std::sync::Mutex::new(false), wake: std::sync::Condvar::new(),
            budget: std::sync::Mutex::new(Vec::new()),
        });
        let worker_home = home.clone();
        let worker_lease = lease.clone();
        let worker_state = state.clone();
        let worker = std::thread::Builder::new().name("discovery-lease".into()).spawn(move || {
            loop {
                let stopped = match worker_state.stopped.lock() {
                    Ok(stopped) => stopped,
                    Err(_) => { worker_state.lose(&worker_home, "heartbeat lock unavailable"); break; }
                };
                let stopped = match worker_state.wake.wait_timeout_while(stopped, interval, |stopped| !*stopped) {
                    Ok((stopped, _)) => stopped,
                    Err(_) => { worker_state.lose(&worker_home, "heartbeat wait unavailable"); break; }
                };
                if *stopped || worker_state.lost.load(std::sync::atomic::Ordering::Acquire) { break; }
                drop(stopped);
                match concurrency_gate::renew_checked(&worker_home, &worker_lease, ttl) {
                    Ok(true) => {},
                    Ok(false) => { worker_state.lose(&worker_home, "operator authority expired or revoked"); break; }
                    Err(_) => { worker_state.lose(&worker_home, "operator authority cannot be verified"); break; }
                }
            }
        }).map_err(|error| {
            let _ = concurrency_gate::release_checked(&home, &lease);
            format!("operator heartbeat unavailable: {error}")
        })?;
        let guard = Self { home, lease, state, worker: Some(worker), ttl };
        guard.check()?;
        Ok(Some(guard))
    }
    pub fn bind_budget(&self, budget: super::budget::SharedBudget) -> Result<(), String> {
        let mut current = self.state.budget.lock().map_err(|_| "operator cancellation binding unavailable")?;
        current.push(budget.clone());
        drop(current);
        if let Err(error) = self.check() { budget.cancel(); return Err(error); }
        Ok(())
    }
    pub async fn cancelled(&self) { self.state.cancelled.cancelled().await; }
    pub fn check(&self) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self.state.lost.load(Ordering::Acquire) { return Err("operator lease lost".into()); }
        match concurrency_gate::renew_checked(&self.home, &self.lease, self.ttl) {
            Ok(true) if !self.state.lost.load(Ordering::Acquire) => Ok(()),
            _ => {
                self.state.lose(&self.home, "operator authority cannot be verified");
                Err("operator lease lost; discovery cancelled and result refused".into())
            }
        }
    }
    pub fn check_home(&self, home: &Path) -> Result<(), String> {
        let actual = super::workspace::canonical_real_directory(home).map_err(|e| e.to_string())?;
        if actual != self.home { return Err("operator lease belongs to another home".into()); }
        self.check()
    }
}
impl Drop for OperatorLeaseGuard {
    fn drop(&mut self) {
        if let Ok(mut stopped) = self.state.stopped.lock() { *stopped = true; }
        self.state.wake.notify_all();
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
        // Release only our token, never all members of the class.
        if concurrency_gate::release_checked(&self.home, &self.lease).is_err() {
            self.state.lose(&self.home, "operator authority release could not be confirmed; token will expire");
        }
    }
}

fn blocked_path(home: &Path) -> std::path::PathBuf {
    home.join("discovery/maintenance_blocked.json")
}

/// Unconfirmed cleanup prevents new scoring and artifact delivery until sweep.
pub fn check_clean(home: &Path) -> Result<(), String> {
    if blocked_path(home).try_exists().map_err(|e| e.to_string())? {
        Err("discovery container cleanup is unconfirmed; operator reconciliation required".into())
    } else { Ok(()) }
}

fn with_marker_lock<T>(home: &Path, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let directory = home.join("discovery");
    super::workspace::create_private_directory(&directory).map_err(|e| e.to_string())?;
    super::workspace::canonical_real_directory(&directory).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(directory.join("maintenance_blocked.lock")).map_err(|e|e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline =>
                std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => return Err("maintenance marker lock unavailable".into()),
        }
    }
    operation()
}
fn read_marker(home: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(blocked_path(home)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub fn record_cleanup_failure(home: &Path, run_id: &str, role: &str, reason: &str) -> Result<(), String> {
    scope_labels(home, run_id, role)?;
    let reason = reason.chars().take(160).collect::<String>();
    let details = serde_json::json!({"run_id":run_id,"role":role,"reason":reason,
        "at":chrono::Utc::now().to_rfc3339(),"generation":uuid::Uuid::new_v4().to_string(),"status":"cleanup_unconfirmed"});
    // Emit before filesystem work so a persistence failure remains visible.
    crate::security_autopilot::audit_and_emit(home,
        &duduclaw_security::audit::AuditEvent::new("discovery_maintenance_cleanup_failed", "discovery-maintenance",
            duduclaw_security::audit::Severity::Warning, details.clone()));
    with_marker_lock(home, || {
        let temporary = home.join("discovery").join(format!("maintenance-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec(&details).map_err(|e| e.to_string())?)
            .and_then(|_| std::fs::rename(&temporary, blocked_path(home))).map_err(|e| e.to_string())
    })
}

#[derive(Clone, PartialEq, Eq)]
struct SweepClient {
    program: std::path::PathBuf,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}
impl SweepClient {
    fn detected() -> Result<Vec<Self>, String> {
        let program = std::env::var_os("PATH").into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join("docker")).find(|path| path.is_file())
            .and_then(|path| path.canonicalize().ok()).ok_or("Docker cleanup client unavailable")?;
        let environment = vec![("PATH".into(), "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin".into())];
        let default = Self { program: program.clone(), environment: environment.clone() };
        let mut operator = Self { program, environment };
        for key in ["HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH"] {
            if let Some(value) = std::env::var_os(key) { operator.environment.push((key.into(), value)); }
        }
        // Evaluators intentionally use the default daemon; policy/attempt
        // launchers can use the operator's trusted Docker context.
        Ok(if operator == default { vec![default] } else { vec![default, operator] })
    }
    fn output(&self, args: &[&str], lease: &OperatorLeaseGuard,
        deadline: std::time::Instant) -> Result<String, String> {
        use std::io::{Read, Seek};
        use std::process::{Command, Stdio};
        use std::sync::atomic::Ordering;
        let mut output = tempfile::tempfile().map_err(|_| "Docker cleanup output unavailable")?;
        let mut command = Command::new(&self.program);
        command.env_clear().envs(self.environment.iter().cloned()).args(args)
            .stdin(Stdio::null()).stdout(output.try_clone().map_err(|_| "Docker cleanup output unavailable")?)
            .stderr(Stdio::null());
        #[cfg(unix)] {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|_| "Docker cleanup launch failed")?;
        let child_pid = child.id();
        let until = deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(5));
        let status = loop {
            if lease.state.lost.load(Ordering::Acquire) { break Err("operator lease lost during cleanup"); }
            if std::time::Instant::now() >= until { break Err("Docker cleanup deadline exceeded"); }
            match output.metadata() {
                Ok(metadata) if metadata.len() <= 1024 * 1024 => {},
                _ => break Err("Docker cleanup output limit exceeded"),
            }
            match child.try_wait() {
                Ok(Some(status)) => break if status.success() { Ok(()) } else { Err("Docker cleanup command failed") },
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break Err("Docker cleanup wait failed"),
            }
        };
        // Reap the client and its descendants on success and every failure.
        #[cfg(unix)] unsafe { libc::kill(-(child_pid as i32), libc::SIGKILL); }
        let _ = child.kill();
        let _ = child.wait();
        status.map_err(str::to_owned)?;
        lease.check()?;
        output.rewind().map_err(|_| "Docker cleanup output unreadable")?;
        let mut bytes = Vec::new();
        output.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "Docker cleanup output unreadable")?;
        if bytes.len() > 1024 * 1024 { return Err("Docker cleanup output limit exceeded".into()); }
        String::from_utf8(bytes).map_err(|_| "Docker cleanup output is not UTF-8".into())
    }
}

fn sweep_with_client(home: &Path, lease: &OperatorLeaseGuard, client: &SweepClient) -> Result<usize, String> {
    lease.check_home(home)?;
    let result: Result<usize, String> = (|| {
        let labels = scope_labels(home, "sweep", "policy")?;
        let home_filter = format!("label={}", labels[1]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let ids = client.output(&["ps", "-aq", "--no-trunc", "--filter", "label=com.duduclaw.discovery=1",
            "--filter", &home_filter], lease, deadline)?;
        let ids = ids.split_whitespace().collect::<std::collections::BTreeSet<_>>();
        if ids.len() > 256 { return Err("Docker orphan count exceeds cleanup limit".into()); }
        let mut removed = 0;
        for id in ids {
            if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("Docker cleanup returned an invalid container id".into());
            }
            lease.check_home(home)?;
            let inspected = client.output(&["inspect", "--format", "{{json .Config.Labels}}", id], lease, deadline)?;
            let actual: std::collections::BTreeMap<String, String> = serde_json::from_str(&inspected)
                .map_err(|_| "Docker cleanup labels cannot be verified")?;
            let (_, expected_home) = labels[1].split_once('=').unwrap();
            if actual.get("com.duduclaw.discovery").map(String::as_str) != Some("1")
                || actual.get("com.duduclaw.discovery.home").map(String::as_str) != Some(expected_home) {
                continue;
            }
            let run = actual.get("com.duduclaw.discovery.run").ok_or("owned orphan has no run label")?;
            let role = actual.get("com.duduclaw.discovery.role").ok_or("owned orphan has no role label")?;
            // All four labels must be confirmed even if a client ignores filters.
            scope_labels(home, run, role)?;
            lease.check_home(home)?;
            client.output(&["rm", "-f", id], lease, deadline)?;
            let id_filter = format!("id={id}");
            if !client.output(&["ps", "-aq", "--no-trunc", "--filter", &id_filter], lease, deadline)?.trim().is_empty() {
                return Err("Docker orphan removal is unconfirmed".into());
            }
            removed += 1;
        }
        Ok(removed)
    })();
    if let Err(reason) = &result {
        record_cleanup_failure(home, "sweep", "policy", reason)?;
    }
    result
}

fn reconcile_with_clients(home: &Path, lease: &OperatorLeaseGuard, clients: &[SweepClient]) -> Result<(), String> {
    lease.check_home(home)?;
    let previous_marker = with_marker_lock(home, || read_marker(home))?;
    for client in clients { sweep_with_client(home, lease, client)?; }
    lease.check_home(home)?;
    with_marker_lock(home, || {
        if read_marker(home)? != previous_marker {
            return Err("new cleanup failure occurred during reconciliation; repeat sweep required".into());
        }
        match std::fs::remove_file(blocked_path(home)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    })?;
    if !home.join("discovery.db").is_file() { return Ok(()); }
    let store = DiscoveryStore::open(home).map_err(|e|e.to_string())?;
    let ids = store.running_ids().map_err(|e|e.to_string())?;
    store.interrupt_running().map_err(|e|e.to_string())?;
    for id in ids {
        if id.is_empty() || id.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-')) { continue; }
        let root = home.join("discovery/runs").join(id);
        if root.is_dir() && super::workspace::canonical_real_directory(&root).is_ok() {
            std::fs::write(root.join(".completed"), "interrupted").map_err(|e|e.to_string())?;
        }
    }
    let text = std::fs::read_to_string(home.join("config.toml")).map_err(|e|e.to_string())?;
    let table = text.parse::<toml::Table>().map_err(|e|e.to_string())?;
    if let Some(value) = table.get("discovery") {
        let config: DiscoveryConfig = value.clone().try_into().map_err(|e: toml::de::Error| e.to_string())?;
        super::workspace::cleanup_retained_runs(&home.join("discovery/runs"), &config,
            std::time::SystemTime::now()).map_err(|e|e.to_string())?;
    }
    lease.check_home(home)
}

/// Reconcile only while this caller holds the renewable home authority.
pub fn reconcile_with_lease(home: &Path, lease: &OperatorLeaseGuard) -> Result<(), String> {
    let clients = SweepClient::detected().map_err(|reason| {
        let _ = record_cleanup_failure(home, "sweep", "policy", &reason);
        reason
    })?;
    reconcile_with_clients(home, lease, &clients)
}

pub fn on_gateway_start(home: &Path) -> Result<(), String> {
    let configured = std::fs::read_to_string(home.join("config.toml")).ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| table.contains_key("discovery"));
    if !configured && !home.join("discovery.db").is_file()
        && !home.join("discovery").is_dir() && !blocked_path(home).try_exists().map_err(|e|e.to_string())? {
        return Ok(());
    }
    // An active CLI owns the same slot. Do not sweep its containers or mark
    // its run interrupted when another gateway process starts.
    let Some(lease) = OperatorLeaseGuard::acquire_if_available(home, 30,
        std::time::Duration::from_secs(1))? else { return Ok(()); };
    reconcile_with_lease(home, &lease)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> super::super::budget::SharedBudget {
        super::super::budget::SharedBudget::new(super::super::contracts::RunBudget {
            max_agent_calls: 1, max_usd: 1., max_wall_secs: 60, max_rounds: 1,
        }).unwrap()
    }

    #[test]
    fn operator_drop_has_a_deadline_when_authority_lock_is_contended() {
        let dir = tempfile::tempdir().unwrap();
        let guard = OperatorLeaseGuard::acquire_with_timing(dir.path(), 30,
            std::time::Duration::from_secs(10)).unwrap();
        let lock = std::fs::OpenOptions::new().create(true).truncate(false)
            .read(true).write(true).open(dir.path().join("concurrency_leases.json.lock")).unwrap();
        lock.lock().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { drop(guard); send.send(()).unwrap(); });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        drop(lock);
        worker.join().unwrap();
        assert!(result.is_ok(), "operator drop must not block indefinitely on release");
    }

    #[test]
    fn operator_heartbeat_keeps_a_long_run_exclusive_beyond_the_original_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let guard = OperatorLeaseGuard::acquire_with_timing(dir.path(), 1,
            std::time::Duration::from_millis(20)).unwrap();
        guard.bind_budget(budget()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(concurrency_gate::try_acquire(dir.path(), "discovery-operator", Some(1), 10).is_at_capacity());
        guard.check().unwrap();
    }

    #[test]
    fn operator_revocation_cancels_work_and_drop_never_releases_a_new_owner() {
        let dir = tempfile::tempdir().unwrap();
        let guard = OperatorLeaseGuard::acquire_with_timing(dir.path(), 10,
            std::time::Duration::from_millis(20)).unwrap();
        let budget = budget();
        guard.bind_budget(budget.clone()).unwrap();
        concurrency_gate::release_class(dir.path(), "discovery-operator");
        let current = match concurrency_gate::try_acquire(dir.path(), "discovery-operator", Some(1), 10) {
            AcquireOutcome::Admitted(lease) => lease, _ => panic!("new owner unavailable"),
        };
        let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !budget.is_cancelled() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(budget.is_cancelled(), "revoked operator must stop running attempts");
        assert!(guard.check().is_err());
        drop(guard);
        assert_eq!(concurrency_gate::active_count(dir.path(), "discovery-operator"), 1);
        concurrency_gate::release(dir.path(), &current);
    }

    #[test]
    fn container_scope_labels_are_exact_and_do_not_cross_operator_homes() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = scope_labels(first.path(), "run-1", "policy").unwrap();
        let b = scope_labels(second.path(), "run-1", "policy").unwrap();
        assert_eq!(a.len(), 4);
        assert_ne!(a[1], b[1]);
        assert!(scope_labels(first.path(), "--all", "unknown").is_err());
        assert!(scope_labels(first.path(), "run\nother", "policy").is_err());
    }

    #[cfg(unix)]
    fn sweep_fixture(home: &Path, foreign: &Path, mode: &str) -> (tempfile::TempDir, SweepClient, String, String) {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let owned = "a".repeat(64);
        let other = "b".repeat(64);
        std::fs::write(fixture.path().join("ids"), format!("{owned}\n{other}\n")).unwrap();
        for (id, scope) in [(&owned, home), (&other, foreign)] {
            let labels = scope_labels(scope, "run-1", "evaluator").unwrap().into_iter()
                .map(|label| { let (key, value) = label.split_once('=').unwrap(); (key.to_string(), value.to_string()) })
                .collect::<std::collections::BTreeMap<_, _>>();
            std::fs::write(fixture.path().join(format!("{id}.json")), serde_json::to_vec(&labels).unwrap()).unwrap();
        }
        let program = fixture.path().join("docker");
        std::fs::write(&program, r#"#!/bin/sh
echo "$*" >> "$LOG"
for last in "$@"; do :; done
case "$1" in
ps) id=''; for arg in "$@"; do case "$arg" in id=*) id=${arg#id=};; esac; done
    if [ -n "$id" ]; then awk -v id="$id" '$0 == id' "$IDS"; else cat "$IDS"; fi;;
inspect) cat "$LABEL_DIR/$last.json";;
rm) [ "$MODE" != fail ] || exit 3
    [ "$MODE" != noop ] || exit 0
    awk -v id="$last" '$0 != id' "$IDS" > "$IDS.tmp"; mv "$IDS.tmp" "$IDS";;
*) exit 4;;
esac
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let environment = vec![("PATH".into(), "/usr/bin:/bin".into()),
            ("LOG".into(), fixture.path().join("log").into_os_string()),
            ("IDS".into(), fixture.path().join("ids").into_os_string()),
            ("LABEL_DIR".into(), fixture.path().as_os_str().to_owned()),
            ("MODE".into(), mode.into())];
        (fixture, SweepClient { program, environment }, owned, other)
    }

    #[cfg(unix)]
    #[test]
    fn orphan_sweep_removes_only_inspected_exact_home_scope() {
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = OperatorLeaseGuard::acquire(home.path()).unwrap();
        let (fixture, client, owned, other) = sweep_fixture(home.path(), foreign.path(), "ok");
        assert_eq!(sweep_with_client(home.path(), &lease, &client).unwrap(), 1);
        let log = std::fs::read_to_string(fixture.path().join("log")).unwrap();
        assert!(log.contains("label=com.duduclaw.discovery=1"));
        assert!(log.contains(&format!("rm -f {owned}")));
        assert!(!log.contains(&format!("rm -f {other}")));
        assert_eq!(std::fs::read_to_string(fixture.path().join("ids")).unwrap().trim(), other);
    }

    #[cfg(unix)]
    #[test]
    fn orphan_cleanup_failure_alerts_and_blocks_scoring_until_reconciled() {
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = OperatorLeaseGuard::acquire(home.path()).unwrap();
        let (fixture, client, owned, _) = sweep_fixture(home.path(), foreign.path(), "fail");
        assert!(sweep_with_client(home.path(), &lease, &client).is_err());
        assert!(check_clean(home.path()).is_err());
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("discovery_maintenance_cleanup_failed"));
        assert!(std::fs::read_to_string(fixture.path().join("ids")).unwrap().contains(&owned));
    }
    #[cfg(unix)]
    #[test]
    fn orphan_success_exit_without_removal_is_not_confirmation() {
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = OperatorLeaseGuard::acquire(home.path()).unwrap();
        let (fixture, client, owned, _) = sweep_fixture(home.path(), foreign.path(), "noop");
        assert!(sweep_with_client(home.path(), &lease, &client).is_err());
        assert!(check_clean(home.path()).is_err());
        assert!(std::fs::read_to_string(fixture.path().join("ids")).unwrap().contains(&owned));
    }

    #[cfg(unix)]
    #[test]
    fn successful_reconciliation_clears_only_the_confirmed_cleanup_block() {
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = OperatorLeaseGuard::acquire(home.path()).unwrap();
        record_cleanup_failure(home.path(), "failed-run", "attempt", "daemon unavailable").unwrap();
        let (fixture, client, owned, other) = sweep_fixture(home.path(), foreign.path(), "ok");
        reconcile_with_clients(home.path(), &lease, &[client]).unwrap();
        check_clean(home.path()).unwrap();
        let remaining = std::fs::read_to_string(fixture.path().join("ids")).unwrap();
        assert!(!remaining.contains(&owned));
        assert!(remaining.contains(&other));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a trusted Docker daemon and the already installed official Python image; no API"]
    fn real_orphan_sweep_removes_own_crashed_container_and_preserves_active_foreign_home() {
        use std::process::{Command, Stdio};
        struct TestContainers { client: SweepClient, ids: Vec<String> }
        impl Drop for TestContainers {
            fn drop(&mut self) {
                use std::os::unix::process::CommandExt;
                for id in &self.ids {
                    let mut command = Command::new(&self.client.program);
                    command.env_clear().envs(self.client.environment.iter().cloned())
                        .args(["rm", "-f", id]).stdin(Stdio::null()).stdout(Stdio::null())
                        .stderr(Stdio::null()).process_group(0);
                    if let Ok(mut child) = command.spawn() {
                        let pid = child.id();
                        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                        while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < until {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
        }
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let first = OperatorLeaseGuard::acquire(home.path()).unwrap();
        let active_foreign = OperatorLeaseGuard::acquire(foreign.path()).unwrap();
        let client = SweepClient::detected().unwrap().pop().unwrap();
        let mut containers = TestContainers { client: client.clone(), ids: Vec::new() };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let image = client.output(&["image", "inspect", "--format", "{{.Id}}", "python:3.12-alpine"], &first, deadline).unwrap();
        let image = image.trim();
        assert!(image.strip_prefix("sha256:").is_some_and(|id| id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit())));
        for scope in [home.path(), foreign.path()] {
            let labels = scope_labels(scope, "restart-probe", "policy").unwrap();
            let mut args = vec!["create", "--network=none", "--read-only", "--memory=268435456",
                "--pids-limit=8", "--cpus=1", "--cap-drop=ALL", "--security-opt=no-new-privileges"];
            for label in &labels { args.extend(["--label", label.as_str()]); }
            args.extend(["--entrypoint=/bin/sh", image, "-c", "sleep 60"]);
            let id = client.output(&args, &first, deadline).unwrap().trim().to_owned();
            assert!(id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit()));
            containers.ids.push(id.clone());
            client.output(&["start", &id], &first, deadline).unwrap();
        }
        drop(first); // Simulate a crashed controller's released authority.
        let restarted = OperatorLeaseGuard::acquire(home.path()).unwrap();
        assert_eq!(sweep_with_client(home.path(), &restarted, &client).unwrap(), 1);
        let foreign_id = &containers.ids[1];
        let alive = client.output(&["inspect", "--format", "{{.State.Running}}", foreign_id], &restarted,
            std::time::Instant::now() + std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(alive.trim(), "true");
        active_foreign.check().unwrap();
        println!("own crashed container removed; active foreign-home container preserved; no API");
    }

    #[cfg(unix)]
    #[test]
    fn boot_marks_crashed_runs_but_leaves_an_active_operator_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        let store = DiscoveryStore::open(dir.path()).unwrap();
        let budget = super::super::contracts::RunBudget { max_agent_calls: 1, max_usd: 1., max_wall_secs: 1, max_rounds: 1 };
        store.create_run("test-run", "goal", "test", "score", "hash", super::super::tree::Direction::Max, &budget).unwrap();
        super::super::workspace::create_private_directory(&dir.path().join("discovery/runs/test-run")).unwrap();
        let lease = match concurrency_gate::try_acquire(dir.path(), "discovery-operator", Some(1), 60) {
            AcquireOutcome::Admitted(lease) => lease, _ => panic!("lease unavailable"),
        };
        on_gateway_start(dir.path()).unwrap();
        assert_eq!(store.running_ids().unwrap(), vec!["test-run"]);
        concurrency_gate::release(dir.path(), &lease);
        let foreign = tempfile::tempdir().unwrap();
        let (fixture, client, _, _) = sweep_fixture(dir.path(), foreign.path(), "ok");
        std::fs::write(fixture.path().join("ids"), "").unwrap();
        let guard = OperatorLeaseGuard::acquire(dir.path()).unwrap();
        reconcile_with_clients(dir.path(), &guard, &[client]).unwrap();
        assert!(store.running_ids().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(dir.path().join("discovery/runs/test-run/.completed")).unwrap(), "interrupted");
    }
}
