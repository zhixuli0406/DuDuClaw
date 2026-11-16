//! Gateway-owned MCP startup observations and bounded, gated probes.
use std::{collections::{HashMap, HashSet, VecDeque}, path::{Path, PathBuf}, sync::{Arc, LazyLock, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use duduclaw_core::mcp_start_status::{ReasonCode, StartStatus, safe_label};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MAX_RESULTS: usize = 4096;
const MAX_EMPLOYEE_RESULTS: usize = 64;
pub(crate) const MAX_INIT_BYTES: usize = 64 * 1024;
const MAX_INIT_SERVERS: usize = 64;
type Key = (PathBuf, String, String);

// Hash the original identity as well as storing its bounded sanitised label,
// so two different configured names cannot alias after label sanitisation.
fn label_key(value: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    format!("{}-{:016x}", safe_label(value), hash.finish())
}
fn key(home: &Path, agent: &str, server: &str) -> Key {
    (home.into(), label_key(agent), label_key(server))
}

#[derive(Default)]
struct Results { entries: HashMap<Key, (Instant, StartStatus)>, order: VecDeque<Key>, employee_counts: HashMap<(PathBuf, String), usize> }
impl Results {
    fn remove(&mut self, key: &Key) {
        if self.entries.remove(key).is_some() {
            let employee = (key.0.clone(), key.1.clone());
            if let Some(count) = self.employee_counts.get_mut(&employee) {
                *count -= 1;
                if *count == 0 { self.employee_counts.remove(&employee); }
            }
        }
    }
    fn insert(&mut self, key: Key, status: StartStatus) {
        if self.entries.contains_key(&key) {
            self.remove(&key);
        }
        let employee = (key.0.clone(), key.1.clone());
        if self.employee_counts.get(&employee).copied().unwrap_or(0) >= MAX_EMPLOYEE_RESULTS {
            if let Some(index) = self.order.iter().position(|old| old.0 == key.0 && old.1 == key.1) {
                let oldest = self.order.remove(index).expect("position came from queue");
                self.remove(&oldest);
            }
        }
        while self.entries.len() >= MAX_RESULTS {
            let Some(oldest) = self.order.pop_front() else { break; };
            if self.entries.contains_key(&oldest) { self.remove(&oldest); }
        }
        self.employee_counts.entry(employee).and_modify(|n| *n += 1).or_insert(1);
        self.order.push_back(key.clone());
        self.entries.insert(key, (Instant::now(), status));
    }
}
static LAST: LazyLock<Mutex<Results>> = LazyLock::new(|| Mutex::new(Results::default()));
static PROBES: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(4));
static IN_FLIGHT: LazyLock<Mutex<HashMap<Key, Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn last(home: &Path, agent: &str, server: &str) -> Option<StartStatus> {
    LAST.lock().ok()?.entries.get(&key(home, agent, server)).map(|(_, status)| status.clone())
}
fn remember(home: &Path, agent: &str, server: &str, status: StartStatus) {
    if let Ok(mut map) = LAST.lock() { map.insert(key(home, agent, server), status); }
}

pub(crate) fn clear(home: &Path, agent: &str, server: Option<&str>) {
    let agent_key = label_key(agent);
    let matches = |k: &Key| k.0.as_path() == home && k.1 == agent_key
        && server.is_none_or(|s| k.2 == label_key(s));
    // Same lock order as publishing a completed probe. Invalidate work already
    // running so a late completion cannot restore an obsolete result.
    if let Ok(active) = IN_FLIGHT.lock() {
        for (_, valid) in active.iter().filter(|(k, _)| matches(k)) {
            valid.store(false, Ordering::SeqCst);
        }
        if let Ok(mut map) = LAST.lock() {
            let removed: Vec<_> = map.entries.keys().filter(|k| matches(k)).cloned().collect();
            for key in removed { map.remove(&key); }
        }
    }
}

/// Read names from the final per-child frozen config, never the mutable source.
pub(crate) fn frozen_names(path: Option<&Path>) -> Vec<String> {
    let Some(path) = path else { return Vec::new(); };
    let Ok(bytes) = std::fs::read(path) else { return Vec::new(); };
    let Ok(config) = serde_json::from_slice::<Value>(&bytes) else { return Vec::new(); };
    config["mcpServers"].as_object().into_iter().flat_map(|m| m.keys())
        .filter(|name| name.len() <= 128).take(MAX_INIT_SERVERS).cloned().collect()
}

// Stop serialisation at the event budget without allocating another event-sized
// string. Caller-side raw frame limits also avoid feeding oversized init frames.
fn bounded_event(event: &Value) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 { return Err(std::io::ErrorKind::InvalidData.into()); }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    serde_json::to_writer(Budget(MAX_INIT_BYTES), event).is_ok()
}

pub(crate) fn observe_init(home: &Path, agent: &str, allowed: &[String], event: &Value) {
    if event["type"] != "system" || event["subtype"] != "init" || !bounded_event(event) { return; }
    let Some(servers) = event["mcp_servers"].as_array() else { return; };
    let mut seen = HashSet::new();
    let mut failures = Vec::new();
    let time = chrono::Utc::now().to_rfc3339();
    for server in servers.iter().take(MAX_INIT_SERVERS) {
        let Some(name) = server["name"].as_str() else { continue; };
        // Check the original exact name before sanitising any diagnostic label.
        if name.len() > 128 || !allowed.iter().take(MAX_INIT_SERVERS).any(|n| n == name)
            || !seen.insert(name) { continue; }
        let status = safe_label(server["status"].as_str().unwrap_or("unknown"));
        let ok = server["status"].as_str() == Some("connected");
        remember(home, agent, name, StartStatus { ok,
            reason_code: (!ok).then_some(ReasonCode::ConnectionFailed), time: time.clone(), tool_count: None });
        if !ok { failures.push(json!({"server":safe_label(name), "status":status})); }
    }
    if !failures.is_empty() {
        append_failure(home, json!({"classification":"mcp_start", "agent_id":safe_label(agent),
            "failures":failures, "ok":false, "reason_code":ReasonCode::ConnectionFailed, "time":time}));
    }
}

fn append_failure(home: &Path, record: Value) {
    if let Err(e) = crate::trajectory_guard::append_anomaly(home, &record) {
        tracing::warn!(error = %e, "MCP startup anomaly append failed");
    }
}

/// Validate and freeze the same effective proxy config without persisted repair.
/// The TempPath stays alive until the probe child is reaped on every exit path.
fn prepare(home: &Path, agent: &str, server: &str) -> Result<(Value, tempfile::TempPath), ReasonCode> {
    let config = duduclaw_agent::mcp_template::effective_mcp_config_for_probe(&home.join("agents").join(agent))
        .map_err(|_| ReasonCode::PolicyDenied)?;
    let guard = crate::redaction_proxy::proxy_config_for_probe(home, agent, &config)
        .map_err(|_| ReasonCode::PolicyDenied)?;
    let config: Value = serde_json::from_slice(&std::fs::read(&guard).map_err(|_| ReasonCode::PolicyDenied)?)
        .map_err(|_| ReasonCode::PolicyDenied)?;
    let entry = config["mcpServers"].get(server).cloned().ok_or(ReasonCode::PolicyDenied)?;
    Ok((entry, guard))
}

struct Flight { key: Key, valid: Arc<AtomicBool> }
impl Flight {
    fn acquire(key: Key) -> Option<Self> {
        let mut active = IN_FLIGHT.lock().ok()?;
        if active.contains_key(&key) { return None; }
        let valid = Arc::new(AtomicBool::new(true));
        active.insert(key.clone(), valid.clone());
        Some(Self { key, valid })
    }
}
impl Drop for Flight {
    fn drop(&mut self) {
        if let Ok(mut active) = IN_FLIGHT.lock() { active.remove(&self.key); }
    }
}
fn result_status(result: Result<usize, ReasonCode>) -> StartStatus {
    StartStatus { ok: result.is_ok(), tool_count: result.as_ref().ok().copied(),
        reason_code: result.err(), time: chrono::Utc::now().to_rfc3339() }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProbeResult {
    pub ok: bool,
    pub tool_count: Option<usize>,
    pub reason_code: Option<ReasonCode>,
}
impl From<&StartStatus> for ProbeResult {
    fn from(status: &StartStatus) -> Self {
        Self { ok: status.ok, tool_count: status.tool_count, reason_code: status.reason_code }
    }
}

pub(crate) async fn probe(home: &Path, agent: &str, server: &str) -> StartStatus {
    let probe_key = key(home, agent, server);
    let occupied = IN_FLIGHT.lock().map(|active| active.contains_key(&probe_key)).unwrap_or(true);
    if occupied { return result_status(Err(ReasonCode::Busy)); }
    // Acquire a global permit before admitting a key: queued RPCs cannot grow
    // the in-flight map. Every process launch is inside both guards' lifetimes.
    let Ok(Ok(_permit)) = tokio::time::timeout(Duration::from_secs(2), PROBES.acquire()).await else {
        return result_status(Err(ReasonCode::Busy));
    };
    let Some(flight) = Flight::acquire(probe_key) else {
        return result_status(Err(ReasonCode::Busy));
    };
    let result = match prepare(home, agent, server) {
        Ok((entry, _guard)) => probe_entry(&entry, Duration::from_secs(10)).await,
        Err(reason) => Err(reason),
    };
    let status = result_status(result);
    if let Ok(_active) = IN_FLIGHT.lock() {
        if flight.valid.load(Ordering::SeqCst) {
            remember(home, agent, server, status.clone());
        }
    }
    status
}

async fn read_response<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Result<Value, ReasonCode> {
    use tokio::io::AsyncReadExt;
    loop {
        // Bound a single hostile upstream frame before allocating it.
        let mut bytes = Vec::new();
        let n = (&mut *reader).take(1024 * 1024).read_until(b'\n', &mut bytes).await
            .map_err(|_| ReasonCode::ProtocolError)?;
        if n == 0 || bytes.last() != Some(&b'\n') { return Err(ReasonCode::ProtocolError); }
        let frame: Value = match serde_json::from_slice(&bytes) { Ok(v) => v, Err(_) => continue };
        if frame["id"] != id { continue; }
        if let Some(error) = frame.get("error") {
            let reason = serde_json::from_value(error["data"]["reason_code"].clone())
                .unwrap_or(ReasonCode::ProtocolError);
            return Err(reason);
        }
        return frame.get("result").cloned().ok_or(ReasonCode::ProtocolError);
    }
}

async fn probe_entry(entry: &Value, deadline: std::time::Duration) -> Result<usize, ReasonCode> {
    let command = entry["command"].as_str().ok_or(ReasonCode::PolicyDenied)?;
    let args: Vec<String> = serde_json::from_value(entry["args"].clone()).map_err(|_| ReasonCode::PolicyDenied)?;
    let env: HashMap<String, String> = serde_json::from_value(entry["env"].clone()).map_err(|_| ReasonCode::PolicyDenied)?;
    let mut cmd = duduclaw_core::platform::async_command_for(command);
    cmd.args(args).env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs())
        .envs(env).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let end = tokio::time::Instant::now() + deadline;
    let mut child = tokio::time::timeout_at(end, duduclaw_core::execution_gate::with_runtime(
        duduclaw_core::execution_gate::RuntimeKind::Registered(
            duduclaw_core::execution_gate::Text::new("third-party-mcp".into()).expect("constant runtime")),
        crate::process_group::BackendChild::spawn_class(&mut cmd,
            duduclaw_core::execution_gate::ExecutionClass::ThirdPartyMcp)))
        .await.map_err(|_| ReasonCode::Timeout)?.map_err(|_| ReasonCode::UpstreamSpawnFailed)?;
    let result = tokio::time::timeout_at(end, async {
        let mut input = child.child().stdin.take().ok_or(ReasonCode::UpstreamIoUnavailable)?;
        let mut reader = BufReader::new(child.child().stdout.take().ok_or(ReasonCode::UpstreamIoUnavailable)?);
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"duduclaw-install-probe\",\"version\":\"1\"}}}\n")
            .await.map_err(|_| ReasonCode::ProtocolError)?;
        let init = read_response(&mut reader, 1).await?;
        if !init["protocolVersion"].is_string() || !init["capabilities"].is_object() {
            return Err(ReasonCode::ProtocolError);
        }
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .await.map_err(|_| ReasonCode::ProtocolError)?;
        let result = read_response(&mut reader, 2).await?;
        result["tools"].as_array().map(Vec::len).ok_or(ReasonCode::ProtocolError)
    }).await.unwrap_or(Err(ReasonCode::Timeout));
    child.kill_and_reap().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_observes_failed_server_without_failing_turn_or_logging_raw_error() {
        let home = tempfile::tempdir().unwrap();
        observe_init(home.path(), "employee", &["good".into(), "bad".into()], &json!({"type":"system", "subtype":"init",
            "mcp_servers":[{"name":"good","status":"connected"},
                {"name":"bad","status":"failed","error":"secret-never-log"}]}));
        assert!(last(home.path(), "employee", "good").unwrap().ok);
        assert_eq!(last(home.path(), "employee", "bad").unwrap().reason_code, Some(ReasonCode::ConnectionFailed));
        let log = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(!log.contains("secret-never-log"));
        let record: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(record["failures"][0]["status"], "failed");
    }

    fn init_event(servers: Vec<Value>) -> Value {
        json!({"type":"system", "subtype":"init", "mcp_servers":servers})
    }

    #[test]
    fn init_only_accepts_frozen_exact_names_deduplicates_and_summarises() {
        let home = tempfile::tempdir().unwrap();
        let allowed = vec!["real".into(), "other".into()];
        observe_init(home.path(), "employee", &allowed, &init_event(vec![
            json!({"name":"real.evil","status":"failed"}),
            json!({"name":"real","status":"connected"}),
            json!({"name":"real","status":"failed"}),
            json!({"name":"other","status":"failed\ncredential-text"}),
        ]));
        assert!(last(home.path(), "employee", "real").unwrap().ok);
        assert!(last(home.path(), "employee", "real.evil").is_none());
        let log = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        let row: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(row["failures"].as_array().unwrap().len(), 1);
        assert_eq!(row["failures"][0]["status"], "failed_credential-text");
    }

    #[test]
    fn init_caps_array_event_size_and_label_bytes() {
        let home = tempfile::tempdir().unwrap();
        let long_name = "員".repeat(60);
        let allowed: Vec<String> = (0..65).map(|n| format!("s{n}")).chain([long_name.clone()]).collect();
        let mut servers: Vec<_> = (0..65).map(|n| json!({"name":format!("s{n}"),"status":"失敗".repeat(70)})).collect();
        servers.insert(0, json!({"name":long_name,"status":"failed"}));
        observe_init(home.path(), "employee", &allowed, &init_event(servers));
        assert!(last(home.path(), "employee", &long_name).is_none());
        assert!(last(home.path(), "employee", "s63").is_none());
        assert!(last(home.path(), "employee", "s62").is_some());
        let log = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        let row: Value = serde_json::from_str(log.trim()).unwrap();
        for failure in row["failures"].as_array().unwrap() {
            assert!(failure["status"].as_str().unwrap().len() <= 128);
        }
        let mut oversized = init_event(vec![json!({"name":"s0","status":"connected"})]);
        oversized["padding"] = json!("x".repeat(MAX_INIT_BYTES));
        observe_init(home.path(), "employee", &allowed, &oversized);
        assert!(!last(home.path(), "employee", "s0").unwrap().ok);
        assert_eq!(std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap(), log);
    }

    #[test]
    fn map_caps_each_employee_without_evicting_others_and_enforces_global_cap() {
        let mut map = Results::default();
        let home = Path::new("isolated-home");
        map.insert(key(home, "other", "keep"), result_status(Ok(0)));
        for n in 0..10_000 {
            map.insert(key(home, "員工\n", &format!("server/{n}")), result_status(Ok(n)));
            assert!(map.employee_counts.get(&(home.into(), label_key("員工\n"))).copied().unwrap_or(0) <= MAX_EMPLOYEE_RESULTS);
        }
        assert_eq!(map.employee_counts.get(&(home.into(), label_key("員工\n"))), Some(&MAX_EMPLOYEE_RESULTS));
        assert_eq!(map.entries.len(), MAX_EMPLOYEE_RESULTS + 1);
        assert!(map.entries.contains_key(&key(home, "other", "keep")));
        assert!(!map.entries.contains_key(&key(home, "員工\n", "server/0")));
        assert!(map.entries.contains_key(&key(home, "員工\n", "server/9999")));
        assert!(map.entries.keys().all(|(_, agent, server)|
            agent.len() <= 145 && server.len() <= 145 && agent.chars().chain(server.chars())
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))));
        assert_ne!(key(home, "employee", "a/b"), key(home, "employee", "a_b"));

        let mut global = Results::default();
        for n in 0..MAX_RESULTS / MAX_EMPLOYEE_RESULTS + 1 {
            for s in 0..MAX_EMPLOYEE_RESULTS {
                global.insert(key(home, &format!("employee-{n}"), &format!("server-{s}")), result_status(Ok(s)));
            }
        }
        assert_eq!(global.entries.len(), MAX_RESULTS);
        assert_eq!(global.employee_counts.values().sum::<usize>(), MAX_RESULTS);
        assert!(!global.entries.contains_key(&key(home, "employee-0", "server-0")));
    }

    #[test]
    fn explicit_probe_failure_updates_memory_without_appending_failure_record() {
        let home = tempfile::tempdir().unwrap();
        let status = result_status(Err(ReasonCode::PolicyDenied));
        remember(home.path(), "employee", "server", status.clone());
        assert_eq!(last(home.path(), "employee", "server").unwrap().reason_code, status.reason_code);
        assert!(!home.path().join("channel_failures.jsonl").exists());
    }

    #[tokio::test]
    async fn concurrency_is_bounded_and_each_key_has_one_flight() {
        let home = tempfile::tempdir().unwrap();
        let k = key(home.path(), "employee", "server");
        let first = Flight::acquire(k.clone()).unwrap();
        assert!(Flight::acquire(k.clone()).is_none());
        let status = probe(home.path(), "employee", "server").await;
        assert_eq!(status.reason_code, Some(ReasonCode::Busy));
        assert!(last(home.path(), "employee", "server").is_none());
        drop(first);
        assert!(Flight::acquire(k).is_some());
        let permits = PROBES.acquire_many(4).await.unwrap();
        let status = probe(home.path(), "employee", "server").await;
        assert_eq!(status.reason_code, Some(ReasonCode::Busy));
        assert!(last(home.path(), "employee", "server").is_none());
        drop(permits);
    }

    #[test]
    fn clear_scopes_to_employee_and_invalidates_pending_probes() {
        let home = tempfile::tempdir().unwrap();
        for agent in ["alice", "bob"] {
            for server in ["one", "two"] { remember(home.path(), agent, server, result_status(Ok(1))); }
        }
        let flight = Flight::acquire(key(home.path(), "alice", "one")).unwrap();
        clear(home.path(), "alice", Some("one"));
        assert!(!flight.valid.load(Ordering::SeqCst));
        assert!(last(home.path(), "alice", "one").is_none());
        assert!(last(home.path(), "alice", "two").is_some());
        clear(home.path(), "alice", None);
        assert!(last(home.path(), "alice", "two").is_none());
        assert!(last(home.path(), "bob", "one").is_some());
    }

    #[test]
    fn frozen_names_do_not_follow_later_source_edits() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("frozen.json");
        std::fs::write(&path, json!({"mcpServers":{"known":{}}}).to_string()).unwrap();
        let names = frozen_names(Some(&path));
        std::fs::write(&path, json!({"mcpServers":{"new":{}}}).to_string()).unwrap();
        observe_init(home.path(), "employee", &names, &init_event(vec![
            json!({"name":"known","status":"connected"}), json!({"name":"new","status":"failed"})]));
        assert!(last(home.path(), "employee", "known").unwrap().ok);
        assert!(last(home.path(), "employee", "new").is_none());
        assert!(!home.path().join("channel_failures.jsonl").exists());
    }

    #[test]
    fn preparation_refusal_never_repairs_employee_config() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/alice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        for raw in ["{}", "not-json", r#"{"mcpServers":{"forged":{"command":"evil","env":{"DUDUCLAW_AGENT_ID":"victim"}}}}"#] {
            std::fs::write(&path, raw).unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert!(matches!(prepare(home.path(), "alice", "missing"), Err(ReasonCode::PolicyDenied)));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
    }

    // Reexec isolates DUDUCLAW_BIN from all other tests. The shim is the
    // stdio peer fixture, not a substitute for proxy identity-refusal tests.
    #[cfg(unix)]
    #[test]
    fn rpc_probe_outcomes_leave_employee_bytes_mtime_and_directory_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        const TEST: &str = "mcp_start::tests::rpc_probe_outcomes_leave_employee_bytes_mtime_and_directory_unchanged";
        if std::env::var_os("DDC_PROBE_TEST_CHILD").is_none() {
            let temp = tempfile::tempdir().unwrap();
            let shim = temp.path().join("proxy-fixture");
            let exe = std::env::current_exe().unwrap().to_string_lossy().replace('\'', "'\"'\"'");
            std::fs::write(&shim, format!("#!/bin/sh\nexec '{exe}' --exact mcp_start::tests::fixture --ignored --nocapture\n")).unwrap();
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"]).env("DDC_PROBE_TEST_CHILD", "1")
                .env("DUDUCLAW_BIN", &shim).env("TMPDIR", temp.path()).status().unwrap().success());
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let home = tempfile::tempdir().unwrap();
            let dir = home.path().join("agents/alice");
            let handler = crate::handlers::MethodHandler::new(home.path().into()).await;
            std::fs::create_dir_all(&dir).unwrap();
            for (mode, expected) in [("success", None), ("failure", Some(ReasonCode::IdentityRejected)),
                ("timeout", Some(ReasonCode::Timeout))] {
                let path = dir.join(".mcp.json");
                // The persisted file intentionally lacks the built-in slot;
                // a normal spawn would repair it. A probe must not do so.
                let raw = json!({"mcpServers":{"fixture":{"command":"fixture", "args":[],
                    "env":{"MCP_FIXTURE_MODE":mode}}}}).to_string().into_bytes();
                std::fs::write(&path, &raw).unwrap();
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                let (entry, temp) = prepare(home.path(), "alice", "fixture").unwrap();
                let temp_path = temp.to_path_buf();
                assert_eq!(std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777, 0o600);
                assert!(entry["command"].is_string());
                drop(temp);
                assert!(!temp_path.exists());
                let temp_configs = || -> HashSet<PathBuf> {
                    std::fs::read_dir(std::env::temp_dir()).unwrap().filter_map(|entry| {
                        let path = entry.ok()?.path();
                        path.file_name()?.to_str()?.strip_prefix("duduclaw-mcp-")?;
                        Some(path)
                    }).collect()
                };
                let before_temp = temp_configs();
                let frame = Box::pin(handler.handle("mcp.probe", json!({"agent_id":"alice", "server_name":"fixture"}),
                    &duduclaw_auth::UserContext::admin_fallback())).await;
                let crate::protocol::WsFrame::Response { ok: true, payload: Some(payload), .. } = frame else {
                    panic!("probe RPC failed: {frame:?}");
                };
                assert_eq!(temp_configs(), before_temp, "{mode} leaked a probe config");
                assert_eq!(payload.as_object().unwrap().len(), 3);
                assert!(payload.get("time").is_none());
                let status: ProbeResult = serde_json::from_value(payload).unwrap();
                assert_eq!(status.reason_code, expected, "{mode}");
                assert_eq!(status.ok, mode == "success");
                if status.ok { assert_eq!(status.tool_count, Some(1)); }
                assert_eq!(std::fs::read(&path).unwrap(), raw, "{mode}");
                assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified, "{mode}");
                assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "{mode}");
            }
            let permits = PROBES.acquire_many(4).await.unwrap();
            let frame = Box::pin(handler.handle("mcp.probe", json!({"agent_id":"alice","server_name":"fixture"}),
                &duduclaw_auth::UserContext::admin_fallback())).await;
            let crate::protocol::WsFrame::Response { ok: true, payload: Some(payload), .. } = frame else { panic!("busy RPC"); };
            assert_eq!(payload.as_object().unwrap().len(), 3);
            assert!(payload.get("time").is_none());
            assert_eq!(payload["reason_code"], "busy");
            // A busy install probe must not retain the old start result.
            remember(home.path(), "alice", "missing", result_status(Ok(99)));
            handler.handle_mcp_update(&json!({"agent_id":"alice","action":"edit", "server_name":"missing",
                "server_def":{"command":"/nonexistent/mcp-fixture","args":[],"env":{}}})).await;
            assert!(last(home.path(), "alice", "missing").is_none());
            drop(permits);
            remember(home.path(), "alice", "missing", result_status(Ok(99)));
            handler.handle_mcp_update(&json!({"agent_id":"alice","action":"remove","server_name":"missing"})).await;
            assert!(last(home.path(), "alice", "missing").is_none());
        });
    }

    #[test]
    #[ignore = "Rust stdio child for probe tests"]
    fn fixture() {
        // Separate libtest progress text from protocol frames.
        println!();
        use std::io::{BufRead, Write};
        let mode = std::env::var("MCP_FIXTURE_MODE").ok().or_else(|| {
            let raw = std::env::var(crate::redaction_proxy::PROXY_UPSTREAM_ENV_VAR).ok()?;
            serde_json::from_str::<Value>(&raw).ok()?["MCP_FIXTURE_MODE"].as_str().map(str::to_owned)
        }).unwrap_or_default();
        if mode == "timeout" { std::thread::sleep(std::time::Duration::from_secs(30)); return; }
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break; };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue; };
            if v.get("id").is_none() { continue; }
            let frame = if mode == "failure" {
                json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32001,
                    "message":"fixed","data":{"reason_code":"identity_rejected"}}})
            } else {
                let result = if v["method"] == "initialize" {
                    json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                } else { json!({"tools":[{"name":"echo_text"}]}) };
                json!({"jsonrpc":"2.0","id":v["id"],"result":result})
            };
            println!("{frame}");
            std::io::stdout().flush().unwrap();
        }
    }

    #[tokio::test]
    async fn install_probe_success_failure_and_timeout() {
        let home = tempfile::tempdir().unwrap();
        for (mode, expected) in [("success", Ok(1)), ("failure", Err(ReasonCode::IdentityRejected)),
            ("timeout", Err(ReasonCode::Timeout))] {
            let entry = json!({"command":std::env::current_exe().unwrap(),
                "args":["--exact","mcp_start::tests::fixture","--ignored","--nocapture"],
                "env":{"MCP_FIXTURE_MODE":mode,"HOME":home.path()}});
            let result = probe_entry(&entry, std::time::Duration::from_millis(500)).await;
            assert_eq!(result, expected, "{mode}");
        }
    }
}
